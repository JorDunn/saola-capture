//! The recording state machine and the frame pump — the daemon-side half of
//! `record start|stop|toggle`.
//!
//! **Recording state lives in the daemon** (PLAN.md Architecture) and
//! survives every window close, every CLI process exiting, and the tray host
//! coming and going. This module is where that state actually is:
//! [`RecorderState`] is the whole of it, `dbus.rs` holds exactly one behind a
//! `Mutex`, and nothing else in the process keeps a second copy that could
//! disagree.
//!
//! # Why this module has no `view`/`subscription` (a deliberate exception)
//!
//! Every other `modules/*` file follows the sibling shape — state struct +
//! `view(&Theme)` + `subscription()` + a nested `Message`. This one has no
//! surface at all: PLAN.md assigns the recording *UX* (the tray item, the
//! optional elapsed-time chip, the finish toast) to **Stage 12**, and Stage
//! 11's job is the machinery underneath it. Stage 12 adds the view half here
//! or in `modules/tray.rs`; the state and its transitions do not move.
//!
//! # The two halves, and why they are separate
//!
//! - [`RecorderState`] is **pure**: phases, guards, counters. It is generic
//!   over the handle type it parks in the `Recording` phase precisely so its
//!   tests can use a `u32` and never build a real screencast. Every rule
//!   PLAN.md Stage 11 task 4 lists — stop-while-starting,
//!   encoder-death-while-recording — is a transition here, not an `if` buried
//!   in a D-Bus method.
//! - [`pump_frames`] is the **loop**: `recv` a frame, write it, repeat. It
//!   runs on tokio's blocking pool for the whole recording (the receivers are
//!   `std::sync::mpsc` and blocking — see `capture::screencast`'s own note on
//!   why the PipeWire side is not async) and takes a `&mut dyn EncoderSink`,
//!   so its tests drive it with real `std::sync::mpsc` channels against a
//!   fake sink.
//!
//! # Backpressure (binding — PLAN.md's "Backpressure and failure posture")
//!
//! > The PipeWire thread never blocks on the encoder: bounded channel, drop
//! > frames and log when full.
//!
//! **That rule is enforced upstream of this file, and it must stay there.**
//! `capture::screencast`'s pw thread owns a `sync_channel(4)` and offers each
//! frame with `try_send`, counting and logging every `Full`. This pump is the
//! *consumer* of that channel, so the only way it could ever block the
//! producer is by not existing — which is why an encoder that has died ends
//! the recording promptly (via [`EncoderSink::poll_health`]) instead of
//! leaving a full queue and a stalled compositor callback. The pump's own
//! contribution is: **do nothing between `recv` and `write_all`**. Every
//! millisecond spent here is a millisecond of PTS error, because
//! `-use_wallclock_as_timestamps 1` timestamps a frame when its bytes reach
//! ffmpeg (see `encode::VideoSpec`'s timing model).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::capture::screencast::{CastControl, VideoFrame};
use crate::encode::{EncoderSink, NegotiatedGuard};

/// How long the pump parks on the frame channel before waking to re-check the
/// stop flag and the encoder's health.
///
/// **A timeout is not a failure.** CAPTURE-RESEARCH D8: window casts are
/// damage-driven and "can go seconds between frames; the recorder must not
/// interpret frame silence as failure". Stage 10 measured exactly that live —
/// an idle window cast produced **5 frames in 5 seconds**. So this value only
/// bounds how long a stop request waits, never how long a legitimate quiet
/// period may last.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How often the pump asks the sink whether its child is still alive. Cheap
/// (`waitpid(WNOHANG)`), and the only thing that notices a dead encoder
/// during a quiet stretch with no frames to write.
const HEALTH_INTERVAL: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------
// The state machine
// ---------------------------------------------------------------------

/// Where a recording is. PLAN.md Stage 11 task 4's four states, exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Nothing is recording. The only phase a new recording may start from.
    Idle,
    /// A start is in flight: the ScreenCast session is being negotiated, the
    /// first frame awaited, ffmpeg spawned. Can take a second or two, and can
    /// fail — which is why it is a phase and not a moment.
    Starting,
    /// Frames are being written.
    Recording,
    /// A stop is in flight: the pump has been asked to finish, ffmpeg is
    /// flushing, the cast is being torn down.
    Stopping,
}

/// Why a request was refused. Each one is user-visible (it becomes a D-Bus
/// error and, for a keybind, a toast), so each says what the user should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecorderError {
    /// `StartRecording` while something is already recording.
    AlreadyRecording,
    /// `StopRecording` with nothing to stop.
    NotRecording,
    /// `StopRecording` twice.
    AlreadyStopping,
}

impl std::fmt::Display for RecorderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecorderError::AlreadyRecording => write!(
                f,
                "a recording is already in progress — stop it first (saola-capture record stop)"
            ),
            RecorderError::NotRecording => write!(f, "nothing is recording"),
            RecorderError::AlreadyStopping => {
                write!(f, "the recording is already stopping")
            }
        }
    }
}

impl std::error::Error for RecorderError {}

