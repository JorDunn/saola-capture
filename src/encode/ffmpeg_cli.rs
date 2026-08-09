//! The one v0.1 [`EncoderSink`]: an `ffmpeg` child process fed raw BGRx
//! frames on stdin.
//!
//! **ffmpeg is an external CLI boundary** (CLAUDE.md Boundaries) — this file
//! spawns it, feeds it, reads its stderr and reaps it, and never links a
//! single libav symbol. Everything about *what* to spawn is in
//! [`super`]'s pure tables; everything about *the world* is here.
//!
//! # Three things this file exists to get right
//!
//! 1. **The stderr pipe must never fill.** A child whose stderr pipe is full
//!    blocks in `write`, stops reading its own stdin, and the recording
//!    deadlocks — with no error anywhere, because nothing failed. So stderr
//!    is drained continuously by a thread that both logs each line and keeps
//!    the last [`STDERR_TAIL_LINES`] for the error message. (This is the one
//!    kind of extra thread CLAUDE.md's "one runtime" rule doesn't need to
//!    sanction specially: it runs no executor, owns no state beyond a
//!    `Mutex<VecDeque<String>>`, and exits when the pipe closes. The
//!    alternative — `Stdio::inherit()` — needs no thread but throws away the
//!    stderr tail, which is exactly where "No space left on device" appears.)
//! 2. **A dead child must surface, not linger.** [`FfmpegSink::poll_health`]
//!    is called between frames, [`FfmpegSink::write_video`] turns a broken
//!    pipe into a real error with the child's own last words, and `Drop`
//!    kills and *waits* — a killed child that is never waited on is a zombie
//!    (this repo has already accumulated some, from an unrelated path).
//! 3. **The VAAPI device is discovered, never assumed.** See
//!    [`choose_encoder`] and CAPTURE-RESEARCH D6's 2026-08-08 amendment.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::{
    ffmpeg_args, select_encoder, EncodeError, EncodePreset, EncoderChoice, EncoderSink, RecordSpec,
    VideoEncoder,
};

/// How many stderr lines are kept for the error message. Enough to include
/// ffmpeg's usual two-line failure (the failing call plus the reason) with
/// room for whatever warnings preceded it; small enough to be worth holding
/// in memory for the whole recording.
const STDERR_TAIL_LINES: usize = 40;

/// The trial encode's frame size.
///
/// **Not arbitrary, and not smaller.** The first Stage 11 probe used 64×64
/// and every codec on every node "failed" with
/// `Hardware does not support encoding at size 64x64 (constraints: width
/// 130-8192 height 128-4352)` — a size rejection that looks exactly like a
/// missing entrypoint if you only check the exit status. 256×256 clears both
/// AMD constraint sets measured (HEVC needs ≥130×128, H.264 ≥128×128) with
/// margin, and still encodes in ~250 ms.
const PROBE_SIZE: u32 = 256;

/// How long any single probe encode may take before it is killed and treated
/// as unsupported. A wedged GPU must not wedge the daemon.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long [`FfmpegSink::finish`] waits after closing stdin before
/// escalating to SIGINT. Generous because an MP4 `+faststart` rewrite of a
/// long recording happens entirely inside this window.
const FINISH_WAIT: Duration = Duration::from_secs(30);

/// How long the SIGINT escalation gets before SIGKILL.
const SIGINT_WAIT: Duration = Duration::from_secs(10);

/// Poll granularity for every bounded wait in this file.
const WAIT_POLL: Duration = Duration::from_millis(25);

/// `-loglevel`'s value, overridable for debugging.
///
/// `warning` by default: ffmpeg's `info` level prints a full input/output
/// dump per recording and its per-frame stats line is suppressed separately
/// (`-nostats`). Set `SAOLA_CAPTURE_FFMPEG_LOGLEVEL=verbose` to get the lines
/// that *prove* hardware encode — CAPTURE-RESEARCH §3.1's
/// `Using VAAPI entrypoint VAEntrypointEncSlice (6).` — which is exactly how
/// Stage 11's live verification was done.
fn loglevel() -> String {
    std::env::var("SAOLA_CAPTURE_FFMPEG_LOGLEVEL").unwrap_or_else(|_| "warning".to_string())
}

// ---------------------------------------------------------------------
// Availability
// ---------------------------------------------------------------------

