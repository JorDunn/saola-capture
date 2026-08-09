//! `io.saola.Capture1` — the D-Bus seam between the CLI, the window
//! process, keybinds, the panel tray, and the daemon (PLAN.md Architecture:
//! "D-Bus is the seam between CLI, window process, keybinds, panel tray,
//! and daemon").
//!
//! # What Stage 3 builds, and what it doesn't
//!
//! The interface shape — every method and signal name and signature — is
//! final; it is the contract a future **saola-notifications** consumes
//! (CLAUDE.md Boundaries: "the `io.saola.Capture1` signals ... are the
//! stable contract that component will consume"). What is *not* final yet
//! is the behavior behind each method: `capture/screencopy.rs` (Stage 5),
//! `capture/screencast.rs` (Stage 10) and `modules/picker.rs` (Stage 16)
//! didn't exist yet, so every method here logged the call and returned a
//! clean D-Bus error naming the stage that would implement it.
//! `Screenshot` (Stage 5) and `OpenWindow` (Stage 9) are real now;
//! `StartRecording`/`StopRecording` stay stubs through Stage 10 — that
//! stage built the *capture* half (`capture/screencast.rs`) but not the
//! encoder, and `record start --dry-run` exercises it without coming
//! through this file at all. This is the same
//! "stub, never `todo!()`" discipline `main.rs`'s Stage 1 body used —
//! CLAUDE.md's no-panic rule applies to a served D-Bus method exactly as it
//! does to a CLI verb: a caller (a keybind, the window process) that gets a
//! clean `Error` reply can show a toast; one that gets a hung connection or
//! a crashed daemon cannot.
//!
//! # Serving vs. proxying (teaching note)
//!
//! Two independent halves live in this file, mirroring
//! `saola-panel::modules::tray::watcher`'s split: [`CaptureService`] (an
//! `impl` block under `#[zbus::interface]`) is what the **daemon** exports
//! — state the bus pokes, dispatched by zbus's `ObjectServer`.
//! [`Capture1Proxy`] (a trait under `#[zbus::proxy]`) is what the **CLI**
//! and the **window process** use to call it — a generated client with one
//! method per served method. They describe the same wire contract from
//! opposite ends and cannot be the same Rust item (a proxy is generated
//! from a trait; an interface from an inherent impl), which is why the
//! interface's method names and the proxy's look identical but are two
//! independent declarations that must be kept in sync by hand (there is no
//! shared source of truth besides this file — an argument-count or
//! signature drift between them would silently fail at the D-Bus layer,
//! not at compile time, since the wire types are the only thing that has
//! to agree).
//!
//! Unlike the tray watcher, this service never has to *consume* somebody
//! else's implementation of the same interface — `io.saola.Capture1` has
//! exactly one legitimate owner, this daemon, so [`serve`]'s only two
//! outcomes are "we're it" and "somebody already is" (see [`ServeOutcome`]
//! and CLAUDE.md's single-instance rule), never a fallback to consuming a
//! rival.
//!
//! # Auto-spawn (the CLI's half)
//!
//! PLAN.md Stage 3: "The CLI auto-spawns the daemon detached and retries
//! once when the name is unowned." [`ensure_daemon_running`] is that logic:
//! ask the bus daemon itself (`org.freedesktop.DBus`'s `NameHasOwner`)
//! whether `io.saola.Capture1` is owned; if not, spawn `saola-capture
//! daemon` detached ([`spawn_daemon_detached`]) and poll briefly for
//! ownership before giving up. This runs once, up front, in every CLI verb
//! that needs the daemon (`shot` without `--no-daemon`, `record`,
//! `pick-color`, `open`) — see `main.rs`'s `run_via_daemon`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;
use zbus::Connection;

// Brings `CaptureBackend`'s trait methods (`.focused_window()`,
// `.capture_window()`) into scope for the `ScreencopyBackend` values built
// inline in `CaptureService::interactive_region` — Stage 8's window-picking
// path. `capture_and_save` above only ever calls the free function
// `capture::take_screenshot(&backend, ..)`, so this import wasn't needed
// until this stage added a direct trait-method call site.
use crate::capture::CaptureBackend;

/// The well-known bus name this daemon claims and the object path it lives
/// at. The interface name (used in the `#[zbus::interface]`/`#[zbus::proxy]`
/// attributes below, which need a string literal rather than a `const`) is
/// the same string again — the reverse-DNS-plus-version convention
/// PLAN.md's Architecture section already spells the interface with.
pub const SERVICE_NAME: &str = "io.saola.Capture1";
pub const OBJECT_PATH: &str = "/io/saola/Capture1";

/// How long [`ensure_daemon_running`] waits, in total, for a freshly
/// spawned daemon to claim the bus name before giving up. Polled in
/// [`SPAWN_POLL_INTERVAL`] steps rather than slept once, so a fast-starting
/// daemon (the common case — no Wayland roundtrip is needed just to serve
/// D-Bus) is usable almost immediately instead of always paying the full
/// budget.
const SPAWN_WAIT_BUDGET: Duration = Duration::from_secs(3);
const SPAWN_POLL_INTERVAL: Duration = Duration::from_millis(150);

/// The daemon-side interface implementation. Stage 5 gave it no fields ("a
/// handle to the capture engine, the recording state machine, … will land
/// here without changing the interface declaration below it" — Stage 5's
/// own words). Stage 6 is the first of those: [`Self::events`] is the
/// bridge to the iced daemon's own surfaces — see [`DaemonEvent`].
struct CaptureService {
    /// Notifies `main.rs`'s iced `Daemon` (flash + toast) after a
    /// successful screenshot. A `try_send` (never `.send().await`) at the
    /// call site — see [`DaemonEvent`]'s doc comment for why this method
    /// must never block on it.
    events: iced::futures::channel::mpsc::Sender<DaemonEvent>,
    /// **Stage 11.** The recording state machine —
    /// [`crate::modules::recorder::RecorderState`] — and the one place in the
    /// process that holds it. PLAN.md Architecture: "Recording state lives in
    /// the daemon and survives window closes."
    ///
    /// # Why a `std::sync::Mutex` in an async file (teaching note)
    ///
    /// The obvious objection to a blocking mutex inside `async fn`s is that a
    /// guard held across an `.await` blocks the executor thread — and worse,
    /// `MutexGuard` is not `Send`, so a future holding one across an await
    /// point does not even compile as a zbus method. That last part is the
    /// point: **the compiler enforces the rule for us.** Every use below takes
    /// the lock inside a `{ }` block that ends before any `.await`, and the
    /// slow parts of a recording (negotiating the cast, spawning ffmpeg,
    /// flushing at the end) all happen with the lock *released* while the
    /// state machine sits in `Starting`/`Stopping` — which is exactly what
    /// those two phases are for. A `tokio::sync::Mutex` would also work and
    /// would additionally need `tokio`'s `sync` feature, which this crate
    /// deliberately does not enable (see `Cargo.toml`'s tokio essay).
    recorder: SharedRecorder,
}

/// The handle parked in [`crate::modules::recorder::RecorderState`] while a
/// recording is live: how to stop it, and who to tell when it has stopped.
struct ActiveRecording {
    /// Set to `true` to ask the pump to finish. An `AtomicBool` rather than a
    /// channel because the pump reads it on every loop turn and nothing ever
    /// needs to block on writing it.
    stop: Arc<AtomicBool>,
    /// The `StopRecording` call waiting for the saved path, if there is one.
    ///
    /// **Registered under the same lock that sets `stop`**, which is what
    /// removes the obvious race: the supervisor task can only *take* the
    /// waiter by acquiring the same mutex, so it cannot finish-and-find-nobody
    /// in between a stopper's two writes. `None` means the recording ended
    /// without anybody asking — an encoder death, a compositor-closed cast —
    /// and the failure goes to the `Error` signal and a toast instead.
    waiter: Option<iced::futures::channel::oneshot::Sender<Result<String, String>>>,
}

type SharedRecorder = Arc<Mutex<crate::modules::recorder::RecorderState<ActiveRecording>>>;