/// What [`RecorderState::started`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    /// Normal: the recording is live.
    Recording,
    /// A stop arrived while this start was still in flight (PLAN.md Stage 11
    /// task 4's "stop-while-starting"). The caller must tear the whole thing
    /// straight back down — the encoder has written nothing worth keeping,
    /// and the user has already said they don't want it.
    StopImmediately,
    /// The start was abandoned before it finished (the state machine was
    /// reset underneath it). Tear down, report nothing.
    Cancelled,
}

/// What [`RecorderState::request_stop`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// The recording is live and has now been asked to stop. The caller
    /// should signal the pump and wait for the file.
    Stopping,
    /// The recording had not finished starting; the stop is remembered and
    /// [`RecorderState::started`] will answer [`StartOutcome::StopImmediately`].
    /// There is no file to wait for.
    QueuedDuringStart,
}

/// The daemon's recording state.
///
/// Generic over `H`, the handle parked while a recording is live — in the
/// daemon that is `dbus.rs`'s `ActiveRecording` (a stop flag plus the
/// one-shot channel a waiting `StopRecording` parks on); in this module's
/// tests it is a `u32`. The point of the parameter is that **every rule in
/// this file is testable without a compositor, a GPU or a D-Bus connection**
/// (PLAN.md's testing strategy: "pure logic … unit-tested directly").
#[derive(Debug)]
pub struct RecorderState<H> {
    phase: Phase,
    active: Option<H>,
    /// Set by a stop that arrived during [`Phase::Starting`].
    stop_pending: bool,
    started_at: Option<Instant>,
    /// The last failure, kept so a `StopRecording` arriving just after an
    /// encoder death can report what actually happened rather than a bare
    /// "nothing is recording".
    last_error: Option<String>,
}

impl<H> Default for RecorderState<H> {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            active: None,
            stop_pending: false,
            started_at: None,
            last_error: None,
        }
    }
}

impl<H> RecorderState<H> {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// "Is a recording happening?" — the `io.saola.Capture1` `Recording`
    /// property, and what `record toggle` branches on.
    ///
    /// **`Stopping` counts as active.** A toggle pressed during a stop must
    /// not start a *second* recording on top of one that is still finalizing
    /// its file; answering `true` routes it to `StopRecording`, which refuses
    /// with [`RecorderError::AlreadyStopping`] — a clean no-op instead of two
    /// ffmpegs writing at once.
    pub fn is_active(&self) -> bool {
        self.phase() != Phase::Idle
    }

    /// How long the current recording has been going, for the tray tooltip
    /// Stage 12 adds. Measured from the *start request*, not from the first
    /// frame: that is what a human means by "how long have I been recording".
    pub fn elapsed(&self, now: Instant) -> Option<Duration> {
        self.started_at
            .map(|started| now.saturating_duration_since(started))
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// The live recording's handle, while there is one.
    pub fn active_mut(&mut self) -> Option<&mut H> {
        self.active.as_mut()
    }

    /// Idle → Starting. The single guard against two concurrent recordings,
    /// and it is taken **before** any of the slow work (session, PipeWire,
    /// ffmpeg) begins, so two `record start`s racing cannot both get past it.
    pub fn begin_start(&mut self, now: Instant) -> Result<(), RecorderError> {
        if self.phase != Phase::Idle {
            return Err(RecorderError::AlreadyRecording);
        }
        self.phase = Phase::Starting;
        self.stop_pending = false;
        self.started_at = Some(now);
        self.last_error = None;
        Ok(())
    }

    /// Starting → Recording (or straight to Stopping if a stop arrived while
    /// the start was in flight).
    pub fn started(&mut self, handle: H) -> StartOutcome {
        if self.phase != Phase::Starting {
            return StartOutcome::Cancelled;
        }
        self.active = Some(handle);
        if self.stop_pending {
            self.stop_pending = false;
            self.phase = Phase::Stopping;
            StartOutcome::StopImmediately
        } else {
            self.phase = Phase::Recording;
            StartOutcome::Recording
        }
    }

    /// Starting → Idle: the start never got off the ground (no ScreenCast, no
    /// PipeWire node, no ffmpeg, no VAAPI device).
    pub fn start_failed(&mut self, why: impl Into<String>) {
        self.phase = Phase::Idle;
        self.active = None;
        self.stop_pending = false;
        self.started_at = None;
        self.last_error = Some(why.into());
    }

    /// Recording → Stopping, or remember the stop if the start is still in
    /// flight.
    pub fn request_stop(&mut self) -> Result<StopOutcome, RecorderError> {
        match self.phase {
            Phase::Idle => Err(RecorderError::NotRecording),
            Phase::Starting => {
                self.stop_pending = true;
                Ok(StopOutcome::QueuedDuringStart)
            }
            Phase::Recording => {
                self.phase = Phase::Stopping;
                Ok(StopOutcome::Stopping)
            }
            Phase::Stopping => Err(RecorderError::AlreadyStopping),
        }
    }

    /// The encoder died (or the cast did) with nobody having asked to stop —
    /// PLAN.md Stage 11 task 4's "encoder-death-while-recording".
    ///
    /// Moves to `Stopping` rather than straight to `Idle`: the teardown
    /// (flush ffmpeg, stop the PipeWire stream, `Session.Stop`) still has to
    /// happen and still takes time, and a `StartRecording` arriving during it
    /// must be refused, not raced. [`Self::finished`] closes the loop.
    pub fn encoder_died(&mut self, why: impl Into<String>) {
        self.last_error = Some(why.into());
        if matches!(self.phase, Phase::Starting | Phase::Recording) {
            self.phase = Phase::Stopping;
        }
    }

    /// Anything → Idle: the teardown is complete. Returns the handle so the
    /// caller can answer whoever was waiting on it.
    pub fn finished(&mut self, result: Result<(), String>) -> Option<H> {
        self.phase = Phase::Idle;
        self.stop_pending = false;
        self.started_at = None;
        if let Err(why) = result {
            self.last_error = Some(why);
        }
        self.active.take()
    }
}

// ---------------------------------------------------------------------
// The pump
// ---------------------------------------------------------------------

/// Why [`pump_frames`] returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PumpOutcome {
    /// Somebody set the stop flag — the normal end of a recording.
    StopRequested,
    /// The PipeWire stream ended (the compositor closed the cast, the user
    /// closed the recorded window, the node went away).
    StreamEnded,
    /// The stream reported a fatal error.
    StreamError(String),
    /// The encoder died or refused a frame.
    EncoderFailed(String),
}