/// Is `ffmpeg` on `$PATH`? Cached for the process's life — the answer cannot
/// change usefully mid-run, and a `record start` should not fork a probe
/// process to re-learn it.
///
/// Deliberately checked **up front** (PLAN.md Stage 11 task 2: "missing-ffmpeg
/// detected up front with a clean error") rather than inferred from a spawn
/// failure later: by the time a spawn fails, a screencast session is already
/// open and a save path already allocated, and unwinding that to say
/// "install ffmpeg" is strictly worse than never starting.
pub fn ensure_ffmpeg_available() -> Result<(), EncodeError> {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    let available = *AVAILABLE.get_or_init(|| {
        Command::new("ffmpeg")
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    });
    if available {
        Ok(())
    } else {
        Err(EncodeError::FfmpegMissing)
    }
}

// ---------------------------------------------------------------------
// VAAPI device discovery
// ---------------------------------------------------------------------

/// Every DRM render node on this machine, sorted by name.
///
/// Sorted so the choice is deterministic across runs (readdir order is not),
/// which matters because "the first node that works" is half of
/// [`select_encoder`]'s rule. `renderD128` sorts before `renderD129`, so the
/// iGPU keeps winning ties on Jordan's machine, which is what we want: it is
/// the one driving the display, so its encoder reads the frames without a
/// cross-device copy.
pub fn render_nodes() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/dev/dri") else {
        return Vec::new();
    };
    let mut nodes: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("renderD"))
        })
        .collect();
    nodes.sort();
    nodes
}

/// Can `device` open `encoder`? Answered by an actual trial encode, cached
/// per `(device, encoder)` pair for the process's life.
///
/// # Why a trial encode and not `vainfo`
///
/// PLAN.md Stage 11 task 3 is explicit: "do **not** add a runtime `vainfo`
/// dependency". Beyond keeping ffmpeg the sole external CLI, the trial encode
/// answers a strictly better question — `vainfo` reports what the *driver*
/// advertises, while this reports what *this ffmpeg build, on this device,
/// right now* can actually open. CAPTURE-RESEARCH §3.3's AV1 case is exactly
/// the gap: `av1_vaapi` exists in the ffmpeg build and the driver advertises
/// `VAProfileAV1Profile0`, and the encode still fails, because the profile is
/// decode-only.
///
/// A probe that times out counts as unsupported, and says so.
fn probe_encoder(device: &Path, encoder: VideoEncoder) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<(PathBuf, VideoEncoder), bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (device.to_path_buf(), encoder);

    // A poisoned lock (a panic while probing — nothing here can, but the
    // no-panic rule has no "cannot happen" exemption) is recovered rather
    // than propagated: the map holds only cached booleans, so the worst a
    // recovered guard can do is re-probe.
    if let Some(cached) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
    {
        return *cached;
    }

    let started = Instant::now();
    let supported = run_probe(device, encoder);
    eprintln!(
        "saola-capture: encode: probe {} on {} -> {} ({} ms)",
        encoder,
        device.display(),
        if supported { "yes" } else { "no" },
        started.elapsed().as_millis()
    );

    cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, supported);
    supported
}