/// Take the recorder lock, recovering from poisoning rather than propagating
/// it.
///
/// A poisoned mutex means some thread panicked while holding it. Nothing in
/// this file can (CLAUDE.md's no-panic rule), but the rule has no "cannot
/// happen" exemption, and the alternative — every call site `unwrap()`ing —
/// would turn one impossible panic into a *guaranteed* daemon death. The
/// state behind the lock is a small enum plus two counters; the worst a
/// recovered guard can observe is a half-applied transition, which the phase
/// guards below already reject.
fn lock_recorder(
    recorder: &SharedRecorder,
) -> std::sync::MutexGuard<'_, crate::modules::recorder::RecorderState<ActiveRecording>> {
    recorder
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How long a starting recording waits for its first frame before giving up.
///
/// Stage 10 measured a real monitor cast's first frame at **123–286 ms**, so
/// five seconds is two orders of magnitude of headroom for the case this
/// bounds: a cast that negotiates and then never produces anything. It is
/// deliberately *not* a general "the cast looks quiet" timeout — once
/// recording, silence is legal and expected (CAPTURE-RESEARCH D8), and the
/// pump has no such deadline at all.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// Everything one live recording owns, handed from the start sequence to the
/// pump and its supervisor.
struct StartedRecording {
    session: crate::capture::screencast::CastSession,
    stream: crate::capture::screencast::PipeWireStream,
    sink: Box<dyn crate::encode::EncoderSink>,
    guard: crate::encode::NegotiatedGuard,
}

/// An event crossing from this zbus-hosted service into `main.rs`'s iced
/// `Daemon::update` (as `Message::CaptureTaken`) — the wiring Stage 6 adds
/// ("Wire the PrintScr flow end-to-end: `shot --fullscreen` via daemon now
/// flashes, toasts, ...").
///
/// **This is a purely in-process bridge type, not part of the
/// `io.saola.Capture1` wire contract.** The actual D-Bus signal
/// (`capture_taken`, below) is unchanged from Stage 5 and is what an
/// external client (a future saola-notifications, `busctl --user monitor`)
/// still sees. `DaemonEvent` exists because the flash/toast surfaces live
/// in a *different* async task (iced's own executor, driven from
/// `main.rs::dbus_worker_stream`) than the one dispatching this method call
/// (zbus's `ObjectServer`), and carries a decoded thumbnail rather than a
/// path so the toast can show one without a second file read + decode — the
/// `Frame` is still in hand at the point this is built (see
/// [`CaptureService::screenshot`]), which is exactly the trade
/// `saola-lockscreen::wallpaper`'s decode-before-iced precedent makes.
///
/// Sent over `iced::futures::channel::mpsc` (already in the dependency tree
/// via `iced`'s own `futures` re-export — see `main.rs`'s `dbus_worker_stream`)
/// rather than adding `tokio`'s `sync` feature for a `tokio::sync::mpsc`
/// that would do the same job: zero net new crates/features, matching this
/// crate's habitual bar for a new dependency.
pub enum DaemonEvent {
    CaptureTaken {
        path: String,
        thumbnail: iced::widget::image::Handle,
    },
    /// **Stage 7.** An interactive `--region` shot needs the user to drag a
    /// rectangle: the frozen full-output capture is already in hand (see
    /// [`CaptureService::interactive_region`]) and this asks the iced daemon
    /// to map `modules::overlay` on top of it.
    ///
    /// Unlike [`Self::CaptureTaken`] this one expects an **answer**, which
    /// is what `reply` carries. It is a one-message
    /// `iced::futures::channel::mpsc` channel rather than a `oneshot`
    /// specifically because `main.rs`'s `Message` must derive `Clone`
    /// (`#[to_layer_message(multi)]` requires it) and `oneshot::Sender` is
    /// not `Clone` — an `mpsc::Sender` is, and a capacity-1 channel used
    /// once is a oneshot in every way that matters here.
    BeginRegion {
        /// The frozen frame, already decoded for iced. A **copy** of the
        /// `Frame`'s bytes: the `Frame` itself stays behind in
        /// [`CaptureService::interactive_region`], because it — not the
        /// handle — is what the confirmed rectangle is finally cropped out
        /// of. One extra full-output allocation per region shot (~16 MB at
        /// 2560×1600) buys the guarantee that the saved pixels are the ones
        /// the compositor handed us, never something round-tripped through
        /// a widget toolkit.
        frame: iced::widget::image::Handle,
        /// Which output the frame came from — its logical origin and size
        /// (the overlay's coordinate space), its name (for
        /// `OutputOption::OutputName`) and its scale (for the size readout).
        output: crate::capture::OutputInfo,
        /// **Stage 8.** Whichever window was focused at the moment the
        /// frame above was frozen, resolved once here (a best-effort extra
        /// niri-ipc round trip — see [`CaptureService::interactive_region`])
        /// and carried straight into `modules::overlay::Overlay::new` so the
        /// toolbar's Window button and a bare `shot --window`'s own
        /// no-`--window-id` default resolve "which window?" identically.
        focused_window: Option<crate::capture::WindowRef>,
        reply: iced::futures::channel::mpsc::Sender<RegionOutcome>,
    },
    /// **Stage 8.** A delayed shot (`--delay N`, any of the three kinds) is
    /// about to start counting down — maps `modules::countdown`'s pill so
    /// the countdown is visible before the shutter, not just felt as a
    /// pause. Fire-and-forget, like [`Self::CaptureTaken`]: nothing waits on
    /// an answer, so this is offered with `try_send` from
    /// [`CaptureService::screenshot`], never `.send().await`, for the same
    /// reason every other daemon-side bridge in this file is.
    CountdownStarted { seconds: u32 },
    /// **Stage 11.** A recording ended badly — the encoder died (a full disk
    /// is the realistic case), the cast collapsed, or ffmpeg refused the
    /// stream. PLAN.md Stage 11 task 2: "disk-full and mid-stream-death
    /// surfaced as `Error` signal + toast". The `Error` signal is emitted on
    /// the bus by the same supervisor that sends this; this half is the
    /// toast, because the person who pressed a keybind is not watching a
    /// terminal and, unlike every other failure in this file, there is no
    /// pending method call left to return an error to.
    ///
    /// **There is deliberately no `RecordingFinished` counterpart here.** The
    /// success toast — with the click-to-open-the-containing-directory
    /// behaviour videos need — is PLAN.md Stage 12's task 3.
    RecordingFailed { message: String },
}

/// How an interactive region selection ended — the value the daemon sends
/// back down [`DaemonEvent::BeginRegion`]'s `reply` channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionOutcome {
    /// Confirmed. Crop this **desktop-logical** rectangle out of the frozen
    /// frame (`capture::crop_frozen_frame`).
    Selected(crate::capture::LogicalRect),
    /// **Stage 8.** The toolbar's Window button: capture this window fresh
    /// via `CaptureBackend::capture_window` instead of cropping anything out
    /// of the frozen frame — see
    /// [`CaptureService::interactive_region`]'s step 4 for why this is a
    /// genuinely different tail than [`Self::Selected`], not a variant on
    /// it.
    SelectedWindow(crate::capture::WindowRef),
    /// Escape, or the toolbar's Cancel. Nothing is saved and the D-Bus call
    /// fails with a message saying so — the same shape `slurp` has always
    /// had, and the only honest answer for a method whose return value is a
    /// saved path.
    Cancelled,
    /// The daemon could not run a selection at all — today, only because one
    /// is already in progress. Carries the message the caller should see.
    Unavailable(&'static str),
}

/// A method that isn't implemented yet: log to stderr (so `daemon`'s own
/// terminal, or its systemd journal once Stage 17 wires autostart, shows
/// what was asked for) and hand back a clean `zbus::fdo::Error` naming the
/// stage that lands it. Centralized here so all five stub bodies stay
/// one-line calls instead of five copies of the same two statements —
/// Stage 5/10/16 delete the corresponding call site (not this function) as
/// each method grows a real implementation.
fn not_yet_implemented(method: &str, stage: &str) -> zbus::fdo::Error {
    eprintln!("saola-capture: daemon: {method} called — not implemented yet ({stage})");
    zbus::fdo::Error::NotSupported(format!("{method} is not implemented yet ({stage})"))
}