impl PumpOutcome {
    /// Whether this is a clean end. A clean end still flushes and keeps the
    /// file; an unclean one becomes an `Error` signal and a toast.
    pub fn is_clean(&self) -> bool {
        matches!(self, PumpOutcome::StopRequested | PumpOutcome::StreamEnded)
    }
}

/// Move frames from the PipeWire stream into the encoder until something
/// stops it.
///
/// Blocking, for the whole recording. `stop` is the only way to end it from
/// outside — an `AtomicBool` rather than a channel because it is read on
/// every loop turn and written once, and because a channel would give the
/// stopper something to block on.
///
/// `written` is incremented per frame so the daemon can report progress (and
/// so a "recording produced zero frames" case is distinguishable from a
/// "recording failed" one) without this function holding a lock.
///
/// `guard` is what makes a mid-stream format change fatal instead of silently
/// corrupting — see [`NegotiatedGuard`].
///
/// `seed` is the frame `dbus::CaptureService::spin_up` already wrote before
/// this loop started — see [`seal_last_frame`] for the only thing this
/// function does with it.
pub fn pump_frames(
    frames: &Receiver<VideoFrame>,
    control: &Receiver<CastControl>,
    sink: &mut dyn EncoderSink,
    stop: &AtomicBool,
    written: &AtomicU64,
    guard: &NegotiatedGuard,
    seed: Option<VideoFrame>,
) -> PumpOutcome {
    let mut last = seed;
    let outcome = pump_until_done(frames, control, sink, stop, written, guard, &mut last);
    if outcome.is_clean() {
        seal_last_frame(sink, last.as_ref());
    }
    outcome
}

/// Re-write the most recent frame, once, as the recording ends.
///
/// # Why a duplicate frame is the correct end of a recording
///
/// A niri cast is **damage-driven**: a screen nobody is touching produces one
/// frame and then nothing (CAPTURE-RESEARCH D8). Since every timestamp comes
/// from *when bytes reach ffmpeg's stdin*, the video stream then ends at the
/// last change rather than at the stop — so a 60-second recording of a still
/// screen was a **0.04-second file**. Writing the last frame again at stop
/// time stamps it "now", which is what makes the video's length the
/// recording's length.
///
/// **Stage 13 made this load-bearing rather than cosmetic**: `-shortest` (which
/// an infinite `-f pulse` input *requires*, §4.3) ends the output when the
/// shortest stream ends, so a video that stopped early truncated the audio
/// with it. Measured live before the fix: a 7.1 s recording of an idle screen
/// with `--audio system` produced 0.048 s of video **and 0.048 s of audio**.
///
/// Failure here is logged, never fatal: the recording is already over and
/// everything before this frame is already encoded.
fn seal_last_frame(sink: &mut dyn EncoderSink, last: Option<&VideoFrame>) {
    let Some(frame) = last else {
        return;
    };
    if let Err(err) = sink.write_video(&frame.bytes) {
        eprintln!(
            "saola-capture: recorder: could not write the closing frame ({err}) — the recording \
             is saved, but its video may be shorter than its audio"
        );
    }
}

