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
//! don't exist yet, so every method here logs the call and returns a clean
//! D-Bus error naming the stage that will implement it. This is the same
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
use std::process::{Command, Stdio};
use std::time::Duration;

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

    /// `StartRecording(kind s, options a{sv})`. Stage 10/11 wire this to
    /// `capture/screencast.rs` + `encode/ffmpeg_cli.rs`.
    async fn start_recording(
        &self,
        kind: String,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        eprintln!(
            "saola-capture: daemon: StartRecording(kind={kind:?}, {} option(s))",
            options.len()
        );
        Err(not_yet_implemented("StartRecording", "Stage 10/11"))
    }

    /// `StopRecording() -> s` — returns the saved recording's path. Stage
    /// 10 wires this to `modules/recorder.rs`'s state machine.
    async fn stop_recording(&self) -> zbus::fdo::Result<String> {
        eprintln!("saola-capture: daemon: StopRecording()");
        Err(not_yet_implemented("StopRecording", "Stage 11"))
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
    /// `cli::WindowAction`). Stage 9 wires this to spawning/raising the
    /// separate window process.
    async fn open_window(&self, mode: String) -> zbus::fdo::Result<()> {
        eprintln!("saola-capture: daemon: OpenWindow(mode={mode:?})");
        Err(not_yet_implemented("OpenWindow", "Stage 9"))
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
        .at(OBJECT_PATH, CaptureService { events })
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