/// The daemon's half of the `shot` pipeline — the exact two library calls
/// `main.rs`'s `--no-daemon` branch makes, in the same order, differing only
/// in who ends up owning the clipboard selection.
///
/// Returns the [`Frame`](crate::capture::Frame) alongside the
/// [`SavedCapture`](crate::storage::SavedCapture) as of Stage 6 (Stage 5
/// discarded it once `save_capture` returned) — [`CaptureService::screenshot`]
/// needs it, still in memory, to build the toast's thumbnail without a
/// second file read + decode.
///
/// Blocking; always called from [`run_blocking`].
fn capture_and_save(
    options: &crate::cli::CaptureOptions,
) -> Result<(crate::storage::SavedCapture, crate::capture::Frame), String> {
    let backend = crate::capture::screencopy::ScreencopyBackend::new();
    let frame =
        crate::capture::take_screenshot(&backend, options).map_err(|err| err.to_string())?;
    let saved = crate::storage::save_capture(
        &frame,
        options,
        options.kind,
        // The daemon outlives the copy, so it can serve the selection
        // itself rather than spawning a helper — see `storage`'s module
        // doc comment on why a Wayland "copy" needs somebody to stay alive.
        crate::storage::ClipboardOwner::ThisProcess,
    )
    .map_err(|err| err.to_string())?;
    Ok((saved, frame))
}

/// Runs a blocking closure without stalling the daemon's executor.
///
/// See [`CaptureService::screenshot`]'s doc comment for the reasoning; this
/// is factored out because Stage 8's window capture and Stage 16's colour
/// picker will want the same treatment.
async fn run_blocking<T, F>(work: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(_) => match tokio::task::spawn_blocking(work).await {
            Ok(result) => result,
            // The blocking task panicked or was cancelled. Nothing in this
            // crate's own capture path panics, but a dependency could, and
            // the daemon reporting it beats the daemon dying with it.
            Err(err) => Err(format!("the capture task did not finish: {err}")),
        },
        // No tokio runtime (not reachable from the daemon, whose iced
        // executor is tokio-backed — but `spawn_blocking` panics rather
        // than erroring in that case, so it is guarded rather than assumed).
        Err(_) => work(),
    }
}

/// Wait for the negotiated format **and** the first frame, or give up.
///
/// Blocking (both receivers are `std::sync::mpsc`); always called through
/// [`run_blocking`].
///
/// # Why the first frame is waited for at all (CAPTURE-RESEARCH §4.4)
///
/// Two independent reasons, and the second is the one that outlives Stage 11:
///
/// 1. ffmpeg's `-video_size` must match the negotiated size, so it cannot be
///    spawned before negotiation. That is forced.
/// 2. Mitigation 1 of D7: **spawn ffmpeg only after the first video frame**,
///    so neither input idles at the start. ffmpeg's clock begins when ffmpeg
///    begins, and §4.3 measured that whatever gap exists between "ffmpeg
///    opened the audio device" and "the first video frame arrived" becomes
///    silent A/V desync of exactly that size, which `-copyts` does not
///    rescue. Waiting here collapses that gap to ffmpeg's own startup.
///
/// The stream is always handed back, success or failure, so the caller can
/// tear down in the binding order (consumer first, then `Session.Stop` — the
/// Stage 10 handoff §4).
fn await_first_frame(
    stream: crate::capture::screencast::PipeWireStream,
    timeout: Duration,
) -> (
    crate::capture::screencast::PipeWireStream,
    Result<
        (
            crate::capture::screencast::NegotiatedFormat,
            crate::capture::screencast::VideoFrame,
        ),
        String,
    >,
) {
    use crate::capture::screencast::CastControl;
    use std::sync::mpsc::RecvTimeoutError;

    let deadline = Instant::now() + timeout;
    let mut format = None;
    let mut pending = None;

    loop {
        // Control first, non-blocking — the same split the pump and Stage
        // 10's `observe` use.
        loop {
            match stream.control().try_recv() {
                Ok(CastControl::Negotiated(negotiated)) => format = Some(negotiated),
                Ok(CastControl::Error(why)) => return (stream, Err(why)),
                Ok(CastControl::Ended) => {
                    return (
                        stream,
                        Err("the screencast ended before it produced a frame".to_string()),
                    )
                }
                Err(_) => break,
            }
        }

        if let (Some(format), Some(frame)) = (format, pending.take()) {
            return (stream, Ok((format, frame)));
        }

        let now = Instant::now();
        if now >= deadline {
            return (
                stream,
                Err(format!(
                    "the screencast produced no frame within {} s — the compositor accepted the \
                     session but never sent anything (try `niri msg casts` to see whether a cast \
                     is stuck)",
                    timeout.as_secs()
                )),
            );
        }

        match stream.frames().recv_timeout(deadline - now) {
            Ok(frame) => pending = Some(frame),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return (
                    stream,
                    Err("the PipeWire stream thread stopped before the first frame".to_string()),
                )
            }
        }
    }
}

/// Summarize how a recording ended into the single `Result` everything
/// downstream (the waiting `StopRecording`, the signals, the toast) branches
/// on.
///
/// The interesting case is the third: an *unclean* end whose file still
/// finished flushing. The recording failed, so it must be reported as a
/// failure — but the bytes on disk are real and Matroska tolerates exactly
/// this, so the message names the file rather than pretending nothing was
/// saved.
fn recording_result(
    outcome: &crate::modules::recorder::PumpOutcome,
    finished: Result<PathBuf, String>,
) -> Result<String, String> {
    use crate::modules::recorder::PumpOutcome;

    let reason = match outcome {
        PumpOutcome::StopRequested => None,
        PumpOutcome::StreamEnded => None,
        PumpOutcome::StreamError(why) => Some(format!("the screencast failed: {why}")),
        PumpOutcome::EncoderFailed(why) => Some(format!("the encoder failed: {why}")),
    };

    match (reason, finished) {
        (None, Ok(path)) => Ok(path.display().to_string()),
        (None, Err(why)) => Err(why),
        (Some(reason), Ok(path)) => Err(format!(
            "{reason} — what had been encoded was saved to {}",
            path.display()
        )),
        // The flush almost always fails for the *same* reason the pump did —
        // the child is already gone, so `finish` re-reports the death it was
        // told about. Concatenating then doubles the sentence in the toast and
        // in the `Error` signal, which Stage 11's live encoder-death run
        // produced verbatim. Only genuinely new information is appended.
        (Some(reason), Err(why)) if reason.contains(&why) => Err(reason),
        (Some(reason), Err(why)) => Err(format!("{reason} ({why})")),
    }
}