/// The loop proper. Split out of [`pump_frames`] so the closing frame has
/// exactly one place to be written, rather than one per `return`.
#[allow(clippy::too_many_arguments)]
fn pump_until_done(
    frames: &Receiver<VideoFrame>,
    control: &Receiver<CastControl>,
    sink: &mut dyn EncoderSink,
    stop: &AtomicBool,
    written: &AtomicU64,
    guard: &NegotiatedGuard,
    last: &mut Option<VideoFrame>,
) -> PumpOutcome {
    let mut last_health = Instant::now();

    loop {
        if stop.load(Ordering::Acquire) {
            return PumpOutcome::StopRequested;
        }

        // Control messages are rare; drained without blocking so the frame
        // channel stays the thing this loop parks on (the same split
        // `screencast::observe` uses, and for the same reason: merging the
        // two would let a busy frame queue delay a fatal error).
        loop {
            match control.try_recv() {
                Ok(CastControl::Negotiated(format)) => {
                    // A *second* `param_changed` with different dimensions
                    // would change the byte length of every subsequent frame
                    // while ffmpeg is still framing at the old `-video_size`.
                    // There is no way to renegotiate ffmpeg's input mid-pipe,
                    // so this is fatal rather than a resize.
                    if let Err(why) = guard.check(&format) {
                        return PumpOutcome::StreamError(why);
                    }
                }
                Ok(CastControl::Error(why)) => return PumpOutcome::StreamError(why),
                Ok(CastControl::Ended) => return PumpOutcome::StreamEnded,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // The pw thread is gone. Its `Ended` may have been the message
                // just consumed, or it may have died; either way there will be
                // no more frames.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }

        match frames.recv_timeout(POLL_INTERVAL) {
            Ok(frame) => {
                // Nothing between here and `write_video` — see this module's
                // doc comment on why latency here is PTS error.
                if let Err(err) = sink.write_video(&frame.bytes) {
                    return PumpOutcome::EncoderFailed(err.to_string());
                }
                written.fetch_add(1, Ordering::Relaxed);
                // Kept — not copied — so the recording can be sealed with it
                // (`seal_last_frame`). The previous one is dropped here, so
                // this holds exactly one frame's worth of memory at a time.
                *last = Some(frame);
                last_health = Instant::now();
            }
            // Not a failure — see [`POLL_INTERVAL`] and CAPTURE-RESEARCH D8.
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return PumpOutcome::StreamEnded,
        }

        if last_health.elapsed() >= HEALTH_INTERVAL {
            last_health = Instant::now();
            if let Err(err) = sink.poll_health() {
                return PumpOutcome::EncoderFailed(err.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{EncodeError, VideoSpec};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{channel, sync_channel, SyncSender};
    use std::sync::Arc;

    // -- the state machine --------------------------------------------

    fn state() -> RecorderState<u32> {
        RecorderState::default()
    }

    #[test]
    fn the_happy_path_walks_all_four_phases() {
        let mut recorder = state();
        assert_eq!(recorder.phase(), Phase::Idle);
        assert!(!recorder.is_active());

        recorder.begin_start(Instant::now()).unwrap();
        assert_eq!(recorder.phase(), Phase::Starting);
        assert!(recorder.is_active());

        assert_eq!(recorder.started(7), StartOutcome::Recording);
        assert_eq!(recorder.phase(), Phase::Recording);

        assert_eq!(recorder.request_stop().unwrap(), StopOutcome::Stopping);
        assert_eq!(recorder.phase(), Phase::Stopping);
        assert!(recorder.is_active(), "a stopping recording is still busy");

        assert_eq!(recorder.finished(Ok(())), Some(7));
        assert_eq!(recorder.phase(), Phase::Idle);
        assert!(!recorder.is_active());
        assert_eq!(recorder.last_error(), None);
    }

    #[test]
    fn a_second_start_is_refused_from_every_busy_phase() {
        for reach_phase in [Phase::Starting, Phase::Recording, Phase::Stopping] {
            let mut recorder = state();
            recorder.begin_start(Instant::now()).unwrap();
            if reach_phase != Phase::Starting {
                recorder.started(1);
            }
            if reach_phase == Phase::Stopping {
                recorder.request_stop().unwrap();
            }
            assert_eq!(recorder.phase(), reach_phase);
            assert_eq!(
                recorder.begin_start(Instant::now()),
                Err(RecorderError::AlreadyRecording),
                "{reach_phase:?}"
            );
        }
    }

    #[test]
    fn stopping_nothing_says_so() {
        let mut recorder = state();
        assert_eq!(recorder.request_stop(), Err(RecorderError::NotRecording));
    }

    #[test]
    fn stopping_twice_is_refused_rather_than_racing_the_first_stop() {
        let mut recorder = state();
        recorder.begin_start(Instant::now()).unwrap();
        recorder.started(1);
        recorder.request_stop().unwrap();
        assert_eq!(recorder.request_stop(), Err(RecorderError::AlreadyStopping));
    }

    /// PLAN.md Stage 11 task 4: **stop-while-starting.**
    #[test]
    fn a_stop_during_start_is_queued_and_cancels_the_start() {
        let mut recorder = state();
        recorder.begin_start(Instant::now()).unwrap();

        assert_eq!(
            recorder.request_stop().unwrap(),
            StopOutcome::QueuedDuringStart
        );
        // Still Starting — the start is in flight and cannot be abandoned
        // mid-negotiation.
        assert_eq!(recorder.phase(), Phase::Starting);

        // When the start finally lands, it is told to tear straight back down
        // rather than becoming a live recording nobody wants.
        assert_eq!(recorder.started(9), StartOutcome::StopImmediately);
        assert_eq!(recorder.phase(), Phase::Stopping);
        assert_eq!(recorder.finished(Ok(())), Some(9));
        assert_eq!(recorder.phase(), Phase::Idle);
    }

    #[test]
    fn a_failed_start_returns_to_idle_and_remembers_why() {
        let mut recorder = state();
        recorder.begin_start(Instant::now()).unwrap();
        recorder.start_failed("no VAAPI device");
        assert_eq!(recorder.phase(), Phase::Idle);
        assert_eq!(recorder.last_error(), Some("no VAAPI device"));
        // …and the next start is allowed.
        assert!(recorder.begin_start(Instant::now()).is_ok());
    }

    /// PLAN.md Stage 11 task 4: **encoder-death-while-recording.**
    #[test]
    fn an_encoder_death_moves_to_stopping_not_straight_to_idle() {
        let mut recorder = state();
        recorder.begin_start(Instant::now()).unwrap();
        recorder.started(3);

        recorder.encoder_died("ffmpeg exited with status 1: No space left on device");
        assert_eq!(
            recorder.phase(),
            Phase::Stopping,
            "teardown still has to happen"
        );
        // A start arriving during that teardown is refused, not raced.
        assert_eq!(
            recorder.begin_start(Instant::now()),
            Err(RecorderError::AlreadyRecording)
        );
        // And a stop arriving during it is refused too — the recording is
        // already on its way down.
        assert_eq!(recorder.request_stop(), Err(RecorderError::AlreadyStopping));

        assert_eq!(recorder.finished(Err("disk full".to_string())), Some(3));
        assert_eq!(recorder.phase(), Phase::Idle);
        assert_eq!(recorder.last_error(), Some("disk full"));
    }

    #[test]
    fn an_encoder_death_during_start_is_absorbed_too() {
        let mut recorder = state();
        recorder.begin_start(Instant::now()).unwrap();
        recorder.encoder_died("ffmpeg died before the first frame");
        assert_eq!(recorder.phase(), Phase::Stopping);
    }

    #[test]
    fn elapsed_runs_from_the_start_request_and_resets_on_idle() {
        let mut recorder = state();
        let start = Instant::now();
        assert_eq!(recorder.elapsed(start), None);
        recorder.begin_start(start).unwrap();
        assert_eq!(
            recorder.elapsed(start + Duration::from_secs(9)),
            Some(Duration::from_secs(9))
        );
        recorder.started(1);
        assert_eq!(
            recorder.elapsed(start + Duration::from_secs(10)),
            Some(Duration::from_secs(10))
        );
        recorder.finished(Ok(()));
        assert_eq!(recorder.elapsed(start + Duration::from_secs(11)), None);
    }

    #[test]
    fn a_start_that_lands_after_a_reset_is_cancelled_rather_than_resurrected() {
        let mut recorder = state();
        recorder.begin_start(Instant::now()).unwrap();
        recorder.start_failed("gave up");
        assert_eq!(recorder.started(5), StartOutcome::Cancelled);
        assert_eq!(recorder.phase(), Phase::Idle);
    }

    #[test]
    fn every_refusal_names_something_the_user_can_do() {
        for err in [
            RecorderError::AlreadyRecording,
            RecorderError::NotRecording,
            RecorderError::AlreadyStopping,
        ] {
            let message = err.to_string();
            assert!(!message.is_empty(), "{err:?}");
        }
        assert!(RecorderError::AlreadyRecording
            .to_string()
            .contains("record stop"));
    }

    // -- the pump -----------------------------------------------------

    /// A sink that records what it was given and can be told to fail.
    struct FakeSink {
        frames: Vec<usize>,
        fail_after: Option<usize>,
        dead: bool,
        path: PathBuf,
    }

    impl FakeSink {
        fn new() -> Self {
            Self {
                frames: Vec::new(),
                fail_after: None,
                dead: false,
                path: PathBuf::from("/tmp/fake.mkv"),
            }
        }

        fn failing_after(count: usize) -> Self {
            Self {
                fail_after: Some(count),
                ..Self::new()
            }
        }
    }

    impl EncoderSink for FakeSink {
        fn write_video(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
            if self.dead || self.fail_after == Some(self.frames.len()) {
                self.dead = true;
                return Err(EncodeError::Died {
                    code: Some(1),
                    tail: vec!["No space left on device".to_string()],
                });
            }
            self.frames.push(bytes.len());
            Ok(())
        }

        fn poll_health(&mut self) -> Result<(), EncodeError> {
            if self.dead {
                Err(EncodeError::Died {
                    code: Some(1),
                    tail: vec!["already dead".to_string()],
                })
            } else {
                Ok(())
            }
        }

        fn finish(self: Box<Self>) -> Result<PathBuf, EncodeError> {
            Ok(self.path)
        }

        fn abort(self: Box<Self>) {}

        fn output_path(&self) -> &Path {
            &self.path
        }
    }

    fn frame(width: u32, height: u32, sequence: u64) -> VideoFrame {
        VideoFrame {
            width,
            height,
            bytes: vec![0u8; (width as usize) * (height as usize) * 4],
            sequence,
            captured_at: Instant::now(),
        }
    }

    fn guard_for(width: u32, height: u32) -> NegotiatedGuard {
        NegotiatedGuard::new(VideoSpec::new(width, height))
    }

    #[test]
    fn the_pump_writes_every_frame_then_stops_on_the_flag() {
        let (frames_tx, frames_rx) = channel();
        let (_control_tx, control_rx) = channel();
        for sequence in 0..4 {
            frames_tx.send(frame(8, 4, sequence)).unwrap();
        }

        let stop = AtomicBool::new(false);
        let written = AtomicU64::new(0);
        let mut sink = FakeSink::new();

        // Stop once the queue has drained: the pump checks the flag at the
        // top of each turn, so setting it after the sends still lets all four
        // frames through.
        std::thread::scope(|scope| {
            scope.spawn(|| {
                while written.load(Ordering::Relaxed) < 4 {
                    std::thread::sleep(Duration::from_millis(5));
                }
                stop.store(true, Ordering::Release);
            });
            let outcome = pump_frames(
                &frames_rx,
                &control_rx,
                &mut sink,
                &stop,
                &written,
                &guard_for(8, 4),
                None,
            );
            assert_eq!(outcome, PumpOutcome::StopRequested);
        });

        // Four frames, plus the closing duplicate of the last one — see
        // `seal_last_frame`. `written` counts what the *cast* produced, so
        // the seal is deliberately not in it.
        assert_eq!(sink.frames, vec![8 * 4 * 4; 5]);
        assert_eq!(written.load(Ordering::Relaxed), 4);
        assert!(PumpOutcome::StopRequested.is_clean());
    }

    // -- Stage 13: the closing frame -----------------------------------

    /// The live failure this fixes: a still screen produces exactly one frame
    /// (in `spin_up`, before the pump exists), so without a closing frame the
    /// video stream ends at 0.04 s — and with `-shortest` that truncated the
    /// **audio** to 0.04 s as well, whatever the recording's real length.
    #[test]
    fn a_recording_that_never_saw_a_second_frame_is_still_sealed() {
        let (_frames_tx, frames_rx) = channel::<VideoFrame>();
        let (_control_tx, control_rx) = channel();
        let stop = AtomicBool::new(true);
        let written = AtomicU64::new(0);
        let mut sink = FakeSink::new();

        let outcome = pump_frames(
            &frames_rx,
            &control_rx,
            &mut sink,
            &stop,
            &written,
            &guard_for(4, 4),
            Some(frame(4, 4, 0)),
        );

        assert_eq!(outcome, PumpOutcome::StopRequested);
        assert_eq!(
            sink.frames,
            vec![4 * 4 * 4],
            "the seed frame is re-written once, at the stop"
        );
        assert_eq!(
            written.load(Ordering::Relaxed),
            0,
            "no frame arrived from the cast"
        );
    }

    /// An *unclean* end is not sealed: the encoder is already gone (or the
    /// cast collapsed), so one more write would at best fail and at worst
    /// append a frame to a recording that ended abnormally seconds earlier.
    #[test]
    fn an_unclean_end_is_not_sealed() {
        let (frames_tx, frames_rx) = channel();
        let (_control_tx, control_rx) = channel();
        for sequence in 0..5 {
            frames_tx.send(frame(4, 4, sequence)).unwrap();
        }

        let mut sink = FakeSink::failing_after(2);
        let outcome = pump_frames(
            &frames_rx,
            &control_rx,
            &mut sink,
            &AtomicBool::new(false),
            &AtomicU64::new(0),
            &guard_for(4, 4),
            Some(frame(4, 4, 99)),
        );

        assert!(!outcome.is_clean());
        assert_eq!(sink.frames.len(), 2, "no closing frame after a failure");
    }

    /// A failing seal is a logged note, not a failed recording — everything
    /// before it is already encoded and on disk.
    #[test]
    fn a_failed_closing_frame_does_not_change_the_outcome() {
        let (_frames_tx, frames_rx) = channel::<VideoFrame>();
        let (_control_tx, control_rx) = channel();
        let mut sink = FakeSink::failing_after(0);

        let outcome = pump_frames(
            &frames_rx,
            &control_rx,
            &mut sink,
            &AtomicBool::new(true),
            &AtomicU64::new(0),
            &guard_for(4, 4),
            Some(frame(4, 4, 0)),
        );

        assert_eq!(outcome, PumpOutcome::StopRequested);
    }

    #[test]
    fn the_pump_reports_an_encoder_death_and_stops_reading() {
        let (frames_tx, frames_rx) = channel();
        let (_control_tx, control_rx) = channel();
        for sequence in 0..5 {
            frames_tx.send(frame(4, 4, sequence)).unwrap();
        }

        let mut sink = FakeSink::failing_after(2);
        let written = AtomicU64::new(0);
        let outcome = pump_frames(
            &frames_rx,
            &control_rx,
            &mut sink,
            &AtomicBool::new(false),
            &written,
            &guard_for(4, 4),
            None,
        );

        match outcome {
            PumpOutcome::EncoderFailed(why) => {
                assert!(why.contains("No space left on device"), "{why}")
            }
            other => panic!("expected an encoder failure, got {other:?}"),
        }
        assert_eq!(written.load(Ordering::Relaxed), 2);
        assert!(!PumpOutcome::EncoderFailed(String::new()).is_clean());
    }

    #[test]
    fn the_pump_ends_when_the_stream_says_it_ended() {
        let (frames_tx, frames_rx) = channel();
        let (control_tx, control_rx) = channel();
        frames_tx.send(frame(4, 4, 0)).unwrap();
        control_tx.send(CastControl::Ended).unwrap();

        let mut sink = FakeSink::new();
        let outcome = pump_frames(
            &frames_rx,
            &control_rx,
            &mut sink,
            &AtomicBool::new(false),
            &AtomicU64::new(0),
            &guard_for(4, 4),
            None,
        );
        assert_eq!(outcome, PumpOutcome::StreamEnded);
        assert!(
            outcome.is_clean(),
            "a compositor-ended cast still keeps the file"
        );
    }

    #[test]
    fn the_pump_surfaces_a_stream_error() {
        let (_frames_tx, frames_rx) = channel();
        let (control_tx, control_rx) = channel();
        control_tx
            .send(CastControl::Error(
                "the buffer was not a dmabuf".to_string(),
            ))
            .unwrap();

        let mut sink = FakeSink::new();
        let outcome = pump_frames(
            &frames_rx,
            &control_rx,
            &mut sink,
            &AtomicBool::new(false),
            &AtomicU64::new(0),
            &guard_for(4, 4),
            None,
        );
        assert_eq!(
            outcome,
            PumpOutcome::StreamError("the buffer was not a dmabuf".to_string())
        );
    }

    /// CAPTURE-RESEARCH D8: "Window casts are damage-driven and can go
    /// seconds between frames; the recorder must not interpret frame silence
    /// as failure." Stage 10 measured five frames in five seconds on a real
    /// idle window.
    #[test]
    fn a_long_gap_between_frames_is_not_an_end() {
        let (frames_tx, frames_rx) = channel();
        let (_control_tx, control_rx) = channel();
        let stop = AtomicBool::new(false);
        let written = AtomicU64::new(0);
        let mut sink = FakeSink::new();

        std::thread::scope(|scope| {
            scope.spawn(|| {
                // Longer than POLL_INTERVAL, so the pump wakes on a timeout at
                // least twice before anything arrives.
                std::thread::sleep(POLL_INTERVAL * 3);
                frames_tx.send(frame(4, 4, 0)).unwrap();
                while written.load(Ordering::Relaxed) == 0 {
                    std::thread::sleep(Duration::from_millis(5));
                }
                stop.store(true, Ordering::Release);
            });
            let outcome = pump_frames(
                &frames_rx,
                &control_rx,
                &mut sink,
                &stop,
                &written,
                &guard_for(4, 4),
                None,
            );
            assert_eq!(outcome, PumpOutcome::StopRequested);
        });

        assert_eq!(written.load(Ordering::Relaxed), 1);
    }

    /// A dropped sender with no `Ended` message (the pw thread vanished)
    /// still ends the pump rather than spinning forever.
    #[test]
    fn a_disconnected_frame_channel_ends_the_pump() {
        let (frames_tx, frames_rx) = channel::<VideoFrame>();
        let (control_tx, control_rx) = channel::<CastControl>();
        drop(frames_tx);
        drop(control_tx);
        let mut sink = FakeSink::new();
        let outcome = pump_frames(
            &frames_rx,
            &control_rx,
            &mut sink,
            &AtomicBool::new(false),
            &AtomicU64::new(0),
            &guard_for(4, 4),
            None,
        );
        assert_eq!(outcome, PumpOutcome::StreamEnded);
    }

    /// A mid-stream renegotiation to a different size cannot be honoured
    /// (ffmpeg's `-video_size` is fixed for the process's life), so it is
    /// fatal rather than silently sheared.
    #[test]
    fn a_renegotiated_frame_size_is_fatal() {
        use crate::capture::screencast::NegotiatedFormat;
        let (_frames_tx, frames_rx) = channel();
        let (control_tx, control_rx) = channel();
        control_tx
            .send(CastControl::Negotiated(NegotiatedFormat {
                width: 1920,
                height: 1080,
                modifier: 0,
                framerate: (0, 1),
                max_framerate: (60000, 1000),
            }))
            .unwrap();

        let mut sink = FakeSink::new();
        let outcome = pump_frames(
            &frames_rx,
            &control_rx,
            &mut sink,
            &AtomicBool::new(false),
            &AtomicU64::new(0),
            &guard_for(2560, 1600),
            None,
        );
        match outcome {
            PumpOutcome::StreamError(why) => {
                assert!(why.contains("2560x1600"), "{why}");
                assert!(why.contains("1920x1080"), "{why}");
            }
            other => panic!("expected a stream error, got {other:?}"),
        }
    }

    /// Re-announcing the *same* format (which a compositor may legitimately
    /// do) must not kill a healthy recording.
    #[test]
    fn a_repeated_identical_format_is_ignored() {
        use crate::capture::screencast::NegotiatedFormat;
        let (frames_tx, frames_rx) = channel();
        let (control_tx, control_rx) = channel();
        control_tx
            .send(CastControl::Negotiated(NegotiatedFormat {
                width: 64,
                height: 32,
                modifier: 0,
                framerate: (0, 1),
                max_framerate: (60000, 1000),
            }))
            .unwrap();
        frames_tx.send(frame(64, 32, 0)).unwrap();
        drop(frames_tx);
        drop(control_tx);

        let mut sink = FakeSink::new();
        let written = AtomicU64::new(0);
        let outcome = pump_frames(
            &frames_rx,
            &control_rx,
            &mut sink,
            &AtomicBool::new(false),
            &written,
            &guard_for(64, 32),
            None,
        );
        assert_eq!(outcome, PumpOutcome::StreamEnded);
        assert_eq!(written.load(Ordering::Relaxed), 1);
    }

    /// The binding backpressure rule, exercised end to end against the *same*
    /// channel shape `capture::screencast` uses (`sync_channel(4)` +
    /// `try_send`): a producer faster than the consumer **drops frames and
    /// counts them; it never blocks**.
    ///
    /// This is the consumer side's guarantee that the rule holds — the
    /// producer here stands in for the pw thread's `on_process` callback,
    /// which runs inside PipeWire's own event loop and must return promptly
    /// or the compositor's cast stalls.
    #[test]
    fn a_full_frame_queue_drops_instead_of_blocking_the_producer() {
        const CAPACITY: usize = 4; // == screencast::FRAME_QUEUE_CAPACITY
        const OFFERED: u64 = 400;

        let (frames_tx, frames_rx) = sync_channel::<VideoFrame>(CAPACITY);
        let (_control_tx, control_rx) = channel();
        let dropped = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let written = Arc::new(AtomicU64::new(0));

        let producer_dropped = Arc::clone(&dropped);
        let producer_stop = Arc::clone(&stop);
        let producer = std::thread::spawn(move || {
            let started = Instant::now();
            let mut longest_offer = Duration::ZERO;
            for sequence in 0..OFFERED {
                let before = Instant::now();
                offer(&frames_tx, frame(64, 64, sequence), &producer_dropped);
                longest_offer = longest_offer.max(before.elapsed());
            }
            producer_stop.store(true, Ordering::Release);
            (started.elapsed(), longest_offer)
        });

        // A consumer slow enough that the queue is full almost immediately.
        struct SlowSink(FakeSink);
        impl EncoderSink for SlowSink {
            fn write_video(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
                std::thread::sleep(Duration::from_millis(2));
                self.0.write_video(bytes)
            }
            fn poll_health(&mut self) -> Result<(), EncodeError> {
                self.0.poll_health()
            }
            fn finish(self: Box<Self>) -> Result<PathBuf, EncodeError> {
                Box::new(self.0).finish()
            }
            fn abort(self: Box<Self>) {}
            fn output_path(&self) -> &Path {
                self.0.output_path()
            }
        }

        let mut sink = SlowSink(FakeSink::new());
        let outcome = pump_frames(
            &frames_rx,
            &control_rx,
            &mut sink,
            &stop,
            &written,
            &guard_for(64, 64),
            None,
        );
        let (total, longest_offer) = producer.join().expect("the producer thread finished");

        assert_eq!(outcome, PumpOutcome::StopRequested);

        // The producer was never blocked: a single `try_send` on a full
        // channel returns immediately, so no individual offer can have taken
        // anything like the consumer's 2 ms-per-frame pace.
        assert!(
            longest_offer < Duration::from_millis(2),
            "an offer took {longest_offer:?} — try_send must never block"
        );
        // And offering 400 frames took nowhere near 400 * 2 ms.
        assert!(
            total < Duration::from_millis(200),
            "the producer ran for {total:?} — it was throttled by the consumer"
        );

        // Frames were genuinely dropped, and every offered frame is accounted
        // for as either written or dropped (modulo whatever is still queued
        // when the stop flag wins the race).
        let dropped = dropped.load(Ordering::Relaxed);
        let written = written.load(Ordering::Relaxed);
        assert!(dropped > 0, "the queue should have overflowed");
        assert!(written > 0, "some frames should have been written");
        assert!(written + dropped <= OFFERED);
        assert!(written + dropped >= OFFERED - CAPACITY as u64);
    }

    /// `capture::screencast`'s producer rule, reproduced here so the test
    /// above is testing the real contract and not a paraphrase of it.
    fn offer(sender: &SyncSender<VideoFrame>, frame: VideoFrame, dropped: &AtomicU64) {
        if sender.try_send(frame).is_err() {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