/// One trial encode: a few frames of solid black from `lavfi`, uploaded and
/// encoded, muxed to `-f null -`. Writes nothing anywhere.
///
/// The filter chain here is deliberately the *simple* `format=nv12,hwupload`
/// rather than the real chain's `hwupload,scale_vaapi=…`: the question is
/// "does this encoder open on this device", and the simpler chain has fewer
/// ways to fail for reasons that are not the encoder's.
fn run_probe(device: &Path, encoder: VideoEncoder) -> bool {
    let size = format!("{PROBE_SIZE}x{PROBE_SIZE}");
    let child = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-loglevel", "error", "-nostdin"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!("color=c=black:s={size}:d=0.04:r=25"))
        .arg("-vaapi_device")
        .arg(device)
        .args(["-vf", "format=nv12,hwupload"])
        .arg("-c:v")
        .arg(encoder.as_str())
        .args(["-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    let Ok(mut child) = child else {
        return false;
    };

    match wait_bounded(&mut child, PROBE_TIMEOUT) {
        Some(status) => status.success(),
        None => {
            eprintln!(
                "saola-capture: encode: probing {} on {} timed out after {:?} — treating it as \
                 unsupported",
                encoder,
                device.display(),
                PROBE_TIMEOUT
            );
            let _ = child.kill();
            let _ = child.wait();
            false
        }
    }
}

/// Resolve `preset` to a concrete encoder and device on *this* machine.
///
/// `requested` is `capture.toml`'s `vaapi-device` override. A node that
/// exists is used **on its own** (so the override genuinely overrides rather
/// than merely reordering); a node that doesn't exist warns and falls back to
/// full enumeration, per this repo's per-knob warn-and-default convention —
/// a stale path in a config file must not make recording impossible.
pub fn choose_encoder(
    preset: EncodePreset,
    requested: Option<&Path>,
) -> Result<EncoderChoice, EncodeError> {
    ensure_ffmpeg_available()?;

    let devices = match requested {
        Some(path) if path.exists() => vec![path.to_path_buf()],
        Some(path) => {
            eprintln!(
                "saola-capture: capture.toml: vaapi-device {} does not exist — probing every \
                 /dev/dri/renderD* instead",
                path.display()
            );
            render_nodes()
        }
        None => render_nodes(),
    };

    let started = Instant::now();
    let choice = select_encoder(preset, &devices, probe_encoder)?;
    eprintln!(
        "saola-capture: encode: {preset} preset resolved to {choice} ({} ms)",
        started.elapsed().as_millis()
    );
    Ok(choice)
}

// ---------------------------------------------------------------------
// The sink
// ---------------------------------------------------------------------

/// A running `ffmpeg`, mid-recording.
pub struct FfmpegSink {
    /// `None` once the child has been reaped by [`Self::finish`]/[`Self::abort`],
    /// which is also what stops `Drop` from killing it a second time.
    child: Option<Child>,
    /// Taken (and thereby closed) at the start of `finish`, which is what
    /// EOFs the rawvideo input.
    stdin: Option<std::process::ChildStdin>,
    stderr_drain: Option<std::thread::JoinHandle<()>>,
    tail: std::sync::Arc<Mutex<VecDeque<String>>>,
    path: PathBuf,
    frame_len: usize,
}

impl FfmpegSink {
    /// Spawn ffmpeg for `spec` with the device choice already made.
    ///
    /// The choice is an argument rather than something this constructor works
    /// out, because it depends on `capture.toml`'s `vaapi-device` override and
    /// this module deliberately reads no config — [`choose_encoder`] is the
    /// other half, and the daemon calls the two in sequence.
    ///
    /// **Blocking, and not briefly**: a fork+exec, and (through
    /// [`choose_encoder`] just before it) the first recording in a daemon's
    /// life also pays the device probe. Callers run both on tokio's blocking
    /// pool (`dbus::run_blocking`), never on the executor.
    pub fn start(spec: &RecordSpec, choice: EncoderChoice) -> Result<Self, EncodeError> {
        ensure_ffmpeg_available()?;

        let args = ffmpeg_args(spec, &choice, &loglevel());
        eprintln!(
            "saola-capture: encode: ffmpeg {}",
            args.iter()
                .map(|arg| arg.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );

        let mut child = Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    EncodeError::FfmpegMissing
                } else {
                    EncodeError::Spawn(err)
                }
            })?;

        // Both pipes are `piped()` above, so `take()` yields `Some` — but a
        // `None` still has to produce an error rather than an unwrap, and
        // "ffmpeg started without the pipes we asked for" is not a state this
        // process can do anything with.
        let stdin = child.stdin.take().ok_or_else(|| {
            EncodeError::Spawn(std::io::Error::other("ffmpeg gave us no stdin pipe"))
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            EncodeError::Spawn(std::io::Error::other("ffmpeg gave us no stderr pipe"))
        })?;

        let tail = std::sync::Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
        let drain_tail = std::sync::Arc::clone(&tail);
        let stderr_drain = std::thread::Builder::new()
            .name("ffmpeg-stderr".to_string())
            .spawn(move || drain_stderr(stderr, &drain_tail))
            .map_err(EncodeError::Spawn)?;

        Ok(FfmpegSink {
            child: Some(child),
            stdin: Some(stdin),
            stderr_drain: Some(stderr_drain),
            tail,
            path: spec.path.clone(),
            frame_len: spec.video.frame_len(),
        })
    }

    /// The last stderr lines seen so far.
    fn tail_lines(&self) -> Vec<String> {
        self.tail
            .lock()
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Turn "the child is gone" into the error that says why.
    fn died(&mut self, status: Option<ExitStatus>) -> EncodeError {
        let code = status.and_then(|status| status.code());
        EncodeError::Died {
            code,
            tail: self.tail_lines(),
        }
    }

    /// Join the stderr thread, which returns once ffmpeg closes the pipe.
    /// Called only after the child has been reaped, so it cannot block.
    fn join_drain(&mut self) {
        if let Some(handle) = self.stderr_drain.take() {
            let _ = handle.join();
        }
    }

    /// Kill (SIGKILL) and reap. Infallible on purpose — everything that calls
    /// it is already handling some other failure.
    fn kill_and_reap(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.join_drain();
    }

    /// Remove the output file if ffmpeg left nothing worth keeping.
    ///
    /// Only ever removes a **zero-byte** file: a recording that produced real
    /// bytes before dying is left on disk (Matroska survives truncation, and
    /// a partial recording is better than none — see [`RecordSpec::path`]).
    fn discard_if_empty(&self) {
        if std::fs::metadata(&self.path)
            .map(|meta| meta.len())
            .unwrap_or(1)
            == 0
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl EncoderSink for FfmpegSink {
    fn write_video(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        // A short or long frame would not fail — it would silently reframe
        // every subsequent row, producing a diagonally sheared video. Caught
        // here rather than trusted, because the negotiated size can change
        // under us (`param_changed` can fire more than once).
        if bytes.len() != self.frame_len {
            return Err(EncodeError::Write(std::io::Error::other(format!(
                "frame is {} bytes, expected {}",
                bytes.len(),
                self.frame_len
            ))));
        }

        let Some(stdin) = self.stdin.as_mut() else {
            let status = self
                .child
                .as_mut()
                .and_then(|c| c.try_wait().ok().flatten());
            return Err(self.died(status));
        };

        match stdin.write_all(bytes) {
            Ok(()) => Ok(()),
            Err(err) => {
                // A broken pipe means the child is gone; its own stderr says
                // why (a full disk, a rejected format, a SIGKILL from
                // somewhere else), and that is far more useful than "Broken
                // pipe". Give it a moment to be reaped so the exit status is
                // available.
                if err.kind() == std::io::ErrorKind::BrokenPipe {
                    let status = self
                        .child
                        .as_mut()
                        .and_then(|child| wait_bounded(child, Duration::from_secs(2)));
                    if status.is_some() {
                        // Already reaped by `wait_bounded`.
                        self.child = None;
                        self.stdin = None;
                        self.join_drain();
                    }
                    Err(self.died(status))
                } else {
                    Err(EncodeError::Write(err))
                }
            }
        }
    }

    fn poll_health(&mut self) -> Result<(), EncodeError> {
        let exited = match self.child.as_mut() {
            Some(child) => child.try_wait().ok().flatten(),
            // Already reaped: whatever reaped it produced the real error, and
            // this is the follow-up call.
            None => return Err(self.died(None)),
        };
        match exited {
            None => Ok(()),
            Some(status) => {
                self.child = None;
                self.stdin = None;
                self.join_drain();
                Err(self.died(Some(status)))
            }
        }
    }

    fn finish(mut self: Box<Self>) -> Result<PathBuf, EncodeError> {
        // 1. EOF the rawvideo input. For a video-only recording this alone
        //    makes ffmpeg flush and exit.
        drop(self.stdin.take());

        let Some(mut child) = self.child.take() else {
            // The child already died and was reaped — `write_video`/
            // `poll_health` produced the real error; repeat it rather than
            // inventing a new one.
            let err = self.died(None);
            self.discard_if_empty();
            return Err(err);
        };

        // 2. Wait. An MP4 `+faststart` rewrite lives entirely inside this
        //    window, which is why it is generous.
        let mut status = wait_bounded(&mut child, FINISH_WAIT);

        // 3. Escalate. Stage 13's `-f pulse` input never EOFs
        //    (CAPTURE-RESEARCH §4.3), so stdin closing will not be enough
        //    then; SIGINT is ffmpeg's documented graceful stop and makes it
        //    finalize the file properly. Writing `q` to stdin — the other
        //    documented way — is impossible here: stdin *is* the video pipe.
        if status.is_none() {
            eprintln!(
                "saola-capture: encode: ffmpeg did not exit within {FINISH_WAIT:?} of stdin \
                 closing — sending SIGINT"
            );
            send_sigint(&child);
            status = wait_bounded(&mut child, SIGINT_WAIT);
        }

        // 4. Give up gracefully.
        if status.is_none() {
            eprintln!("saola-capture: encode: ffmpeg ignored SIGINT — killing it");
            let _ = child.kill();
            status = child.wait().ok();
        }

        self.join_drain();

        match status {
            Some(status) if status.success() => {
                match std::fs::metadata(&self.path).map(|meta| meta.len()) {
                    Ok(0) | Err(_) => {
                        self.discard_if_empty();
                        Err(EncodeError::EmptyOutput(self.path.clone()))
                    }
                    Ok(_) => Ok(self.path.clone()),
                }
            }
            other => {
                let err = self.died(other);
                self.discard_if_empty();
                Err(err)
            }
        }
    }

    fn abort(mut self: Box<Self>) {
        drop(self.stdin.take());
        self.kill_and_reap();
        self.discard_if_empty();
    }

    fn output_path(&self) -> &Path {
        &self.path
    }
}

/// Kill-on-drop plus zombie reaping (PLAN.md Stage 11 task 2).
///
/// `std::process::Child` deliberately does **not** kill on drop — the child
/// keeps running, orphaned, and its exit status is never collected. For a
/// recording that means an ffmpeg holding a VAAPI context and writing to a
/// file nobody is tracking, forever. Every early return in a start sequence
/// therefore relies on this.
impl Drop for FfmpegSink {
    fn drop(&mut self) {
        if self.child.is_some() {
            eprintln!(
                "saola-capture: encode: dropping a live ffmpeg for {} — killing it",
                self.path.display()
            );
        }
        drop(self.stdin.take());
        self.kill_and_reap();
    }
}

// ---------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------

/// Read `stderr` line by line until it closes, logging each line and keeping
/// the last [`STDERR_TAIL_LINES`].
///
/// Runs on its own thread for the child's whole life; see this module's doc
/// comment for why not draining is a deadlock rather than a lost log.
fn drain_stderr(stderr: std::process::ChildStderr, tail: &Mutex<VecDeque<String>>) {
    let reader = BufReader::new(stderr);
    for line in reader.lines() {
        // A non-UTF-8 line (ffmpeg can print a file name verbatim) ends the
        // drain early rather than looping — but the pipe still gets closed
        // when the child exits, so no deadlock follows.
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        eprintln!("saola-capture: ffmpeg: {line}");
        if let Ok(mut tail) = tail.lock() {
            if tail.len() == STDERR_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    }
}

/// `child.wait()`, but bounded: polls `try_wait` until `timeout` elapses.
/// `None` means "still running".
///
/// Polling rather than a blocking `wait()` because every caller needs an
/// escalation path — a `wait()` that never returns is exactly the failure
/// mode this file is written to avoid.
fn wait_bounded(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            // An error from `try_wait` means the child cannot be waited on at
            // all (it was reaped elsewhere); reporting "gone" beats spinning.
            Err(_) => return None,
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(WAIT_POLL);
    }
}

/// SIGINT to the child — ffmpeg's documented graceful stop.
///
/// Teaching note: `std::process::Child` can only `kill()` (SIGKILL), which
/// for ffmpeg means an unfinalized file. There is no std API for other
/// signals, so this is a one-line `libc::kill` — `libc` is already a
/// dependency (see its survey in `Cargo.toml`) and this adds nothing.
fn send_sigint(child: &Child) {
    let pid = match i32::try_from(child.id()) {
        Ok(pid) => pid,
        // A pid that doesn't fit in an `i32` cannot happen on Linux; the
        // no-panic rule still wants the fallible conversion answered.
        Err(_) => return,
    };
    // SAFETY: `kill` with a pid this process owns as a child, and a signal
    // constant. It cannot invalidate any Rust memory; the worst outcome is
    // `ESRCH` if the child was already reaped, which is ignored.
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe size must clear both constraint sets Stage 11 measured
    /// live — this is a regression guard on a value whose wrongness looks
    /// exactly like "this machine has no encoder".
    #[test]
    fn the_probe_size_clears_the_measured_hardware_minimums() {
        // hevc_vaapi: width 130-8192, height 128-4352.
        // h264_vaapi: width 128-4096, height 128-4096.
        // A `const` block so the check is a compile-time fact rather than a
        // runtime assertion clippy (correctly) calls pointless.
        const _: () = assert!(PROBE_SIZE >= 130 && PROBE_SIZE <= 4096);
        // Named here too, so the test's failure message would say what broke.
        assert_eq!(PROBE_SIZE, 256, "the measured-safe trial size");
    }

    #[test]
    fn render_nodes_are_sorted_and_only_render_nodes() {
        // Reads the real /dev/dri — a machine with no DRM at all (CI in a
        // container) legitimately returns an empty list, which is why this
        // asserts shape rather than content.
        let nodes = render_nodes();
        let mut sorted = nodes.clone();
        sorted.sort();
        assert_eq!(nodes, sorted, "enumeration must be deterministic");
        for node in &nodes {
            let name = node
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            assert!(name.starts_with("renderD"), "{name}");
        }
    }

    #[test]
    fn the_default_loglevel_is_quiet_but_overridable() {
        // Reads the process environment, never writes it — CLAUDE.md's
        // binding "never `std::env::set_var` in a test" rule.
        match std::env::var("SAOLA_CAPTURE_FFMPEG_LOGLEVEL") {
            Ok(value) => assert_eq!(loglevel(), value),
            Err(_) => assert_eq!(loglevel(), "warning"),
        }
    }
}