/// The interactive half of `Screenshot`, kept in a **plain** `impl` block —
/// anything inside the `#[zbus::interface]` block below would be exported on
/// the bus, and this is an internal helper, not a method.
impl CaptureService {
    /// `Screenshot("region", …)` with no `geometry`: the full round trip
    /// through Stage 7's selection overlay.
    ///
    /// Four steps, in an order Architecture and CAPTURE-RESEARCH §1.5 both
    /// fix and none of which is negotiable:
    ///
    /// 1. **Freeze.** Capture the whole focused output *before* any overlay
    ///    exists, because screencopy composites layer-shell surfaces — a
    ///    capture taken with the overlay up would contain the overlay.
    /// 2. **Ask.** Hand a copy of those pixels to the iced daemon
    ///    ([`DaemonEvent::BeginRegion`]), which maps the overlay surface.
    /// 3. **Wait.** Park on the reply channel for as long as the user takes.
    ///    This is the one D-Bus method in this interface that can legitimately
    ///    take minutes to answer; `zbus` dispatches each incoming call on its
    ///    own task (`spawn_tasks_for_methods`, on by default), so a pending
    ///    selection blocks nothing else on the bus, and no client-side
    ///    timeout applies (`zbus::Connection`'s `method_timeout` defaults to
    ///    `None`, so `shot --region` blocks until the user acts — exactly
    ///    like `slurp`).
    /// 4. **Crop and save.** Out of the frame from step 1, never a fresh
    ///    capture, through the same `save_capture` every other shot uses.
    ///
    /// **`try_send`, not `.send().await`,** at step 2 — same rule as
    /// [`Self::screenshot`]'s `CaptureTaken` bridge: a D-Bus method must
    /// never park on the iced event loop draining a channel. A full channel
    /// here means the daemon is wedged, and a clean error beats a hung
    /// keybind.
    async fn interactive_region(
        &self,
        options: crate::cli::CaptureOptions,
    ) -> Result<(crate::storage::SavedCapture, crate::capture::Frame), String> {
        use iced::futures::StreamExt;

        // 1. Freeze. `--delay` is honoured here, before the overlay maps, so
        //    the frozen frame shows the delayed state rather than delaying
        //    the *selection* — the countdown pill (`Self::screenshot`'s
        //    `CountdownStarted` send, before this method is even called)
        //    is what makes that wait visible as of Stage 8.
        let freeze_options = options.clone();
        let (frame, output) = run_blocking(move || {
            let backend = crate::capture::screencopy::ScreencopyBackend::new();
            crate::capture::freeze_focused_output(&backend, &freeze_options)
                .map_err(|err| err.to_string())
        })
        .await?;

        // 1b. Also resolve the focused window, best-effort (Stage 8) — see
        //     `DaemonEvent::BeginRegion::focused_window`'s doc comment. A
        //     failure here (or a `run_blocking` panic) degrades to "nothing
        //     focused" rather than failing the whole region flow: the
        //     Window toolbar button simply renders disabled, which is the
        //     same honest answer a real "nothing is focused" gives.
        let focused_window = run_blocking(|| {
            let backend = crate::capture::screencopy::ScreencopyBackend::new();
            Ok::<_, String>(backend.focused_window().ok().flatten())
        })
        .await
        .unwrap_or(None);

        // 2. Ask. See `DaemonEvent::BeginRegion`'s doc comment for why the
        //    handle is a copy and the `Frame` stays here.
        let handle = iced::widget::image::Handle::from_rgba(
            frame.width(),
            frame.height(),
            frame.pixels().to_vec(),
        );
        let (reply, mut replies) = iced::futures::channel::mpsc::channel::<RegionOutcome>(1);
        self.events
            .clone()
            .try_send(DaemonEvent::BeginRegion {
                frame: handle,
                output: output.clone(),
                focused_window,
                reply,
            })
            .map_err(|_| {
                "could not reach the daemon's overlay (its event loop is busy or gone)".to_string()
            })?;

        // 3. Wait. A dropped sender (the daemon exited mid-selection) reads
        //    as a cancel: nothing was selected, and nothing should be saved.
        let outcome = replies.next().await.unwrap_or(RegionOutcome::Cancelled);

        // 4. Crop-and-save, or capture-and-save — two different tails,
        //    because `SelectedWindow` (Stage 8) is not a variant on cropping
        //    the frozen frame, it is an entirely different capture
        //    mechanism (`CaptureBackend::capture_window`, CAPTURE-RESEARCH
        //    D3). The frozen frame this method spent step 1 producing is
        //    simply discarded on that branch.
        match outcome {
            RegionOutcome::Selected(region) => {
                run_blocking(move || {
                    let cropped = crate::capture::crop_frozen_frame(&frame, &output, region)
                        .map_err(|err| err.to_string())?;
                    let saved = crate::storage::save_capture(
                        &cropped,
                        &options,
                        options.kind,
                        crate::storage::ClipboardOwner::ThisProcess,
                    )
                    .map_err(|err| err.to_string())?;
                    Ok((saved, cropped))
                })
                .await
            }
            RegionOutcome::SelectedWindow(window) => {
                run_blocking(move || {
                    let backend = crate::capture::screencopy::ScreencopyBackend::new();
                    let frame = backend
                        .capture_window(window, options.cursor)
                        .map_err(|err| err.to_string())?;
                    let saved = crate::storage::save_capture(
                        &frame,
                        &options,
                        crate::cli::ShotKind::Window,
                        crate::storage::ClipboardOwner::ThisProcess,
                    )
                    .map_err(|err| err.to_string())?;
                    Ok((saved, frame))
                })
                .await
            }
            RegionOutcome::Cancelled => Err("the region selection was cancelled".to_string()),
            RegionOutcome::Unavailable(note) => Err(note.to_string()),
        }
    }

    /// Everything between "a `StartRecording` was accepted" and "frames are
    /// flowing into an encoder" — **Stage 11**.
    ///
    /// The order is not negotiable, and every step's failure has to undo the
    /// steps before it:
    ///
    /// 1. **ffmpeg first.** Cheapest check, and the one thing that should
    ///    never leave a screencast session open behind it (PLAN.md Stage 11
    ///    task 2: "missing-ffmpeg detected up front").
    /// 2. **Which monitor?** Resolved through the *same* `CaptureBackend` a
    ///    screenshot uses, so "which output does a bare `record start` mean?"
    ///    has one answer in this codebase rather than two that can drift —
    ///    the same reasoning `main.rs::run_record_dry_run` already applies.
    /// 3. **The cast**, then the PipeWire stream (Stage 10's
    ///    `CastSession::open` → `PipeWireStream::connect`).
    /// 4. **The first frame** — see [`await_first_frame`].
    /// 5. **The file name**, then ffmpeg, then that first frame written
    ///    immediately so nothing is dropped and ffmpeg's wallclock starts on
    ///    a real frame rather than on silence.
    ///
    /// Teardown on failure is **consumer first, producer second** (Stage 10
    /// handoff §4): `PipeWireStream::stop()` before `CastSession::close()`,
    /// always. Reversing them makes the node vanish under a live consumer and
    /// logs a `StreamState::Error` for what was a clean abort.
    async fn spin_up(
        &self,
        connection: &Connection,
        options: &crate::cli::RecordOptions,
    ) -> Result<StartedRecording, String> {
        use crate::capture::screencast::{CastSession, CastTarget, CursorMode, PipeWireStream};
        use crate::encode::{ffmpeg_cli, EncodePreset, NegotiatedGuard, RecordSpec, VideoSpec};

        // 1.
        ffmpeg_cli::ensure_ffmpeg_available().map_err(|err| err.to_string())?;

        // 2.
        let connector = run_blocking(|| {
            let backend = crate::capture::screencopy::ScreencopyBackend::new();
            backend
                .focused_output()
                .map(|output| output.name)
                .map_err(|err| err.to_string())
        })
        .await?;

        // 3.
        let target = CastTarget::Monitor { connector };
        let cursor = CursorMode::from_cursor_option(options.cursor);
        eprintln!(
            "saola-capture: daemon: recording {target} (cursor {cursor:?}, preset {})",
            options.preset
        );
        let session = CastSession::open(connection, &target, cursor)
            .await
            .map_err(|err| err.to_string())?;

        let stream = match PipeWireStream::connect(session.node_id()) {
            Ok(stream) => stream,
            Err(err) => {
                // Nothing to stop on the consumer side — `connect` failed —
                // so the session is all there is to close.
                session.close().await;
                return Err(err.to_string());
            }
        };

        // 4.
        let (stream, negotiated) =
            match run_blocking(move || Ok(await_first_frame(stream, FIRST_FRAME_TIMEOUT))).await {
                Ok((stream, result)) => (stream, result),
                // `run_blocking`'s own failure (the blocking task was
                // cancelled): the stream went with it, so only the session is
                // left to close.
                Err(err) => {
                    session.close().await;
                    return Err(err);
                }
            };
        let (format, first_frame) = match negotiated {
            Ok(pair) => pair,
            Err(why) => {
                stream.stop();
                session.close().await;
                return Err(why);
            }
        };
        eprintln!("saola-capture: daemon: recording {format}");

        // 5.
        let preset = EncodePreset::from_config(options.preset);
        let video = VideoSpec::from_negotiated(&format);
        let vaapi_device = options.vaapi_device.clone();

        let guard = NegotiatedGuard::new(video.clone());
        let output_dir = options.output_dir.clone();
        let sink = run_blocking(move || {
            let path =
                crate::storage::allocate_recording_path(output_dir.as_deref(), preset.extension())
                    .map_err(|err| err.to_string())?;
            let spec = RecordSpec {
                video,
                audio: None,
                preset,
                path,
            };
            let choice = ffmpeg_cli::choose_encoder(preset, vaapi_device.as_deref())
                .map_err(|err| err.to_string())?;
            let mut sink =
                ffmpeg_cli::FfmpegSink::start(&spec, choice).map_err(|err| err.to_string())?;
            // The frame that was waited for in step 4 is the recording's
            // first frame, not a probe — dropping it would both lose a frame
            // and start ffmpeg's wallclock on nothing.
            crate::encode::EncoderSink::write_video(&mut sink, &first_frame.bytes)
                .map_err(|err| err.to_string())?;
            Ok(Box::new(sink) as Box<dyn crate::encode::EncoderSink>)
        })
        .await;

        let sink = match sink {
            Ok(sink) => sink,
            Err(why) => {
                stream.stop();
                session.close().await;
                return Err(why);
            }
        };

        eprintln!(
            "saola-capture: daemon: recording to {}",
            sink.output_path().display()
        );

        Ok(StartedRecording {
            session,
            stream,
            sink,
            guard,
        })
    }

    /// Hand a live recording to the two tasks that own it for the rest of its
    /// life — **Stage 11**.
    ///
    /// # Why two tasks and not one
    ///
    /// The pump is *blocking* (`std::sync::mpsc` receivers, a `write_all`
    /// into a pipe, a `waitpid`), so it belongs on tokio's blocking pool —
    /// running it on the executor would park the daemon's whole event loop
    /// for the duration of the recording. But the teardown it must be
    /// followed by is *async*: `CastSession::close` is a D-Bus call. So the
    /// blocking half runs everything it can, in the binding order
    /// (`PipeWireStream::stop()` then `sink.finish()`), and the async half
    /// awaits it, closes the session, and does the reporting.
    ///
    /// # Why the supervisor exists at all
    ///
    /// Because a recording can end **without anyone having asked**: the
    /// encoder dies (a full disk), the compositor closes the cast, the
    /// recorded window disappears. There is no pending method call to return
    /// an error to in that case, so something has to be awake to notice —
    /// emit the `Error` signal, raise a toast, and put the state machine back
    /// to `Idle` so the next `record start` works. That is this task. A
    /// `StopRecording` that *is* waiting simply finds its answer delivered by
    /// the same code path (`ActiveRecording::waiter`), which is why there is
    /// exactly one finalization site rather than two that could disagree.
    fn spawn_recording_tasks(
        &self,
        runtime: tokio::runtime::Handle,
        connection: Connection,
        started: StartedRecording,
        stop: Arc<AtomicBool>,
    ) {
        use crate::modules::recorder::{pump_frames, PumpOutcome};

        let StartedRecording {
            session,
            stream,
            mut sink,
            guard,
        } = started;
        let recorder = Arc::clone(&self.recorder);
        let events = self.events.clone();
        let written = Arc::new(AtomicU64::new(0));
        let pump_written = Arc::clone(&written);

        let pump = runtime.spawn_blocking(move || {
            let outcome = pump_frames(
                stream.frames(),
                stream.control(),
                sink.as_mut(),
                &stop,
                &pump_written,
                &guard,
            );
            // Read the producer's drop counter *before* tearing the stream
            // down, while it still exists.
            let dropped = stream.dropped_frames();
            // Binding order, Stage 10 handoff §4: consumer first.
            stream.stop();
            let finished = sink.finish().map_err(|err| err.to_string());
            (outcome, finished, dropped)
        });

        runtime.spawn(async move {
            let (outcome, finished, dropped) = match pump.await {
                Ok(result) => result,
                // The blocking task panicked or was cancelled. Both the
                // stream and the sink were dropped with it, and both have
                // `Drop` impls that tear down properly (the pw loop is quit
                // and joined; ffmpeg is killed and reaped), so there is
                // nothing to clean up here beyond the session below.
                Err(err) => (
                    PumpOutcome::EncoderFailed(format!("the recording task did not finish: {err}")),
                    Err("the recording task did not finish".to_string()),
                    0,
                ),
            };

            // An end nobody asked for (the encoder died, the cast collapsed)
            // moves the machine into `Stopping` *before* the async teardown
            // below, so a `record start` arriving during those few hundred
            // milliseconds is refused rather than raced — see
            // `RecorderState::encoder_died`.
            if !outcome.is_clean() {
                lock_recorder(&recorder).encoder_died(format!("{outcome:?}"));
            }

            // …producer second.
            session.close().await;

            let result = recording_result(&outcome, finished);

            let waiter = {
                let mut state = lock_recorder(&recorder);
                // Read the elapsed time *before* `finished` clears it.
                let elapsed = state.elapsed(Instant::now()).unwrap_or_default();
                let frames = written.load(Ordering::Relaxed);
                eprintln!(
                    "saola-capture: daemon: recording ended after {:.1} s ({outcome:?}) — \
                     {frames} frame(s) encoded, {dropped} dropped",
                    elapsed.as_secs_f64()
                );
                state
                    .finished(result.as_ref().map(|_| ()).map_err(|why| why.clone()))
                    .and_then(|active| active.waiter)
            };

            match SignalEmitter::new(&connection, OBJECT_PATH) {
                Ok(emitter) => match &result {
                    Ok(path) => {
                        if let Err(err) = CaptureService::recording_finished(&emitter, path).await {
                            eprintln!(
                                "saola-capture: daemon: could not emit RecordingFinished: {err}"
                            );
                        }
                    }
                    Err(message) => {
                        if let Err(err) = CaptureService::error(&emitter, message).await {
                            eprintln!("saola-capture: daemon: could not emit Error: {err}");
                        }
                    }
                },
                Err(err) => {
                    eprintln!("saola-capture: daemon: could not build a signal emitter: {err}")
                }
            }

            if let Err(message) = &result {
                eprintln!("saola-capture: daemon: recording failed: {message}");
                if events
                    .clone()
                    .try_send(DaemonEvent::RecordingFailed {
                        message: message.clone(),
                    })
                    .is_err()
                {
                    eprintln!(
                        "saola-capture: daemon: could not raise a toast for the failed recording \
                         (channel full or the daemon's event loop is gone)"
                    );
                }
            }

            if let Some(waiter) = waiter {
                // A dropped receiver means the caller gave up (or the CLI
                // process exited); the file is saved either way.
                let _ = waiter.send(result);
            }
        });
    }
}

#[zbus::interface(name = "io.saola.Capture1")]
impl CaptureService {
    /// `Screenshot(kind s, options a{sv}) -> s`. `kind` is one of
    /// `"fullscreen"`/`"region"`/`"window"` (see `cli::ShotKind::as_str`);
    /// `options` carries the resolved `CaptureOptions` as CLI-flag-shaped
    /// key/value pairs (`cli::CaptureOptions::to_dbus_options`, decoded by
    /// its sibling `from_dbus_options`).
    ///
    /// **Real as of Stage 5** for `fullscreen` and for `region` with an
    /// explicit `geometry`, **as of Stage 7** for an interactive `region`
    /// too (no `geometry` — see [`Self::interactive_region`]), and **as of
    /// Stage 8** for `window` — with or without `window-id` (`capture::
    /// take_screenshot`'s own dispatch) and via the region overlay's
    /// Window button (also [`Self::interactive_region`], which is why that
    /// one method now branches on two capture mechanisms rather than one).
    ///
    /// # Two teaching notes on the body
    ///
    /// **`spawn_blocking`.** Everything below the `CaptureBackend` boundary
    /// blocks: Wayland roundtrips, a ~0.3 s compositor blit, a libwebp
    /// encode, a file write. Running that directly in this `async fn` would
    /// park the daemon's whole executor — no other D-Bus method, no iced
    /// subscription, nothing — for the duration. `spawn_blocking` moves it
    /// to tokio's blocking pool instead. `Handle::try_current()` guards the
    /// call because `spawn_blocking` *panics* outside a runtime, and
    /// CLAUDE.md's no-panic rule does not make an exception for "that can't
    /// happen"; if there is somehow no runtime, the work runs inline.
    ///
    /// **The signal.** `CaptureTaken` is emitted on success, before
    /// returning. It is the future saola-notifications contract (CLAUDE.md
    /// Boundaries), so it fires whether the capture came from a keybind, the
    /// window process or `busctl`. A failed emission is logged, not
    /// propagated: the screenshot is already on disk and the caller is owed
    /// its path.
    ///
    /// **Stage 6: the `events` bridge.** After the signal, a
    /// [`DaemonEvent::CaptureTaken`] is offered to [`Self::events`] via
    /// `try_send` — never `.send().await`. Blocking a D-Bus method reply on
    /// the iced daemon's own event loop draining a channel would be exactly
    /// the kind of cross-task deadlock risk CLAUDE.md's backpressure posture
    /// (the pipewire-thread rule: "never block ... drop and log when full")
    /// warns about generally; `try_send`'s failure path (channel full, or
    /// the receiving end gone) degrades to a logged warning, never a failed
    /// screenshot — the file is already on disk and the D-Bus caller is
    /// still owed its path either way.
    async fn screenshot(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        kind: String,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<String> {
        eprintln!(
            "saola-capture: daemon: Screenshot(kind={kind:?}, {} option(s))",
            options.len()
        );

        let options = crate::cli::CaptureOptions::from_dbus_options(&kind, &options)
            .map_err(|err| zbus::fdo::Error::InvalidArgs(err.to_string()))?;

        // **Stage 8.** Show the countdown pill *before* any of the blocking
        // work below starts — this is the one place all three shot kinds
        // (fullscreen, region — both interactive and `--geometry` — and
        // window) funnel through, so it is the one place this event needs
        // to be sent rather than three. `try_send`, never
        // `.send().await`, for the same reason every other bridge in this
        // file uses it: a full channel or a gone event loop degrades to "no
        // visible countdown, capture anyway" rather than blocking (or
        // failing) the reply. `--no-daemon` shots never reach this method at
        // all, so they stay silent during their delay exactly as before —
        // there is no surface to show a countdown *on*.
        if options.delay > 0
            && self
                .events
                .clone()
                .try_send(DaemonEvent::CountdownStarted {
                    seconds: options.delay,
                })
                .is_err()
        {
            eprintln!(
                "saola-capture: daemon: could not show the delay countdown (channel full or \
                 the daemon's event loop is gone) — capturing anyway"
            );
        }

        // **Stage 7.** A `region` shot with no explicit `geometry` is the
        // one kind that cannot be answered without asking the user, so it
        // takes the long way round: freeze the output, map the overlay, wait
        // for a rectangle, *then* crop and save. Everything else — a
        // fullscreen shot, or a region with `--geometry` — is still the
        // single blocking call Stage 5 shipped. Both branches end in the
        // same `(SavedCapture, Frame)`, so the signal/thumbnail/reply tail
        // below is shared rather than duplicated.
        let interactive_region =
            options.kind == crate::cli::ShotKind::Region && options.geometry.is_none();

        let outcome = if interactive_region {
            self.interactive_region(options).await
        } else {
            run_blocking(move || capture_and_save(&options)).await
        };

        let (saved, frame) = outcome.map_err(|err| {
            eprintln!("saola-capture: daemon: Screenshot failed: {err}");
            zbus::fdo::Error::Failed(err)
        })?;

        let path = saved.path.to_string_lossy().into_owned();
        if let Err(err) = Self::capture_taken(&emitter, &path, &kind).await {
            eprintln!("saola-capture: daemon: could not emit CaptureTaken: {err}");
        }

        // 128 px is generous headroom over the toast's 36 px icon tile
        // (`modules::toast::ICON_TILE_SIZE`) — enough that a HiDPI render
        // still looks sharp, small enough that the GPU upload stays cheap.
        let thumbnail = crate::modules::toast::thumbnail_handle(&frame, 128);
        if self
            .events
            .clone()
            .try_send(DaemonEvent::CaptureTaken {
                path: path.clone(),
                thumbnail,
            })
            .is_err()
        {
            eprintln!(
                "saola-capture: daemon: could not notify the flash/toast surfaces (channel \
                 full or the daemon's event loop is gone) — {path} is still saved"
            );
        }

        Ok(path)
    }

    /// `StartRecording(kind s, options a{sv})` — **real as of Stage 11**, for
    /// `kind == "fullscreen"`.
    ///
    /// Returns as soon as the recording is *live* (negotiated, encoding, first
    /// frame written), not when it finishes — unlike `Screenshot`, whose
    /// return value is the artefact. The artefact here comes back from
    /// [`Self::stop_recording`], or arrives as a `RecordingFinished` signal.
    ///
    /// # The shape of the body, and why it is three phases
    ///
    /// 1. **Claim the state machine** (`begin_start`) before any slow work.
    ///    That one guarded transition is the whole defence against two
    ///    concurrent recordings; taking it first means two racing `record
    ///    start`s cannot both get as far as spawning an ffmpeg.
    /// 2. **Do the slow work with the lock released** ([`Self::spin_up`]),
    ///    while the machine sits in `Starting` — which is what that phase is
    ///    for, and what makes stop-while-starting expressible at all.
    /// 3. **Hand over** to the pump and its supervisor, or unwind.
    ///
    /// `region`/`window` recording is Stage 12 (CAPTURE-RESEARCH D8: region is
    /// a monitor cast cropped in the filter chain, window is `RecordWindow`),
    /// and `cli::RecordOptions::from_dbus_options` rejects those kinds rather
    /// than quietly recording the whole monitor.
    async fn start_recording(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(connection)] connection: &Connection,
        kind: String,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        eprintln!(
            "saola-capture: daemon: StartRecording(kind={kind:?}, {} option(s))",
            options.len()
        );

        let options = crate::cli::RecordOptions::from_dbus_options(&kind, &options)
            .map_err(|err| zbus::fdo::Error::InvalidArgs(err.to_string()))?;

        // Refused rather than silently dropped: a script that asked to record
        // its microphone must learn it didn't, not discover it on playback.
        // (Same posture `cli.rs` takes for `--window-id` without `--dry-run`.)
        if options.audio.is_some() {
            return Err(zbus::fdo::Error::NotSupported(
                "audio recording lands in Stage 13 — start the recording without --audio"
                    .to_string(),
            ));
        }

        // Checked before anything is claimed: `spawn_blocking`/`spawn` need a
        // runtime, and (per the no-panic rule) that is asked rather than
        // assumed — the same `Handle::try_current()` guard `run_blocking`
        // uses, but fatal here, because a recording without somewhere to run
        // its pump cannot degrade to anything useful.
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            zbus::fdo::Error::Failed(
                "the daemon has no async runtime to run a recording on".to_string(),
            )
        })?;

        // 1.
        {
            let mut state = lock_recorder(&self.recorder);
            state
                .begin_start(Instant::now())
                .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))?;
        }

        // 2.
        let started = match self.spin_up(connection, &options).await {
            Ok(started) => started,
            Err(why) => {
                eprintln!("saola-capture: daemon: StartRecording failed: {why}");
                lock_recorder(&self.recorder).start_failed(why.clone());
                return Err(zbus::fdo::Error::Failed(why));
            }
        };

        // 3.
        let stop = Arc::new(AtomicBool::new(false));
        let outcome = {
            let mut state = lock_recorder(&self.recorder);
            state.started(ActiveRecording {
                stop: Arc::clone(&stop),
                waiter: None,
            })
        };

        use crate::modules::recorder::StartOutcome;
        match outcome {
            StartOutcome::Recording => {}
            // A `StopRecording` (or a reset) landed while this start was in
            // flight. Nothing worth keeping has been encoded — one frame —
            // so this unwinds inline rather than going through the pump.
            StartOutcome::StopImmediately | StartOutcome::Cancelled => {
                let StartedRecording {
                    session,
                    stream,
                    sink,
                    ..
                } = started;
                sink.abort();
                stream.stop();
                session.close().await;
                let message =
                    "the recording was stopped while it was still starting — nothing was saved"
                        .to_string();
                {
                    let mut state = lock_recorder(&self.recorder);
                    if let Some(active) = state.finished(Err(message.clone())) {
                        if let Some(waiter) = active.waiter {
                            let _ = waiter.send(Err(message.clone()));
                        }
                    }
                }
                return Err(zbus::fdo::Error::Failed(message));
            }
        }

        self.spawn_recording_tasks(runtime, connection.clone(), started, stop);

        if let Err(err) = Self::recording_started(&emitter, &kind).await {
            eprintln!("saola-capture: daemon: could not emit RecordingStarted: {err}");
        }
        Ok(())
    }

    /// `StopRecording() -> s` — the saved recording's path. **Real as of
    /// Stage 11.**
    ///
    /// Blocks until ffmpeg has flushed and the cast is torn down, which for a
    /// long MP4 (`+faststart` rewrites the whole file) can be seconds. That is
    /// the same "a method may legitimately take a while" contract
    /// `Screenshot`'s interactive region already established, and for the same
    /// reason it is safe: zbus dispatches each call on its own task, so
    /// nothing else on the bus waits with it.
    async fn stop_recording(&self) -> zbus::fdo::Result<String> {
        eprintln!("saola-capture: daemon: StopRecording()");

        use crate::modules::recorder::StopOutcome;

        // The lock is held across *both* writes — registering the waiter and
        // setting the stop flag — which is what stops the supervisor from
        // finishing in between and concluding nobody was listening. See
        // `ActiveRecording::waiter`.
        let waiting = {
            let mut state = lock_recorder(&self.recorder);
            match state.request_stop() {
                // "nothing is recording" is a much better answer when it can
                // also say *why* there is nothing recording — a recording that
                // died thirty seconds ago is the case where a user presses the
                // stop keybind and deserves the real reason, not a shrug.
                Err(err) => {
                    let message = match state.last_error() {
                        Some(last) => format!("{err} (the last recording ended: {last})"),
                        None => err.to_string(),
                    };
                    return Err(zbus::fdo::Error::Failed(message));
                }
                Ok(StopOutcome::QueuedDuringStart) => {
                    // Honest: the stop was accepted, but there is no path to
                    // return, because nothing was ever recorded. The start
                    // sequence will see `StopImmediately` and unwind.
                    return Err(zbus::fdo::Error::Failed(
                        "the recording had not finished starting — it has been cancelled, and \
                         nothing was saved"
                            .to_string(),
                    ));
                }
                Ok(StopOutcome::Stopping) => match state.active_mut() {
                    Some(active) => {
                        let (sender, receiver) = iced::futures::channel::oneshot::channel();
                        active.waiter = Some(sender);
                        active.stop.store(true, Ordering::Release);
                        receiver
                    }
                    // `Recording` with no handle is not reachable (the handle
                    // is stored by the same transition that enters the
                    // phase), but leaving the machine parked in `Stopping`
                    // with nothing to stop would wedge every later start — so
                    // it is reset rather than assumed away.
                    None => {
                        state.finished(Err("the recording had no live handle".to_string()));
                        return Err(zbus::fdo::Error::Failed(
                            "the recording had no live handle — the recorder has been reset"
                                .to_string(),
                        ));
                    }
                },
            }
        };

        match waiting.await {
            Ok(Ok(path)) => Ok(path),
            Ok(Err(why)) => Err(zbus::fdo::Error::Failed(why)),
            // The supervisor dropped the sender without answering — only
            // possible if the daemon is tearing down around us.
            Err(_) => Err(zbus::fdo::Error::Failed(
                "the recorder stopped without reporting a result".to_string(),
            )),
        }
    }

    /// `Recording b` — read-only, true while anything is starting, recording
    /// or stopping.
    ///
    /// **An additive extension to the Stage 3 interface, and the one this
    /// stage needed.** The frozen part of `io.saola.Capture1` is its signals
    /// (CLAUDE.md Boundaries: the future saola-notifications contract) and the
    /// five methods Architecture lists; a read-only property adds to that
    /// without changing any of it. It exists because `record toggle` cannot be
    /// implemented correctly without it — the alternatives were "always call
    /// `StartRecording` and let it fail", which loses the saved path on the
    /// stop half, or "guess from a `StopRecording` error string", which is a
    /// parser for prose. Stage 12's tray item wants exactly this property for
    /// its idle/recording icon states, and may want to emit
    /// `PropertiesChanged` on it (this stage does not: nothing subscribes
    /// yet, and an unnotified property is still correct, just polled).
    #[zbus(property)]
    fn recording(&self) -> bool {
        lock_recorder(&self.recorder).is_active()
    }

    /// `PickColor() -> (ddd)` — RGB in `0.0..=1.0`, matching
    /// `org.gnome.Shell.Screenshot.PickColor`'s own return shape (Stage 2's
    /// research: niri serves this itself). Stage 16 wires this to
    /// `modules/picker.rs`.
    async fn pick_color(&self) -> zbus::fdo::Result<(f64, f64, f64)> {
        eprintln!("saola-capture: daemon: PickColor()");
        Err(not_yet_implemented("PickColor", "Stage 16"))
    }

    /// `OpenWindow(mode s)` — `mode` is `"main"` or `"edit:<path>"` (see
    /// `cli::WindowAction::dbus_mode`, the sending side's own encoder).
    ///
    /// **Real as of Stage 9**: spawns a detached `saola-capture window
    /// [edit <path>]` process — the same fire-and-forget shape
    /// [`spawn_daemon_detached`] (this file) and `main.rs`'s `spawn_editor`
    /// (the toast-click flow, unchanged since Stage 6) already use, for the
    /// same reason: a Wayland toplevel window needs a live process behind
    /// it, and this method's own caller (a keybind, `open`, the future tray
    /// menu) is not that process.
    ///
    /// **Does not try to *reuse* an already-running-but-hidden window
    /// process** — `modules::app`'s hide-on-capture model (see that
    /// module's doc comment) means a window can be alive-but-invisible, and
    /// this daemon keeps no registry of that (unlike the toast/overlay/
    /// countdown surfaces, a window process's liveness isn't daemon state
    /// at all in v0.1). A second `OpenWindow` while one is already hidden
    /// spawns a second process. Recorded as an acceptable v0.1 gap in the
    /// Stage 9 handoff, not a silent oversight — closing it means the
    /// daemon tracking window-process liveness, which is more than this
    /// stage's task list asks for.
    async fn open_window(&self, mode: String) -> zbus::fdo::Result<()> {
        eprintln!("saola-capture: daemon: OpenWindow(mode={mode:?})");
        spawn_window_process(&mode).map_err(|err| {
            zbus::fdo::Error::Failed(format!("could not open the app window: {err}"))
        })
    }

    /// Emitted after a screenshot is saved. The future
    /// **saola-notifications** contract (CLAUDE.md Boundaries) — kept
    /// stable from this stage on, even though nothing emits it yet (every
    /// method above returns before reaching a success path that would).
    #[zbus(signal)]
    async fn capture_taken(emitter: &SignalEmitter<'_>, path: &str, kind: &str)
        -> zbus::Result<()>;

    /// Emitted when a recording starts.
    #[zbus(signal)]
    async fn recording_started(emitter: &SignalEmitter<'_>, kind: &str) -> zbus::Result<()>;

    /// Emitted when a recording finishes and is saved.
    #[zbus(signal)]
    async fn recording_finished(emitter: &SignalEmitter<'_>, path: &str) -> zbus::Result<()>;

    /// A user-facing failure the toast stack (or, later,
    /// saola-notifications) should surface. Distinct from a method call's
    /// own D-Bus error reply: this is for failures that happen *after* a
    /// method already returned success — an encoder dying mid-recording,
    /// disk-full mid-write.
    #[zbus(signal)]
    async fn error(emitter: &SignalEmitter<'_>, message: &str) -> zbus::Result<()>;
}

/// What [`serve`] settled into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeOutcome {
    /// We own `io.saola.Capture1` and are now the daemon.
    Serving,
    /// Another process already owns the name — CLAUDE.md's single-instance
    /// rule: "a second `daemon` invocation exits cleanly when the name is
    /// taken."
    AlreadyRunning,
}

/// Claim `io.saola.Capture1` on `connection`, or discover somebody already
/// has it.
///
/// Teaching note (**object first, name second**, same rule
/// `tray::watcher::claim_watcher` documents): the object is exported
/// *before* the name is requested, so there is no window in which a caller
/// who saw the name appear on the bus finds nothing answering at
/// [`OBJECT_PATH`].
///
/// Deliberately **no `AllowReplacement`** (unlike the tray watcher, which
/// legitimately might lose its name to a fuller desktop shell): exactly one
/// `saola-capture daemon` should ever run, so `DoNotQueue` alone is enough
/// — either we get it immediately, or somebody else already has it and we
/// report that back to the caller rather than parking in the ownership
/// queue.
pub async fn serve(
    connection: &Connection,
    events: iced::futures::channel::mpsc::Sender<DaemonEvent>,
) -> zbus::Result<ServeOutcome> {
    connection
        .object_server()
        .at(
            OBJECT_PATH,
            CaptureService {
                events,
                recorder: SharedRecorder::default(),
            },
        )
        .await?;

    let claimed = connection
        .request_name_with_flags(SERVICE_NAME, RequestNameFlags::DoNotQueue.into())
        .await;

    match claimed {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {
            Ok(ServeOutcome::Serving)
        }
        // zbus turns `Exists` into `Err(NameTaken)` before we ever see it
        // as a reply variant, and `InQueue` cannot happen with
        // `DoNotQueue` — matching all three keeps this honest rather than
        // relying on an implementation detail of zbus's error mapping to
        // stay put (same posture as the tray watcher's `claim_watcher`).
        Ok(RequestNameReply::InQueue | RequestNameReply::Exists) | Err(zbus::Error::NameTaken) => {
            // Take the unclaimed object back down — nobody will ever call
            // it (we don't own the name), and leaving it registered would
            // only confuse anything that introspects this connection.
            connection
                .object_server()
                .remove::<CaptureService, _>(OBJECT_PATH)
                .await?;
            Ok(ServeOutcome::AlreadyRunning)
        }
        Err(error) => Err(error),
    }
}

/// The client-side half of the interface — one method per served method,
/// generated by `#[zbus::proxy]` from this trait. `default_service`/
/// `default_path` mean `Capture1Proxy::new(&connection)` needs no further
/// arguments at any call site (`main.rs`'s CLI dispatch, and eventually the
/// window process).
#[zbus::proxy(
    interface = "io.saola.Capture1",
    default_service = "io.saola.Capture1",
    default_path = "/io/saola/Capture1"
)]
pub trait Capture1 {
    fn screenshot(&self, kind: &str, options: HashMap<String, OwnedValue>) -> zbus::Result<String>;

    #[zbus(name = "StartRecording")]
    fn start_recording(&self, kind: &str, options: HashMap<String, OwnedValue>)
        -> zbus::Result<()>;

    #[zbus(name = "StopRecording")]
    fn stop_recording(&self) -> zbus::Result<String>;

    /// **Stage 11.** Read-only; see [`CaptureService::recording`] for why a
    /// property was added and what `record toggle` does with it.
    #[zbus(property)]
    fn recording(&self) -> zbus::Result<bool>;

    #[zbus(name = "PickColor")]
    fn pick_color(&self) -> zbus::Result<(f64, f64, f64)>;

    #[zbus(name = "OpenWindow")]
    fn open_window(&self, mode: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn capture_taken(&self, path: String, kind: String);

    #[zbus(signal, name = "RecordingStarted")]
    fn recording_started(&self, kind: String);

    #[zbus(signal, name = "RecordingFinished")]
    fn recording_finished(&self, path: String);

    #[zbus(signal)]
    fn error(&self, message: String);
}

/// Why [`ensure_daemon_running`] gave up.
#[derive(Debug)]
pub enum ClientError {
    /// Talking to the session bus itself failed (no bus at all — unusual
    /// outside a container/CI environment).
    Bus(zbus::Error),
    /// `Command::spawn` for the detached daemon failed (binary not on
    /// `$PATH`/not executable — vanishingly unlikely for `current_exe`, but
    /// still an I/O call that can fail).
    Spawn(std::io::Error),
    /// The daemon was spawned but never claimed the bus name within
    /// [`SPAWN_WAIT_BUDGET`] — a hung Wayland connection, a missing
    /// dependency crashing it on boot, etc. Actionable for the human: run
    /// `saola-capture daemon` in a terminal and read what it prints.
    DidNotStart,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Bus(err) => write!(f, "could not reach the session bus: {err}"),
            ClientError::Spawn(err) => write!(f, "could not start the daemon: {err}"),
            ClientError::DidNotStart => write!(
                f,
                "started the daemon but it never claimed {SERVICE_NAME} — \
                 run `saola-capture daemon` directly to see why"
            ),
        }
    }
}

impl std::error::Error for ClientError {}

/// Ensure a daemon is reachable on `connection`, spawning one detached and
/// waiting for it if `io.saola.Capture1` is currently unowned.
///
/// This is the one auto-spawn attempt PLAN.md Stage 3 asks for ("retries
/// once when the name is unowned") — a single spawn, then a bounded poll,
/// never a retry loop that could paper over a daemon that keeps crashing on
/// boot.
pub async fn ensure_daemon_running(connection: &Connection) -> Result<(), ClientError> {
    let bus = DBusProxy::new(connection).await.map_err(ClientError::Bus)?;

    let owned = name_has_owner(&bus).await?;
    if owned {
        return Ok(());
    }

    spawn_daemon_detached()?;

    let mut waited = Duration::ZERO;
    while waited < SPAWN_WAIT_BUDGET {
        tokio::time::sleep(SPAWN_POLL_INTERVAL).await;
        waited += SPAWN_POLL_INTERVAL;
        if name_has_owner(&bus).await? {
            return Ok(());
        }
    }

    Err(ClientError::DidNotStart)
}

async fn name_has_owner(bus: &DBusProxy<'_>) -> Result<bool, ClientError> {
    let name = zbus::names::BusName::try_from(SERVICE_NAME)
        // `SERVICE_NAME` is a compile-time constant known to be a valid
        // bus name — this can never actually fail, but the fallible
        // `TryFrom` still has to be answered without `.expect()` on a
        // runtime path (CLAUDE.md's no-panic rule), so an unreachable
        // failure here degrades to "not owned" rather than unwrapping.
        .map_err(|_| ClientError::DidNotStart)?;
    bus.name_has_owner(name)
        .await
        .map_err(|err| ClientError::Bus(zbus::Error::from(err)))
}

/// Spawn `saola-capture daemon` detached from this process — not waited on,
/// stdio redirected to `/dev/null` so it doesn't inherit the CLI's
/// terminal.
///
/// Teaching note (why this isn't a full double-fork daemon): a proper Unix
/// daemon calls `setsid` and forks twice so it can never be reattached to a
/// controlling terminal. This just spawns and drops the `Child` handle,
/// which is enough here because the parent (the CLI verb that triggered
/// this) is about to exit on its own anyway — the daemon is reparented to
/// init and keeps running, exactly like any other background process a
/// shell launches with `&`. If v0.2 ever needs the stronger guarantee
/// (surviving a parent that sends its whole process group a signal, say),
/// that's a `nix::unistd::setsid` call to add here, not a redesign.
fn spawn_daemon_detached() -> Result<(), ClientError> {
    let exe = std::env::current_exe().map_err(ClientError::Spawn)?;
    Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(ClientError::Spawn)?;
    Ok(())
}

/// Spawn `saola-capture window [edit <path>]` detached —
/// [`CaptureService::open_window`]'s real implementation (Stage 9). `mode`
/// is the wire string `cli::WindowAction::dbus_mode` produces on the
/// sending side (`"main"` or `"edit:<path>"`); this is that function's
/// inverse, kept deliberately tiny (one `strip_prefix`) rather than shared
/// code — the two ends of a D-Bus string argument only need to agree on its
/// shape, not share a parser to do it.
fn spawn_window_process(mode: &str) -> Result<(), std::io::Error> {
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command.arg("window");
    if let Some(path) = mode.strip_prefix("edit:") {
        command.arg("edit").arg(path);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::recorder::PumpOutcome;

    /// The first live encoder-death run (Stage 11) produced this exact
    /// doubled sentence in both the toast and the `Error` signal: the pump
    /// reported the child's death, and `sink.finish()` then reported the same
    /// death again, so `recording_result` concatenated a string with itself.
    #[test]
    fn a_flush_that_repeats_the_pumps_reason_is_not_said_twice() {
        let death = "ffmpeg was killed by a signal".to_string();
        let result = recording_result(
            &PumpOutcome::EncoderFailed(death.clone()),
            Err(death.clone()),
        );
        let message = result.unwrap_err();
        assert_eq!(message, format!("the encoder failed: {death}"));
        assert_eq!(
            message.matches("killed by a signal").count(),
            1,
            "{message}"
        );
    }

    /// …but a flush that failed for a *different* reason is genuinely new
    /// information and still gets appended.
    #[test]
    fn a_flush_with_its_own_reason_is_appended() {
        let message = recording_result(
            &PumpOutcome::StreamError("the cast collapsed".to_string()),
            Err("the output file was empty".to_string()),
        )
        .unwrap_err();
        assert!(message.contains("the cast collapsed"), "{message}");
        assert!(message.contains("the output file was empty"), "{message}");
    }

    /// An unclean end whose file still flushed is a failure that names the
    /// bytes it did keep — Matroska tolerates exactly this.
    #[test]
    fn a_failed_recording_that_still_flushed_names_the_partial_file() {
        let message = recording_result(
            &PumpOutcome::EncoderFailed("the disk filled up".to_string()),
            Ok(PathBuf::from("/tmp/Recording_x.mkv")),
        )
        .unwrap_err();
        assert!(message.contains("the disk filled up"), "{message}");
        assert!(message.contains("/tmp/Recording_x.mkv"), "{message}");
    }

    /// Both clean endings return the path, including a cast the compositor
    /// closed on its own — the file is finished either way.
    #[test]
    fn every_clean_ending_returns_the_saved_path() {
        for outcome in [PumpOutcome::StopRequested, PumpOutcome::StreamEnded] {
            let result = recording_result(&outcome, Ok(PathBuf::from("/tmp/ok.mkv")));
            assert_eq!(result, Ok("/tmp/ok.mkv".to_string()), "{outcome:?}");
        }
    }

    /// A clean stop whose flush failed reports the flush's own error, with
    /// nothing invented in front of it.
    #[test]
    fn a_clean_stop_with_a_failed_flush_reports_only_the_flush() {
        let message = recording_result(
            &PumpOutcome::StopRequested,
            Err("ffmpeg exited with status 1: No space left on device".to_string()),
        )
        .unwrap_err();
        assert_eq!(
            message,
            "ffmpeg exited with status 1: No space left on device"
        );
    }
}
