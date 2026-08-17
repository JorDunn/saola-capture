//! `saola-capture` entry point: parses the CLI and dispatches to one of the
//! three run modes described in `CLAUDE.md` — `daemon`, `window`, or one of
//! the CLI verbs (`shot`, `record`, `pick-color`, `open`).
//!
//! # What is real, and what is still a stub
//!
//! Stage 3 made the *plumbing* real — argument parsing (`cli.rs`),
//! config-vs-flag resolution (`cli::CaptureOptions`/`cli::RecordOptions`),
//! the `io.saola.Capture1` bus (`dbus.rs`), and the daemon's surfaceless
//! boot — while every capture backend stayed a stub.
//!
//! **Stage 5 made `shot` real**, both ways: `run_shot_in_process`
//! (`--no-daemon`) and the daemon's `Screenshot` method call the same two
//! library functions (`capture::take_screenshot` then
//! `storage::save_capture`), and the saved path prints to stdout.
//!
//! **Stage 7 made an interactive `--region` real** — through the daemon
//! only, since it needs a layer-shell surface: `capture::
//! freeze_focused_output` → `modules::overlay` on a
//! `SurfaceRole::Overlay` surface → `capture::crop_frozen_frame` →
//! the same `storage::save_capture`. A `--no-daemon --region` with no
//! `--geometry` still reports a clean error, because a surfaceless process
//! has nowhere to draw a selection.
//!
//! **Stage 8 made `--window` real**, and **Stage 9 made `window` and
//! `OpenWindow` real**: `run_window` now boots `modules::app`'s plain
//! `iced::application` (a `Surface::Paper` toplevel, D-Bus client of the
//! daemon — see that module's doc comment for the hide-until-reply model
//! and the `window edit <path>` stub editor), and `dbus.rs`'s `OpenWindow`
//! spawns it detached instead of answering with a stub error.
//!
//! What is still a stub: `record start|stop|toggle` (Stage 11 — the daemon
//! has no encoder yet) and `pick-color` (Stage 16) — each reporting a clean
//! error naming its stage rather than failing silently. **`record start
//! --dry-run` is not a stub as of Stage 10**: it runs the whole
//! ScreenCast-plus-PipeWire negotiation in this process (`run_record_dry_run`
//! → `capture::screencast::dry_run`) and never contacts the daemon at all.
//!
//! # The two process shapes in this file
//!
//! `run_daemon` builds an `iced_layershell` daemon — a winit-style event
//! loop that owns the rest of the process forever, driven by iced's own
//! `tokio`-backed executor (CLAUDE.md's "one runtime" rule: the daemon
//! never constructs a second `tokio::Runtime` of its own; every async
//! thing it does — the D-Bus worker, the SIGTERM/SIGINT wait — runs as an
//! `iced::Subscription`, the same shape `saola-panel`'s zbus-driven modules
//! use). Every other mode (`shot`, `record`, `pick-color`, `open`) is a
//! short-lived plain-async CLI verb with **no** iced event loop, so it
//! *does* build its own `tokio::runtime::Runtime` — see `run_async`'s doc
//! comment for why that's still "one runtime" and not a violation of the
//! rule.

mod audio;
mod capture;
mod cli;
mod config;
mod dbus;
mod encode;
mod modules;
mod storage;

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Parser;
use iced::futures::channel::mpsc;
use iced::futures::{SinkExt, Stream, StreamExt};
use iced::widget::{image, Space};
use iced::window;
use iced::{Element, Subscription, Task};
use iced_layershell::build_pattern::daemon;
use iced_layershell::reexport::{
    Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption,
};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;
use saola_theme::Theme;

use cli::{Cli, Command};
use config::CaptureConfig;

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Command::Daemon => run_daemon(),
        Command::Window { action } => run_window(action.as_ref()),
        Command::Shot(args) => report(run_shot(cli.config_dir.as_deref(), args)),
        Command::Record(args) => report(run_record(cli.config_dir.as_deref(), args)),
        Command::PickColor => report(run_pick_color()),
        Command::Open => report(run_open()),
        Command::ClipboardServe(args) => run_clipboard_serve(&args.mime),
    }
}

/// Print a CLI verb's result the way Architecture specifies: "the saved
/// path prints to stdout; errors to stderr with nonzero exit." Every verb
/// in this file that reaches the daemon funnels through this — the one
/// place stdout/stderr/exit-code discipline is enforced, so no dispatch
/// arm can accidentally print an error to stdout or a path to stderr.
fn report(result: Result<String, CliRunError>) -> ExitCode {
    match result {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("saola-capture: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Every way a CLI verb can fail, collapsed into one error type so
/// `report` has one thing to format. Deliberately *not* `Box<dyn Error>`:
/// naming each source explicitly means a new failure mode has to be
/// threaded through consciously rather than auto-boxing into an opaque
/// blob, which matters here because several of these (`Args`, `Capture`,
/// `Storage`) are this crate's own, user-actionable messages, not just
/// wrapped library errors.
#[derive(Debug)]
enum CliRunError {
    /// A flag, or flag/config combination, didn't resolve — see `cli.rs`'s
    /// `CliError`.
    Args(cli::CliError),
    /// The session bus itself, or a served method call, failed.
    Bus(zbus::Error),
    /// Reaching the daemon (auto-spawn included) failed — see
    /// `dbus::ClientError`.
    Client(dbus::ClientError),
    /// This crate's own `tokio::Runtime` (the CLI-verb process's one
    /// runtime — see `run_async`'s doc comment) failed to build. Distinct
    /// from `Bus`/`Client` because it means nothing async ever ran at all.
    Runtime(std::io::Error),
    /// The in-process (`--no-daemon`) capture itself failed — no
    /// compositor, no outputs, a refused screencopy request. Only the
    /// `--no-daemon` path produces this; on the daemon path the same
    /// failures come back as a D-Bus error reply and land in `Bus`.
    Capture(capture::CaptureError),
    /// The in-process capture succeeded but could not be saved.
    Storage(storage::StorageError),
    /// **Stage 10.** A `record start --dry-run` could not negotiate a
    /// screencast — no `org.gnome.Mutter.ScreenCast`, no PipeWire node, no
    /// PipeWire daemon. Its own arm rather than folding into `Bus` because
    /// `capture::screencast::CastError` already carries the actionable half
    /// of each message (which `busctl`/`systemctl` line to try), and
    /// flattening it into a bare `zbus::Error` would throw that away.
    Cast(capture::screencast::CastError),
}

impl fmt::Display for CliRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliRunError::Args(err) => write!(f, "{err}"),
            CliRunError::Bus(err) => write!(f, "{err}"),
            CliRunError::Client(err) => write!(f, "{err}"),
            CliRunError::Runtime(err) => write!(f, "could not start an async runtime: {err}"),
            CliRunError::Capture(err) => write!(f, "{err}"),
            CliRunError::Storage(err) => write!(f, "{err}"),
            CliRunError::Cast(err) => write!(f, "{err}"),
        }
    }
}

impl From<capture::CaptureError> for CliRunError {
    fn from(err: capture::CaptureError) -> Self {
        CliRunError::Capture(err)
    }
}

impl From<storage::StorageError> for CliRunError {
    fn from(err: storage::StorageError) -> Self {
        CliRunError::Storage(err)
    }
}

impl From<cli::CliError> for CliRunError {
    fn from(err: cli::CliError) -> Self {
        CliRunError::Args(err)
    }
}

impl From<zbus::Error> for CliRunError {
    fn from(err: zbus::Error) -> Self {
        CliRunError::Bus(err)
    }
}

impl From<dbus::ClientError> for CliRunError {
    fn from(err: dbus::ClientError) -> Self {
        CliRunError::Client(err)
    }
}

/// `capture.toml`, resolved and loaded exactly once per CLI-verb
/// invocation. `config_dir` is `Cli::config_dir` (the `--config-dir`
/// override, a global flag so it works before or after the subcommand —
/// see `cli::Cli`'s doc comment).
fn load_config(config_dir: Option<&Path>) -> CaptureConfig {
    let path = CaptureConfig::resolve_path(config_dir);
    CaptureConfig::load(path.as_deref())
}

/// Runs `future` to completion on a fresh, single-purpose `tokio::Runtime`.
///
/// Teaching note (why this is still "one runtime" — CLAUDE.md's binding
/// rule): the rule forbids a process running *two different async runtime
/// crates* at once (e.g. tokio alongside async-std) — never picking two
/// event loops that would each try to drive the same I/O. This function's
/// `Runtime` and iced's own internal `tokio` executor (used by `run_daemon`
/// below) never coexist in the same process: a CLI verb never starts the
/// iced event loop, and the daemon never calls this function. Each process
/// this binary can become has exactly one runtime for its entire life,
/// which is the rule's actual point — this repo's Cargo.toml essay on
/// `zbus` says as much: "the CLI-verb process ... will construct its own
/// `tokio::runtime::Runtime` directly ... still the *only* runtime in that
/// process."
fn run_async<F, T>(future: F) -> Result<T, CliRunError>
where
    F: Future<Output = Result<T, CliRunError>>,
{
    let runtime = tokio::runtime::Runtime::new().map_err(CliRunError::Runtime)?;
    runtime.block_on(future)
}

/// Connects to the session bus and makes sure a daemon is there to answer
/// — auto-spawning one, detached, and waiting briefly if
/// `io.saola.Capture1` is currently unowned (PLAN.md Stage 3;
/// `dbus::ensure_daemon_running`).
async fn connect_to_daemon() -> Result<zbus::Connection, CliRunError> {
    let connection = zbus::Connection::session().await?;
    dbus::ensure_daemon_running(&connection).await?;
    Ok(connection)
}

/// `shot`: resolve flags-over-config into a `CaptureOptions`, then either
/// call the daemon over D-Bus or (`--no-daemon`) run in-process.
///
/// Both branches end up in the same two library calls —
/// `capture::take_screenshot` then `storage::save_capture` — because the
/// daemon's `Screenshot` method calls exactly those too (`dbus.rs`). The
/// only differences are where they run and who owns the clipboard
/// afterwards; see `storage::ClipboardOwner`.
fn run_shot(config_dir: Option<&Path>, args: cli::ShotArgs) -> Result<String, CliRunError> {
    let config = load_config(config_dir);
    let options = cli::CaptureOptions::resolve(&config, &args)?;

    if options.no_daemon {
        return run_shot_in_process(&options);
    }

    run_async(async move {
        let connection = connect_to_daemon().await?;
        let proxy = dbus::Capture1Proxy::new(&connection).await?;
        let dbus_options = options.to_dbus_options();
        let path = proxy
            .screenshot(options.kind.as_str(), dbus_options)
            .await?;
        Ok(path)
    })
}

/// `shot --no-daemon`: the whole capture, in this process, with no D-Bus,
/// no iced, no surfaces (PLAN.md Stage 5, task 4 — "the same library calls,
/// no UI").
///
/// Deliberately **synchronous**: there is no async work here at all, so this
/// branch never builds the `tokio::Runtime` `run_async` would. That is the
/// scriptable path's whole selling point — a `shot --fullscreen --no-daemon`
/// is process start, one Wayland connection, one encode, one write.
///
/// The clipboard is the one thing that can't be finished in-process (see
/// `storage`'s module doc comment): this process is about to exit, so it
/// hands the selection to a detached helper instead of owning it.
fn run_shot_in_process(options: &cli::CaptureOptions) -> Result<String, CliRunError> {
    let backend = capture::screencopy::ScreencopyBackend::new();
    let frame = capture::take_screenshot(&backend, options)?;
    let saved = storage::save_capture(
        &frame,
        options,
        options.kind,
        storage::ClipboardOwner::DetachedHelper,
    )?;
    Ok(saved.path.display().to_string())
}

/// The hidden `clipboard-serve` verb (see `cli::Command::ClipboardServe`):
/// blocks serving the Wayland selection until something else claims it, so
/// it must not go through `report` — there is no path to print, and its
/// stdout is `/dev/null` anyway.
fn run_clipboard_serve(mime: &str) -> ExitCode {
    match storage::run_clipboard_serve(mime) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("saola-capture: clipboard-serve: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `record start|stop|toggle` — **all three real as of Stage 11**, for
/// fullscreen.
///
/// `toggle` reads the daemon's `Recording` property and picks a branch (see
/// `dbus::CaptureService::recording` for why that property was added rather
/// than inferring the answer from an error). There is a theoretical race —
/// the state could change between the read and the call — and it does not
/// matter: this is a keybind on a single-user desktop, and both losing
/// branches fail cleanly ("a recording is already in progress" / "nothing is
/// recording") rather than doing the wrong thing. A daemon too old to have
/// the property reads as "not recording", so a toggle degrades to a start,
/// which is the same behaviour Stage 10 shipped.
fn run_record(config_dir: Option<&Path>, args: cli::RecordArgs) -> Result<String, CliRunError> {
    let config = load_config(config_dir);
    let options = cli::RecordOptions::resolve(&config, &args)?;

    // **Stage 10.** `--dry-run` short-circuits before any daemon contact —
    // see `capture::screencast::dry_run`'s doc comment for why it runs
    // in-process rather than over the bus. It is checked here rather than
    // inside the `run_async` block below so that the auto-spawn in
    // `connect_to_daemon` never happens for a diagnostic that has nothing
    // to ask the daemon for.
    if options.dry_run {
        return run_record_dry_run(&config, &options);
    }

    run_async(async move {
        let connection = connect_to_daemon().await?;
        let proxy = dbus::Capture1Proxy::new(&connection).await?;

        let stop = match options.action {
            cli::RecordActionKind::Stop => true,
            cli::RecordActionKind::Start => false,
            // A daemon that doesn't serve the property (or a bus hiccup)
            // reads as "not recording" — degrade to a start rather than
            // failing the keybind outright.
            cli::RecordActionKind::Toggle => proxy.recording().await.unwrap_or(false),
        };

        if stop {
            // The saved path, printed to stdout by `report` — the same
            // contract `shot` has.
            return Ok(proxy.stop_recording().await?);
        }

        // **Stage 12.** `--region`/`--window` recording targets are real —
        // `options.kind` carries which one the CLI resolved
        // (`cli::resolve_record_kind`), the same way `shot`'s own
        // `options.kind.as_str()` call already does for `Screenshot`.
        proxy
            .start_recording(options.kind.as_str(), options.to_dbus_options())
            .await?;
        // **Stage 13.** The audio the recording *asked for*, not the audio it
        // got: `StartRecording` returns nothing, and a device that wasn't
        // there degrades daemon-side to video-only with a warning toast (see
        // `dbus::CaptureService::resolve_audio`, which documents why that
        // cannot be reported back through this call).
        Ok(format!(
            "recording started (preset={}, audio={}) — stop it with `saola-capture record stop`",
            options.preset,
            options.audio.map(|audio| audio.as_str()).unwrap_or("none"),
        ))
    })
}

/// How long `record start --dry-run` watches the stream. PLAN.md Stage 10
/// task 4 fixes this at five seconds: long enough that a 60 Hz cast shows a
/// stable cadence, short enough that a human runs it without hesitating.
const DRY_RUN_DURATION: Duration = Duration::from_secs(5);

/// `record start --dry-run` (PLAN.md Stage 10 task 4): negotiate a real
/// screencast against the focused output, log the negotiated SPA format and
/// the frame cadence for five seconds, write nothing, tear it all down.
///
/// Without `--window-id` the target is the focused monitor, resolved through
/// the **same** `CaptureBackend` a screenshot uses
/// (`ScreencopyBackend::focused_output`), so "which monitor does a bare
/// `record start` mean?" has exactly one answer in this codebase rather than
/// two that can drift. With `--window-id` it casts that window instead — see
/// `cli::RecordStartArgs::window_id` for why a dry run can do that when a
/// real recording (Stage 12) can't yet. Region recording is a monitor cast
/// cropped in ffmpeg (CAPTURE-RESEARCH D8), so it is not a target here and
/// never will be.
fn run_record_dry_run(
    config: &CaptureConfig,
    options: &cli::RecordOptions,
) -> Result<String, CliRunError> {
    use capture::screencast::{CastTarget, CursorMode};
    use capture::CaptureBackend;

    let target = match options.window_id {
        Some(id) => CastTarget::Window { id },
        None => {
            let backend = capture::screencopy::ScreencopyBackend::new();
            CastTarget::Monitor {
                connector: backend.focused_output()?.name,
            }
        }
    };
    let cursor = CursorMode::from_cursor_option(config.cursor);

    eprintln!(
        "saola-capture: dry run: casting {target} (cursor {cursor:?}, preset {} — the preset is \
         logged for completeness only; a dry run never starts an encoder)",
        options.preset
    );
    // **Stage 13.** Same "logged, not honoured" note for audio, and worth
    // saying out loud rather than silently ignoring: a dry run has no encoder,
    // so it has nowhere to put an audio track — `--audio` is resolved by the
    // *daemon* (`dbus::CaptureService::resolve_audio`), which a dry run never
    // contacts.
    if let Some(audio) = options.audio {
        eprintln!(
            "saola-capture: dry run: --audio {audio} is ignored — a dry run negotiates video only \
             and writes nothing"
        );
    }

    let report = run_async(async move {
        capture::screencast::dry_run(target, cursor, DRY_RUN_DURATION)
            .await
            .map_err(CliRunError::Cast)
    })?;

    Ok(report.to_string())
}

/// `pick-color`: call the daemon, format the RGB triple as a hex swatch.
/// The daemon's `PickColor` stub always errors today (Stage 16 wires it
/// to niri's `org.gnome.Shell.Screenshot.PickColor`), so the formatting
/// path below isn't reachable end to end yet — kept real (not a second
/// stub) so Stage 16 only has to delete the `Err` short-circuit in
/// `dbus.rs`, not write this conversion.
fn run_pick_color() -> Result<String, CliRunError> {
    run_async(async {
        let connection = connect_to_daemon().await?;
        let proxy = dbus::Capture1Proxy::new(&connection).await?;
        let (r, g, b) = proxy.pick_color().await?;
        Ok(rgb_to_hex(r, g, b))
    })
}

/// `(r, g, b)` in `0.0..=1.0` (matching `org.gnome.Shell.Screenshot.PickColor`'s
/// own return shape, per CAPTURE-RESEARCH) to `#RRGGBB`. Clamped before the
/// cast to `u8` — a value fractionally outside range (floating-point noise
/// at the 0.0/1.0 boundary) rounds to a valid byte instead of wrapping.
fn rgb_to_hex(r: f64, g: f64, b: f64) -> String {
    let byte = |c: f64| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", byte(r), byte(g), byte(b))
}

/// `open`: ask the daemon to raise the main window. `WindowAction::
/// dbus_mode(None)` (rather than a bare `"main"` literal) is the same
/// encoder `dbus.rs`'s `spawn_window_process` decodes on the daemon side —
/// the one real production call site for that mapping, now that Stage 9
/// makes `OpenWindow` real (the toast-click flow spawns the editor process
/// directly, without going through `OpenWindow` at all — see
/// `main.rs::spawn_editor`).
fn run_open() -> Result<String, CliRunError> {
    run_async(async {
        let connection = connect_to_daemon().await?;
        let proxy = dbus::Capture1Proxy::new(&connection).await?;
        proxy
            .open_window(&cli::WindowAction::dbus_mode(None))
            .await?;
        Ok("opened the app window".to_string())
    })
}

/// `window [edit <path>]`: the separate-process app window — **real as of
/// Stage 9** (`modules::app::run`, a plain `iced::application`; see that
/// module's doc comment for the process split from the daemon, the
/// hide-until-reply capture flow, and the `edit <path>` stub editor).
/// Blocks for the window's whole life, same as `run_daemon` blocks for the
/// daemon's.
fn run_window(action: Option<&cli::WindowAction>) -> ExitCode {
    let mode = modules::app::window_mode_from_action(action);
    match modules::app::run(mode) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("saola-capture: window: {err}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------
// The daemon: a surfaceless iced_layershell daemon (PLAN.md Stage 3, task
// 4). No layer-shell surface is ever mapped by this stage — Stages 5/6
// spawn the first ones (flash/toast/overlay) on demand.
// ---------------------------------------------------------------------

fn run_daemon() -> ExitCode {
    // The one `Theme` this process ever builds — read here, before the
    // `daemon(..)` builder call, purely so `default_font` (below) can be
    // computed from it. `Daemon::boot` builds its own clone independently
    // (`Theme::saola()`, via `#[derive(Default)]` — `saola_theme::Theme`'s
    // own `Default` equals `saola()`, so the struct-level derive already
    // does the right thing without a boot-closure argument like the
    // panel's `Panel::new` needs); this crate has no config knob that
    // overrides palette colors the way `panel.kdl`'s `colors { }` does, so
    // there is nothing to thread between the two beyond staying in sync by
    // construction.
    let theme = Theme::saola();
    let default_font = saola_theme::convert::ui_font(&theme);

    let result = daemon(Daemon::boot, "saola-capture", Daemon::update, Daemon::view)
        .subscription(Daemon::subscription)
        .theme(Daemon::theme)
        // Transparent app-wide background — see `Daemon::style`'s doc
        // comment for the live-verified bug this fixes (an opaque ink
        // rectangle covering the whole output without it).
        .style(Daemon::style)
        .settings(Settings {
            // See `saola-panel::main`'s comment on this exact field
            // ordering: `default_font` must live *inside* this literal,
            // never behind a separate `.default_font(..)` builder call
            // before `.settings(..)` — that call's effect is clobbered by
            // the `..Default::default()` in whichever literal actually
            // lands last, and this one lands last.
            default_font,
            layer_settings: LayerShellSettings {
                // None of these matter for a `Background`-mode surface —
                // it is a bare `wl_surface` with no shell role at all
                // (Stage 2's finding, folded into the Stage 3 handoff), so
                // there is no anchor to stick to, no layer to draw on, and
                // nothing to grab keyboard focus. They're spelled out
                // (rather than `..LayerShellSettings::default()`) so a
                // future change here can't quietly inherit a value nobody
                // chose on purpose.
                anchor: Anchor::empty(),
                layer: Layer::Background,
                exclusive_zone: 0,
                size: None,
                margin: (0, 0, 0, 0),
                keyboard_interactivity: KeyboardInteractivity::None,
                events_transparent: true,
                // The one field that *does* matter: `Background` is what
                // keeps the event loop alive with zero real surfaces
                // mapped (`layershellev`'s own run loop only stops itself
                // when `units.is_empty() && !is_allscreens() &&
                // !is_background()` — this is the whole reason Stage 2/3
                // chose this mode over `Active`). Stages 5/6 spawn real
                // surfaces later via `NewLayerShellSettings` +
                // `OutputOption::OutputName`, which is unrelated to this
                // boot-time background surface and doesn't replace it.
                start_mode: StartMode::Background,
            },
            ..Default::default()
        })
        .run();

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("saola-capture: daemon: {err}");
            ExitCode::FAILURE
        }
    }
}

/// PLAN.md Stage 6, task 4: `docs/SAOLA-STYLE-GUIDE.md` §11's checklist,
/// walked for both surfaces this stage adds. Ten items, in order:
///
/// **The flash** (`modules::flash`):
/// 1. Ink or ivory? Neither, exactly — it *is* `palette.paper` (ivory) at a
///    fading opacity, which is the point: a camera flash reads as a wash of
///    light, not a shell-chrome surface.
/// 2. Terracotta element? None, and correctly so — the flash has zero
///    interactive elements to accent.
///
/// Items 3–7 (controls/text/corners/serif/icons): no controls, no text, no
/// corners (full-bleed, un-rounded — a scrim, not a card), no serif, no
/// icons. All vacuously satisfied.
///
/// 8. Animates — is it one of the five sanctioned exceptions (notification,
///    popover, hover, breathing status dot, opt-in marquee)? **Honestly,
///    no** — a shutter flash isn't named in §5's list, because the list
///    predates this app. PLAN.md's own Stage 6 task ("a ~150 ms ivory
///    fade") specifies it anyway; recorded here as a deliberate,
///    spec-directed addition to the animation set for *this app only*,
///    not a violation to silently wave through.
/// 9. N/A — not a popover.
/// 10. Added a colour? No — `palette.paper` only, one of the three.
///
/// **The toast** (`modules::toast`):
/// 1. Ink — §6 verbatim (`ink_card_style`; see that function's doc comment
///    for the `saola_theme::style::container::card` gap this uncovered).
/// 2. Terracotta element — the life rule, and it *is* the live one (a
///    real-time countdown), matching the checklist's "is it the live one"
///    test exactly.
/// 3. N/A as stated — there's no button chrome on a toast card, the whole
///    card is the click target (`mouse_area`), ink fill with ivory text
///    per §6's own spec rather than the general at-rest-ivory control rule.
/// 4. Title `typography.size.body` (13.5, Sans 500) ≥ 13px ✓; app name and
///    body sit at `size.meta`/`size.secondary` (12/12.5, Sans 400), which
///    is §3's own Metadata/Secondary rows, not the panel-bar floor — the
///    13px hard minimum is scoped to "panel and bar text" and a
///    notification card is neither. No countable readout on the card, so
///    no tabular-numeral question arises.
/// 5. `radii.card` (26px) ✓.
/// 6. Zero serif — Sans throughout, matching §3's own notification-card row.
/// 7. **Deviation, deliberate**: no Lucide glyph. §6 specifies "36px icon
///    tile (ivory, ink glyph)" for a generic notification; this app's
///    notification already has something better to show in that tile — the
///    screenshot's own thumbnail (`modules::toast::thumbnail_handle`) —
///    so a generic glyph would be strictly less useful. Recorded as an
///    intentional substitution, not an oversight.
/// 8. Animates — **yes**, and this one *is* named: §5's own motion table
///    lists "Notification popup" as a sanctioned timed animation.
/// 9. N/A — not a popover (no "only one at a time" rule; §6's own "stack of
///    three, fourth replaces oldest" is the toast's actual sibling rule,
///    and `ToastStack::push` implements exactly that).
/// 10. Added a colour? No — ink, ivory, terracotta only. The thumbnail's
///     own pixels are user content (the screenshot itself), not a
///     design-system colour choice — exempt for the same reason an avatar
///     photo is exempt in `saola-lockscreen::modules::reveal`.
///
/// **The region overlay** (`modules::overlay`, PLAN.md Stage 7, task 1),
/// walked the same way:
/// 1. Neither ink nor ivory as a *ground* — the ground is the frozen
///    screenshot itself, dimmed by `scrim.capture` (§2's own "Capture
///    overlay (outside selection)" row, the one scrim in the table written
///    for exactly this surface). Its chrome — toolbar, readout — is ink.
/// 2. Exactly one terracotta element: **the selection**, meaning its dashed
///    edge and its eight handles together (§7 names both in one breath). It
///    is unambiguously the live one. The toolbar's Capture button is
///    deliberately *not* terracotta despite being the primary action — see
///    `modules::overlay::toolbar`'s doc comment.
/// 3. Every control at rest is ivory (`style::button::rest`, ink label).
/// 4. Toolbar labels and the size readout are `typography.size.body` (13.5,
///    Sans 500) ≥ 13px ✓, and the readout counts, so it takes tabular
///    numerals — IBM Plex's default figures, via `convert::ui_font`.
/// 5. Corners: `radii.selection` (6px) on the selection rect — §4's own
///    "Capture selection" row, and the deliberate exception to the ≥18px
///    rule, since a 6px radius is what that row specifies. The toolbar is
///    `radii.popover` (30) and the readout `radii.pill`.
/// 6. Zero serif.
/// 7. No icons at all: the toolbar is text pills (§6's pill button), so the
///    Lucide/stroke-2.75 question is vacuous. Recorded as a deliberate
///    choice — `src/icons.rs` still does not exist in this repo, and
///    inventing an icon set for four buttons that read perfectly well as
///    words would be the wrong first reason to add one.
/// 8. Animates? **No.** Nothing on this surface moves on a timer; the
///    selection follows the pointer, which is direct manipulation, not
///    animation. It is the first Saola surface with *no* motion at all.
/// 9. N/A — not a popover.
/// 10. Added a colour? No — ink, ivory, terracotta. The frozen frame's own
///     pixels are user content, exempt on the same grounds as the toast's
///     thumbnail.
///
/// Registry of live layer-shell surfaces, keyed by iced's `window::Id`, and
/// what each one is *for* — the shape `saola-panel::main::SurfaceRole`
/// established (PLAN.md Stage 3, task 4: "the SurfaceRole registry in
/// place"). Stage 6 gave it its first two real variants; Stage 7 adds the
/// third (the region-selection overlay).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceRole {
    /// The full-output camera-flash fade (`modules::flash`). **Spawned once,
    /// at boot** (`Daemon::boot`), and kept mapped for the daemon's whole
    /// life — see `Daemon::boot`'s doc comment for why a per-capture
    /// spawn/unmap (Stage 6's first draft) turned out to be the wrong shape.
    /// Idle, it renders fully transparent (`Flash::opacity` is `0.0`) and is
    /// click-through (`events_transparent: true`), so a permanently-mapped
    /// surface costs nothing visible or interactive between screenshots.
    Flash,
    /// The notification stack (`modules::toast`). Mapped by
    /// [`Daemon::sync_toast_surface`] on the first toast, resized (by
    /// unmap-then-respawn — see that method's doc comment) whenever the
    /// card count changes, and unmapped once the last toast expires.
    Toast,
    /// The region-selection overlay (`modules::overlay`). **Spawned
    /// reactively and torn down the moment the user acts** — a third
    /// lifecycle, distinct from both of the above, and the only one it
    /// could have: it needs `KeyboardInteractivity::Exclusive` from the
    /// moment it maps, so it cannot be pre-warmed at boot the way the flash
    /// is (an always-mapped exclusive-keyboard surface would hold the
    /// keyboard forever), and it has no size that depends on its content,
    /// so it never needs the toast's respawn-to-resize dance. See
    /// [`Daemon::begin_region`] for the surface-latency risk this shape
    /// inherits from Stage 6's first flash draft, and why it is survivable
    /// here.
    Overlay,
    /// The delayed-capture countdown pill (`modules::countdown`, PLAN.md
    /// Stage 8). Reactive like the overlay (spawned on the first delayed
    /// shot, torn down the instant the countdown reaches zero — see
    /// [`Daemon::sync_countdown_surface`]) but with neither of the
    /// overlay's two reasons to be reactive instead of boot-spawned: no
    /// keyboard interactivity at all (`KeyboardInteractivity::None`, same
    /// as the flash) and, unlike the flash, real content that changes over
    /// its own lifetime, so there is no idle state worth pre-warming.
    /// `modules::countdown`'s own doc comment has the full reasoning for
    /// why the flash's boot-time trick neither applies nor is needed here.
    Countdown,
}

/// The daemon's whole state.
#[derive(Debug, Default)]
struct Daemon {
    windows: HashMap<window::Id, SurfaceRole>,
    /// The Saola theme — every color/size the flash and toast surfaces
    /// draw comes from here. `#[derive(Default)]` on this struct already
    /// does the right thing (`saola_theme::Theme`'s own `Default` equals
    /// `Theme::saola()`), so `Daemon::boot` doesn't need to construct it
    /// by hand — see `run_daemon`'s comment on why the *other* `Theme`
    /// this process builds (for `default_font`, before the daemon exists)
    /// doesn't need to be threaded in here either.
    theme: Theme,
    flash: modules::flash::Flash,
    toasts: modules::toast::ToastStack,
    /// The toast surface's Id, while one is mapped.
    toast_surface: Option<window::Id>,
    /// How many cards the *currently mapped* toast surface was sized for.
    /// Compared against `toasts.len()` on every sync so a card being added
    /// or expiring (which changes the surface's declared height — see
    /// `toast_surface_settings`) is noticed even though the surface's own
    /// Id doesn't change on its own. See `Daemon::sync_toast_surface`.
    toast_surface_count: usize,
    /// The region-selection overlay's state, while one is up (Stage 7).
    /// `Some` here is the daemon's "a selection is in progress" flag: a
    /// second `shot --region` arriving now is answered
    /// `RegionOutcome::Unavailable` rather than stacking a second
    /// exclusive-keyboard surface on top of the first.
    overlay: Option<modules::overlay::Overlay>,
    /// The overlay surface's Id, while one is mapped. Tracked separately
    /// from `overlay` (rather than read back out of `windows`) for the same
    /// reason `toast_surface` is: `remove_surface` needs the Id, and
    /// scanning the registry for "the one with role Overlay" would be a
    /// worse way to ask the same question.
    overlay_surface: Option<window::Id>,
    /// Where the answer goes when the user finishes — the reply half of
    /// `dbus::DaemonEvent::BeginRegion`, held for exactly as long as the
    /// overlay is up.
    overlay_reply: Option<mpsc::Sender<dbus::RegionOutcome>>,
    /// The delayed-capture countdown's state (Stage 8). `Countdown::
    /// is_active` (not a separate `Option`, unlike `overlay`) is what
    /// decides whether [`Self::countdown_surface`] should be mapped —
    /// `#[derive(Default)]` already gives a countdown that is never active,
    /// which is the correct idle state.
    countdown: modules::countdown::Countdown,
    /// The countdown surface's Id, while one is mapped. Same role
    /// `overlay_surface`/`toast_surface` play for their own surfaces.
    countdown_surface: Option<window::Id>,
}

impl Daemon {
    /// `iced_layershell::build_pattern::daemon`'s boot closure: builds the
    /// default state, then spawns the flash surface immediately — the same
    /// `(State, Task<Message>)` boot-time-surface shape
    /// `saola-panel::main::Panel::spawn_boot_surfaces` uses for its own
    /// always-on surfaces.
    ///
    /// **Why the flash is pre-warmed instead of spawned per capture (a
    /// finding from this stage's own nested-niri live check, not a
    /// hypothetical):** the first draft spawned a fresh layer-shell surface
    /// on every `CaptureTaken` and tore it down once
    /// [`modules::flash::Flash::is_active`] went false — mirroring how the
    /// toast surface still works. Live-tested against the nested niri
    /// instance (`niri msg layers` immediately after triggering `shot
    /// --fullscreen`, plus repeated `grim` captures timed against the
    /// trigger), the *toast* reliably appeared — it stays mapped for up to
    /// 6.35 s, plenty of time to absorb a first surface's Wayland
    /// configure/ack_configure round trip and its first GPU frame — but the
    /// *flash*, whose entire budget is `modules::flash::fade`'s ~140 ms,
    /// never once rendered a visible frame in ten consecutive `grim`
    /// captures taken immediately after a completed `shot`. The chain from
    /// "`CaptureService::screenshot` sends a `DaemonEvent`" to "a pixel is
    /// on screen" crosses several independent scheduler hops (the events
    /// channel, `dbus_worker_stream`'s forward, iced's own message queue,
    /// `NewLayerShell`, the compositor's configure round trip, first
    /// `wgpu` frame) — on this test machine that chain alone eats a
    /// meaningful fraction of 140 ms before a single pixel is composited,
    /// so a surface created *after* the trigger can lose the entire fade
    /// window to setup latency and never be seen at all.
    ///
    /// Spawning the surface once, at boot, removes surface-creation latency
    /// from every capture after the first: by the time any screenshot ever
    /// happens, the flash surface has already existed (and almost
    /// certainly already rendered at least one transparent frame) for the
    /// daemon's entire uptime, so `Flash::trigger` only has to change an
    /// *already-mapped* surface's opacity, not create one from scratch.
    /// It's click-through and fully transparent at rest
    /// (`flash_surface_settings`), so a surface that outlives every
    /// individual flash costs nothing between screenshots.
    fn boot() -> (Self, Task<Message>) {
        let mut daemon = Self::default();
        let (_id, task) = daemon.spawn_surface(SurfaceRole::Flash, flash_surface_settings());
        (daemon, task)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Shutdown(reason) => {
                reason.log();
                // `iced::exit()` is `iced_runtime::exit()` re-exported —
                // it produces `Action::Exit`, which `iced_layershell`'s
                // event loop turns into `ReturnData::RequestExit` and
                // stops the loop. `run_daemon`'s `.run()` call then
                // returns `Ok(())`, so the process exits 0 — the "exits
                // cleanly" CLAUDE.md's single-instance rule asks for,
                // reused here for the signal-shutdown case too.
                iced::exit()
            }
            // The PrintScr flow's tail (PLAN.md Stage 6, task 3): a
            // screenshot was just saved (`dbus.rs`'s `CaptureService::
            // screenshot` already emitted the `CaptureTaken` D-Bus signal
            // and printed the path back to the caller — this is purely the
            // in-process flash+toast reaction on top of that). The flash
            // surface already exists (`Daemon::boot`), so triggering it is
            // just a state change — no `Task` of its own, unlike the toast
            // (which still spawns/resizes reactively; see
            // `sync_toast_surface`'s doc comment for why the two surfaces
            // don't share that lifecycle).
            Message::CaptureTaken { path, thumbnail } => {
                let now = Instant::now();
                self.flash.trigger(now);
                self.toasts
                    .push(PathBuf::from(path), thumbnail.0, &self.theme, now);
                self.sync_toast_surface()
            }
            // A fade tick changes nothing in `self` — `Flash::view` reads
            // `Instant::now()` itself on every render (the same "Tick's
            // only job is to wake rendering" shape `modules::flash::
            // Message::Tick`'s own doc comment describes) — so this falls
            // through to the wildcard `Task::none()` arm below rather than
            // getting one of its own; it's called out here only so a
            // reader grepping for `Message::Flash` finds this note instead
            // of concluding the variant is unhandled.
            Message::Toast(inner) => {
                let now = Instant::now();
                let action = self.toasts.update(inner, now, &self.theme);
                match action {
                    modules::toast::Action::Open(path) => {
                        if let Err(err) = spawn_editor(&path) {
                            eprintln!(
                                "saola-capture: daemon: could not open the editor for {}: {err}",
                                path.display()
                            );
                        }
                    }
                    // Stage 12: a recording's toast — "videos open
                    // containing dir for now" (PLAN.md task 3; the editor
                    // has no video support at all yet, so there is nothing
                    // for `spawn_editor` to do with this path).
                    modules::toast::Action::OpenDir(path) => {
                        if let Err(err) = open_containing_dir(&path) {
                            eprintln!(
                                "saola-capture: daemon: could not open the folder containing {}: \
                                 {err}",
                                path.display()
                            );
                        }
                    }
                    modules::toast::Action::None => {}
                }
                self.sync_toast_surface()
            }
            // Stage 11: a recording died. The same toast machinery a saved
            // screenshot uses, with a message instead of a file — see
            // `modules::toast::ToastKind::Notice`. Nobody is watching a
            // terminal when a keybind started the recording, which is the
            // whole reason this is a surface and not just a log line.
            Message::RecordingFailed(message) => {
                self.toasts
                    .push_notice("Recording failed", message, &self.theme, Instant::now());
                self.sync_toast_surface()
            }
            // Stage 12: the success half — `RecordingFailed`'s sibling.
            // Clicking it opens the containing directory (`modules::toast::
            // ToastKind::Recording`), the same "no video editor yet" answer
            // the module doc comment above explains.
            Message::RecordingFinished(path) => {
                self.toasts
                    .push_recording(PathBuf::from(path), &self.theme, Instant::now());
                self.sync_toast_surface()
            }
            // Stage 13: a recording that started, but not with the audio it
            // was asked for. The same notice card as a failure, because the
            // style guide carries severity in the *wording* and never in a
            // colour (CLAUDE.md's Design language, "three colors, never a
            // fourth") — so there is nothing else for a warning to look like.
            Message::Warning { title, body } => {
                self.toasts
                    .push_notice(title, body, &self.theme, Instant::now());
                self.sync_toast_surface()
            }
            // Stage 8's delayed-capture countdown pill.
            Message::CountdownStarted(seconds) => {
                self.countdown.trigger(
                    std::time::Duration::from_secs(u64::from(seconds)),
                    Instant::now(),
                );
                self.sync_countdown_surface()
            }
            // A tick changes nothing in `self.countdown` (the countdown's
            // own `Instant` is fixed at trigger time — see
            // `modules::countdown::Countdown::trigger`); its only job is to
            // wake rendering and let this arm re-check whether the
            // countdown surface should still be mapped, the same shape
            // `Message::Toast`'s arm re-checks the toast surface on every
            // tick.
            Message::Countdown(_inner) => self.sync_countdown_surface(),
            // Stage 7's region flow, both halves.
            Message::BeginRegion(request) => self.begin_region(request),
            Message::OverlayEvent {
                id,
                event,
                captured,
            } => {
                if self.overlay_surface != Some(id) {
                    // An event from the toast surface (or from a surface
                    // that has already been torn down) must never be read as
                    // an overlay drag: pointer coordinates are
                    // surface-relative, so a click on a toast card would
                    // otherwise land somewhere arbitrary inside the
                    // selection.
                    return Task::none();
                }
                match modules::overlay::message_from_event(&event, captured) {
                    Some(message) => self.update_overlay(message),
                    None => Task::none(),
                }
            }
            Message::Overlay(message) => self.update_overlay(message),
            // The macro-injected layer-shell control variants (see
            // `Message`'s doc comment) never reach here — this is the
            // same catch-all `saola-panel::main::Panel::update` ends with,
            // for the same reason.
            _ => Task::none(),
        }
    }

    /// Every `id` this is called with is either a surface this daemon
    /// spawned itself (registered synchronously in [`Self::spawn_surface`]
    /// — see that method's doc comment for why there's no window between
    /// "asked for a surface" and "know which surface it is") or the boot
    /// surface from `run_daemon`'s `Settings` (surfaceless, `Background`
    /// mode, never registered — see `LayerShellSettings`'s `start_mode`
    /// comment), which falls through to the empty `None` arm exactly as
    /// Stage 3 left it.
    fn view(&self, id: window::Id) -> Element<'_, Message> {
        match self.windows.get(&id) {
            Some(SurfaceRole::Flash) => self
                .flash
                .view(
                    &self.theme,
                    Instant::now(),
                    modules::flash::fade(&self.theme),
                )
                .map(Message::Flash),
            Some(SurfaceRole::Toast) => self
                .toasts
                .view(&self.theme, Instant::now())
                .map(Message::Toast),
            // The overlay surface can outlive its state by one frame: the
            // `RemoveWindow` task and the `self.overlay = None` that
            // accompanies it are processed by the runtime in that order, so
            // a redraw in between must render *something*. An empty
            // `Space` is the same answer the unregistered-Id arm gives.
            Some(SurfaceRole::Overlay) => match &self.overlay {
                Some(overlay) => overlay.view(&self.theme).map(Message::Overlay),
                None => Space::new().into(),
            },
            Some(SurfaceRole::Countdown) => self
                .countdown
                .view(&self.theme, Instant::now())
                .map(Message::Countdown),
            None => Space::new().into(),
        }
    }

    /// Independent workers, batched: the D-Bus service (which owns
    /// the bus name for the daemon's whole life, or reports why it
    /// couldn't), the SIGTERM/SIGINT wait, and — new in Stage 6, joined by
    /// the countdown in Stage 8 — the flash/toast/countdown animation
    /// ticks, each gated to run only while its
    /// surface actually needs to redraw (see
    /// `modules::flash::Flash::subscription` / `modules::toast::ToastStack::
    /// subscription` / `modules::countdown::Countdown::subscription`), so an
    /// idle daemon between screenshots burns zero
    /// extra timer wakeups. `Instant::now()` is read once per subscription
    /// rebuild (cheap, and iced only rebuilds this when `Daemon`'s state
    /// actually changed) rather than threaded in — see `modules::flash`'s
    /// module doc comment on why the *modules* themselves never read the
    /// clock, which this call site is the one sanctioned exception to.
    fn subscription(&self) -> Subscription<Message> {
        let now = Instant::now();
        Subscription::batch([
            Subscription::run(dbus_worker_stream),
            Subscription::run(shutdown_signal_stream),
            self.flash
                .subscription(now, modules::flash::fade(&self.theme))
                .map(Message::Flash),
            self.toasts.subscription().map(Message::Toast),
            self.countdown.subscription(now).map(Message::Countdown),
            // Raw input, **only while a selection is in progress**. Gated
            // for the same reason the flash/toast ticks are: this is the
            // one subscription in the daemon that would otherwise deliver a
            // message for every pointer motion anywhere on the desktop, all
            // day, to a daemon that spends almost all of its life idle. The
            // gate closes again the instant `self.overlay` goes back to
            // `None` (iced rebuilds subscriptions whenever state changes).
            if self.overlay.is_some() {
                overlay_event_subscription()
            } else {
                Subscription::none()
            },
        ])
    }

    /// `saola_theme::to_iced_theme` for whichever surface `_id` is —
    /// there's only one `Theme` in this daemon (no per-surface palette),
    /// so every Id gets the same answer. Wired via `.theme(Daemon::theme)`
    /// in `run_daemon`, mirroring `saola-panel::main::Panel::theme`.
    fn theme(&self, _id: window::Id) -> iced::Theme {
        saola_theme::to_iced_theme(&self.theme)
    }

    /// The app-wide surface background — **must** be transparent, copied
    /// verbatim from `saola-panel::main::Panel::style`. Live-verified the
    /// hard way (nested niri, this stage): without this, iced clears every
    /// surface to `to_iced_theme`'s `background` (`palette.ink`) before
    /// drawing anything, so the flash surface's own semi-transparent ivory
    /// container — correct in isolation — was being alpha-blended over an
    /// *opaque* ink base rather than true Wayland transparency. At rest
    /// (opacity `0.0`) that composited to solid ink, which was invisible
    /// while the flash surface was still spawned-and-torn-down per capture
    /// (Stage 6's first draft — gone again before anyone looked), but
    /// became a permanent ink rectangle covering the whole output the
    /// moment `Daemon::boot` started keeping the surface mapped forever
    /// (this same stage's fix for the *other* flash bug — see `Daemon::
    /// boot`'s doc comment). Caught by `grim` sampling a pixel a full
    /// second after a capture, when the flash should long since have faded
    /// back to nothing. Wired via `.style(Daemon::style)` in `run_daemon`.
    fn style(&self, theme: &iced::Theme) -> iced::theme::Style {
        iced::theme::Style {
            background_color: iced::Color::TRANSPARENT,
            ..iced::theme::default(theme)
        }
    }

    /// Ask the compositor for a new layer-shell surface in the given
    /// `role`, and register the role against the Id the surface will have
    /// — copied verbatim from `saola-panel::main::Panel::spawn_surface`
    /// (see that method's doc comment for the full teaching note on why
    /// `Message::layershell_open` mints the Id itself, closing the window
    /// in which `view` could be called with an Id this registry can't
    /// classify).
    fn spawn_surface(
        &mut self,
        role: SurfaceRole,
        settings: NewLayerShellSettings,
    ) -> (window::Id, Task<Message>) {
        let (id, task) = Message::layershell_open(settings);
        self.windows.insert(id, role);
        (id, task)
    }

    /// Ask the compositor to destroy the surface identified by `id`, and
    /// forget its role — copied verbatim from `saola-panel::main::Panel::
    /// remove_surface`.
    fn remove_surface(&mut self, id: window::Id) -> Task<Message> {
        self.windows.remove(&id);
        Task::done(Message::RemoveWindow(id))
    }

    /// Map, resize, or unmap the toast surface to match
    /// `self.toasts.len()`.
    ///
    /// **Resizing is unmap-then-respawn, not a live `SizeChange`.** A
    /// layer-shell surface with `events_transparent: false` (the toast
    /// surface — it has to be clickable) takes pointer input across its
    /// *entire* declared area, reserved or not (the same all-or-nothing
    /// input-region constraint `saola-panel::main::IslandKind`'s doc
    /// comment discovered the hard way). If the surface stayed sized for
    /// three cards while only one was showing, the blank space below that
    /// one card would silently swallow clicks meant for whatever window is
    /// underneath it — every time the toast stack isn't full, which is
    /// most of the time. Respawning at exactly the height
    /// `modules::toast::card_stack_height` wants for the
    /// *current* count keeps the surface's clickable footprint always
    /// matching what's actually drawn on it. The toast has no keyboard
    /// focus and no cross-frame animation state that a fresh surface would
    /// lose (its `Instant`s live on `Daemon`, not the surface), so the
    /// respawn is invisible to the user.
    fn sync_toast_surface(&mut self) -> Task<Message> {
        let needed = self.toasts.len();
        match (self.toast_surface, needed) {
            (None, 0) => Task::none(),
            (None, _) => {
                let (id, task) = self.spawn_surface(
                    SurfaceRole::Toast,
                    toast_surface_settings(&self.theme, needed),
                );
                self.toast_surface = Some(id);
                self.toast_surface_count = needed;
                task
            }
            (Some(id), 0) => {
                self.toast_surface = None;
                self.toast_surface_count = 0;
                self.remove_surface(id)
            }
            (Some(id), n) if n != self.toast_surface_count => {
                self.toast_surface_count = n;
                let remove = self.remove_surface(id);
                let (new_id, spawn) =
                    self.spawn_surface(SurfaceRole::Toast, toast_surface_settings(&self.theme, n));
                self.toast_surface = Some(new_id);
                Task::batch([remove, spawn])
            }
            // Already mapped at the right size.
            _ => Task::none(),
        }
    }

    /// Map or unmap the delayed-capture countdown surface to match
    /// `self.countdown.is_active(..)` (PLAN.md Stage 8, task 2).
    ///
    /// Simpler than [`Self::sync_toast_surface`] on purpose: the countdown
    /// pill's size never changes over its own lifetime (unlike the toast's
    /// card count), so there is only ever a map/unmap decision here, never
    /// a resize. Called from both the trigger (`Message::CountdownStarted`)
    /// and every subsequent tick (`Message::Countdown`) — the tick side is
    /// what notices "the countdown just reached zero" and tears the surface
    /// back down, since nothing else in this daemon watches the countdown's
    /// own clock.
    fn sync_countdown_surface(&mut self) -> Task<Message> {
        let active = self.countdown.is_active(Instant::now());
        match (self.countdown_surface, active) {
            (None, true) => {
                let (id, task) =
                    self.spawn_surface(SurfaceRole::Countdown, countdown_surface_settings());
                self.countdown_surface = Some(id);
                task
            }
            (Some(id), false) => {
                self.countdown_surface = None;
                self.remove_surface(id)
            }
            // Already mapped-and-counting, or already unmapped-and-idle.
            _ => Task::none(),
        }
    }

    /// Map the region-selection overlay over a frozen frame (PLAN.md Stage
    /// 7, task 1) — the daemon's half of `dbus::CaptureService::
    /// interactive_region`.
    ///
    /// **One selection at a time.** A second request while an overlay is up
    /// is refused immediately with `RegionOutcome::Unavailable` rather than
    /// queued or stacked: two surfaces both holding
    /// `KeyboardInteractivity::Exclusive` is a state CAPTURE-RESEARCH §6.6
    /// explicitly flags as unspecified by the protocol, and "the second
    /// `Print` press does nothing visible but returns a clear error" is a far
    /// better failure than "the keyboard is now owned by an invisible
    /// surface".
    ///
    /// **On surface-creation latency** (Stage 6's bug #1, which this shape
    /// re-exposes): the overlay *is* spawned reactively, exactly like the
    /// flash draft that lost its whole visible window to Wayland/GPU setup
    /// latency. The difference that makes it survivable is lifetime — the
    /// flash's entire existence was ~140 ms, so latency ate the whole thing,
    /// while the overlay stays mapped until the user acts. Latency here
    /// delays the moment the overlay becomes visible/interactive; it cannot
    /// make the overlay never appear. It is still the first thing to suspect
    /// if a region shot feels sluggish — see the Stage 7 handoff for how it
    /// was measured live.
    fn begin_region(&mut self, request: RegionRequest) -> Task<Message> {
        if self.overlay.is_some() {
            let mut reply = request.reply;
            if reply
                .try_send(dbus::RegionOutcome::Unavailable(
                    "a region selection is already in progress",
                ))
                .is_err()
            {
                eprintln!(
                    "saola-capture: daemon: could not refuse a second region selection — the \
                     caller is gone"
                );
            }
            return Task::none();
        }

        let settings = overlay_surface_settings(&request.output.name);
        let overlay =
            modules::overlay::Overlay::new(request.frame, request.output, request.focused_window);
        self.overlay = Some(overlay);
        self.overlay_reply = Some(request.reply);
        let (id, task) = self.spawn_surface(SurfaceRole::Overlay, settings);
        self.overlay_surface = Some(id);
        task
    }

    /// Fold one overlay message in, and act on whatever it decides.
    fn update_overlay(&mut self, message: modules::overlay::Message) -> Task<Message> {
        let Some(overlay) = self.overlay.as_mut() else {
            return Task::none();
        };
        match overlay.update(message) {
            modules::overlay::Action::None => Task::none(),
            modules::overlay::Action::Cancel => self.finish_overlay(dbus::RegionOutcome::Cancelled),
            modules::overlay::Action::Confirm(region) => {
                self.finish_overlay(dbus::RegionOutcome::Selected(region))
            }
            // Stage 8: the toolbar's Window button.
            modules::overlay::Action::ConfirmWindow(window) => {
                self.finish_overlay(dbus::RegionOutcome::SelectedWindow(window))
            }
        }
    }

    /// Answer the waiting D-Bus call and tear the overlay down.
    ///
    /// **Order matters, and it is: reply, then unmap.** The `try_send`
    /// happens before the `RemoveWindow` task is even returned, so the
    /// blocking crop-and-save on the other side starts while this surface is
    /// still being torn down rather than after. The flash and toast that
    /// follow are then landing on a screen the overlay has already left —
    /// which is what makes the flash read as "the shutter fired", instead of
    /// firing behind a scrim.
    fn finish_overlay(&mut self, outcome: dbus::RegionOutcome) -> Task<Message> {
        if let Some(mut reply) = self.overlay_reply.take() {
            if reply.try_send(outcome).is_err() {
                eprintln!(
                    "saola-capture: daemon: the region selection finished but nobody was \
                     waiting for it (the caller gave up or exited)"
                );
            }
        }
        self.overlay = None;
        match self.overlay_surface.take() {
            Some(id) => self.remove_surface(id),
            None => Task::none(),
        }
    }
}

/// The flash surface's layer-shell settings: the whole output, click
/// through, no keyboard, no reservation. `Layer::Overlay` — the same layer
/// `saola-panel::popover.rs` uses for "must sit above everything else,
/// including a fullscreen window" — so the flash is visible over whatever
/// was just captured. Single-output only (no `output_option` override, so
/// this targets the currently active output like the boot surface does);
/// CLAUDE.md's Architecture section already documents this as an
/// acceptable v0.1 limitation shared with the not-yet-built region overlay.
///
/// Called exactly once, from `Daemon::boot` — see that method's doc comment
/// for the live nested-niri finding that moved the flash surface's spawn
/// from "on the first `CaptureTaken`" (this stage's first draft) to boot
/// time: a surface created fresh per capture can lose its entire ~140 ms
/// fade window to Wayland/GPU setup latency before ever compositing a
/// pixel, which is exactly what was observed (ten consecutive `grim`
/// captures immediately after a completed `shot --fullscreen`, none showing
/// the flash) until the surface was made permanent.
fn flash_surface_settings() -> NewLayerShellSettings {
    NewLayerShellSettings {
        anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
        layer: Layer::Overlay,
        // `(0, 0)` stretches to fill — legal because all four edges are
        // anchored (`SurfaceGeometry`'s own doc comment in the panel
        // explains the same rule for its one-axis case).
        size: Some((0, 0)),
        margin: Some((0, 0, 0, 0)),
        exclusive_zone: Some(0),
        keyboard_interactivity: KeyboardInteractivity::None,
        events_transparent: true,
        ..Default::default()
    }
}

/// The toast surface's layer-shell settings, sized for `count` cards (see
/// `modules::toast::card_stack_height`). Anchored top-right with
/// the same vertical offset (`sizes.popover_top`) `saola-panel`'s popover
/// uses below its own bar, so a toast never collides with wherever the
/// panel puts itself; `sizes.island_gap` insets it from the right edge by
/// the same modest amount islands use between each other. `exclusive_zone:
/// 0` mirrors the popover's own choice — reserve nothing, but let the
/// compositor keep the surface out of anyone else's reserved strip.
fn toast_surface_settings(theme: &Theme, count: usize) -> NewLayerShellSettings {
    let width = theme.sizes.notification_card_width.round() as u32;
    let height = modules::toast::card_stack_height(theme, count);
    let top = theme.sizes.popover_top.round() as i32;
    let right = theme.sizes.island_gap.round() as i32;

    NewLayerShellSettings {
        anchor: Anchor::Top | Anchor::Right,
        layer: Layer::Overlay,
        size: Some((width, height)),
        margin: Some((top, right, 0, 0)),
        exclusive_zone: Some(0),
        keyboard_interactivity: KeyboardInteractivity::None,
        // Unlike the flash, the toast has to receive clicks/hover — see
        // `Daemon::sync_toast_surface`'s doc comment for how the surface
        // is kept sized so this doesn't swallow clicks meant for anything
        // else.
        events_transparent: false,
        ..Default::default()
    }
}

/// The region-selection overlay's layer-shell settings, targeted at one
/// named output — CAPTURE-RESEARCH D9's shape, verbatim (`Layer::Overlay`,
/// all four edges anchored, `size: (0, 0)`, `exclusive_zone: -1`,
/// `KeyboardInteractivity::Exclusive`, `OutputOption::OutputName`), which is
/// the configuration that was live-tested in nested niri in Stage 2 rather
/// than a fresh guess.
///
/// Three fields carry real weight:
///
/// - **`exclusive_zone: -1`** — "ignore everyone else's reserved space". The
///   overlay must cover the whole output including whatever strip the panel
///   reserved, or the top 48 px of the screen would be unselectable.
/// - **`keyboard_interactivity: Exclusive`** — Escape has to reach this
///   surface even though a normal window has focus. This is also the reason
///   the overlay is the one surface in this daemon that must **never** be
///   live-tested outside a nested niri (CLAUDE.md's binding rule): a bug
///   that leaves it mapped takes the keyboard with it.
/// - **`events_transparent: false`** — unlike the flash, this surface is the
///   whole point of the pointer. It covers the entire output, so it swallows
///   every click while it is up, which is correct here and exactly why it is
///   torn down the instant the user acts.
///
/// **Single-output, deliberately, for v0.1** (PLAN.md Stage 7, task 3
/// allows it; CAPTURE-RESEARCH D10 documents why): per-output surface
/// creation is source-verified but has never been *run* — this machine has
/// one output and niri's headless backend has no CLI surface — so
/// cross-output drags and Escape arbitration between several
/// exclusive-keyboard surfaces are untested. The overlay maps on the focused
/// output only (`capture::freeze_focused_output` picks it), and a selection
/// cannot leave that output. The signature already takes an output name, so
/// the multi-output version is a loop over `outputs()` here plus a shared
/// coordinate space in `modules::overlay`, not a rewrite.
fn overlay_surface_settings(output: &str) -> NewLayerShellSettings {
    NewLayerShellSettings {
        anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
        layer: Layer::Overlay,
        size: Some((0, 0)),
        margin: Some((0, 0, 0, 0)),
        exclusive_zone: Some(-1),
        keyboard_interactivity: KeyboardInteractivity::Exclusive,
        events_transparent: false,
        output_option: OutputOption::OutputName(output.to_string()),
        // Named so `niri msg layers` can tell this apart from the flash and
        // toast surfaces during a live check — the introspection command the
        // Stage 6 handoff calls out as the only one that lists layer-shell
        // surfaces at all.
        namespace: Some("saola-capture-overlay".to_string()),
    }
}

/// The delayed-capture countdown pill's layer-shell settings (PLAN.md Stage
/// 8, task 2): full-output, click-through, no keyboard, no reservation —
/// the same shape [`flash_surface_settings`] uses, minus the "spawn once at
/// boot" part (`modules::countdown`'s own doc comment explains why the
/// countdown doesn't want that trick even though it's available). Single-
/// output only, matching the flash and the (single-output-for-v0.1) overlay
/// — no `output_option` override, so this targets the currently active
/// output.
fn countdown_surface_settings() -> NewLayerShellSettings {
    NewLayerShellSettings {
        anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
        layer: Layer::Overlay,
        size: Some((0, 0)),
        margin: Some((0, 0, 0, 0)),
        exclusive_zone: Some(0),
        keyboard_interactivity: KeyboardInteractivity::None,
        events_transparent: true,
        // Named for the same `niri msg layers` introspection reason
        // `overlay_surface_settings` names its own surface.
        namespace: Some("saola-capture-countdown".to_string()),
        ..Default::default()
    }
}

/// Raw pointer/keyboard events, tagged with the surface they arrived on.
///
/// `iced::event::listen_with` takes a **`fn` pointer, not a closure**, so it
/// cannot capture the overlay's `window::Id` to filter on — hence the id
/// rides in the message and `Daemon::update` does the filtering. Everything
/// that is definitely not overlay input is dropped here rather than in
/// `update`, so an idle-but-open overlay isn't waking the daemon for window
/// events, touch, or key releases.
fn overlay_event_subscription() -> Subscription<Message> {
    iced::event::listen_with(|event, status, id| {
        let interesting = matches!(
            event,
            iced::Event::Mouse(_) | iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { .. })
        );
        interesting.then(|| Message::OverlayEvent {
            id,
            event,
            captured: status == iced::event::Status::Captured,
        })
    })
}

/// Spawns `saola-capture window edit <path>` detached — a toast click
/// (PLAN.md Stage 6, task 2). Same shape as `dbus::spawn_daemon_detached`
/// and `storage::spawn_clipboard_helper`: `current_exe()`, redirected
/// stdio, dropped `Child` handle. The window process is still Stage 9's
/// stub (`run_window` — it prints and exits 0 today), which is fine: this
/// call site only has to *ask*, not depend on what answers.
fn spawn_editor(path: &Path) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .arg("window")
        .arg("edit")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

/// A recording toast's click (PLAN.md Stage 12, task 3: "videos open
/// containing dir for now") — spawns `xdg-open` on the file's *parent*
/// directory, detached, the same fire-and-forget shape [`spawn_editor`] and
/// `dbus::spawn_daemon_detached` already use.
///
/// **`xdg-open`, not a portal.** CLAUDE.md's Boundaries section forbids
/// `xdg-desktop-portal` specifically (broken by configuration here, and
/// portals gate untrusted apps — irrelevant to this first-party component
/// asking to show its own output). `xdg-open` is the unrelated freedesktop
/// convenience script every desktop environment ships to resolve "open this
/// path with whatever the user's file manager is" — the same category of
/// external CLI boundary `ffmpeg` already is (CLAUDE.md Boundaries: "ffmpeg
/// is an external CLI boundary"), not a second one invented for this call.
/// A missing `xdg-open` (unlikely — it ships with `xdg-utils`, a near-universal
/// dependency of any desktop environment) degrades to a logged error at the
/// call site, never a panic.
fn open_containing_dir(path: &Path) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(path);
    std::process::Command::new("xdg-open")
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

/// The daemon's message type. `#[to_layer_message(multi)]` appends
/// iced_layershell's own layer-shell control variants (`NewLayerShell`,
/// `SizeChange`, …) and implements the `TryInto<LayerShellCustomActionWithId>`
/// conversion the runtime requires — see `saola-panel::main::Message`'s doc
/// comment for the full teaching note on what the macro does and why those
/// injected variants never reach `Daemon::update`. `multi` (not the
/// single-surface form) because Stage 6/7 spawn output-targeted surfaces on
/// demand (`NewLayerShellSettings` + `OutputOption::OutputName`, per Stage
/// 2's D9) — the same shape the panel's Islands layout uses, and the reason
/// this crate adopts `multi` from the start rather than migrating to it
/// later.
/// A thumbnail image, wrapped so [`Message`] can keep its plain
/// `#[derive(Debug, Clone)]`: `iced::widget::image::Handle` derives `Clone`
/// but **not** `Debug` (verified directly in `iced_core-0.14.0/src/image.rs`
/// — `#[derive(Clone, PartialEq, Eq)]`, no `Debug`), so embedding it in
/// `Message` bare would fail to compile the moment `#[to_layer_message]`'s
/// `Debug` requirement is checked. `Thumbnail`'s hand-written `Debug` prints
/// a placeholder rather than the pixel data, mirroring
/// `saola-lockscreen::modules::reveal::Message`'s own hand-written `Debug`
/// (there, redacting a password; here, just avoiding a `Handle` that has
/// nothing useful to print).
#[derive(Clone)]
struct Thumbnail(image::Handle);

impl fmt::Debug for Thumbnail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Thumbnail(..)")
    }
}

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
enum Message {
    /// The daemon should exit, and why. One variant for three distinct
    /// triggers (SIGTERM/SIGINT, another instance already running, the
    /// session bus itself unreachable) because all three want the exact
    /// same response — log why, then `iced::exit()` — and CLAUDE.md's
    /// no-panic rule means "just let it hang" is not an acceptable
    /// alternative to any of them.
    Shutdown(ShutdownReason),
    /// A screenshot was just saved — `dbus.rs`'s `CaptureService::screenshot`
    /// forwarded a [`dbus::DaemonEvent::CaptureTaken`] through
    /// `dbus_worker_stream`. Stage 6's PrintScr wiring: triggers the flash
    /// and pushes a toast (`Daemon::update`'s arm for this variant).
    CaptureTaken { path: String, thumbnail: Thumbnail },
    /// Wraps [`modules::flash::Message`] (just `Tick`) — the fade's own
    /// gated animation timer.
    Flash(modules::flash::Message),
    /// Wraps [`modules::toast::Message`] — the stack's tick, hover and
    /// click messages.
    Toast(modules::toast::Message),
    /// An interactive `shot --region` wants a rectangle —
    /// `dbus.rs`'s [`dbus::DaemonEvent::BeginRegion`], forwarded through
    /// `dbus_worker_stream`. Maps the overlay (`Daemon::begin_region`).
    BeginRegion(RegionRequest),
    /// A raw input event, tagged with the surface it landed on — see
    /// [`overlay_event_subscription`]. Only delivered while an overlay is
    /// up.
    OverlayEvent {
        id: window::Id,
        event: iced::Event,
        /// Whether a widget on the surface (a toolbar button) already
        /// handled it — `modules::overlay::message_from_event` uses this to
        /// keep a button press from also starting a drag underneath it.
        captured: bool,
    },
    /// Wraps [`modules::overlay::Message`] — what the overlay's own widgets
    /// (the toolbar buttons) emit directly, as opposed to the raw events
    /// above.
    Overlay(modules::overlay::Message),
    /// **Stage 8.** A delayed shot just started counting down —
    /// `dbus.rs`'s [`dbus::DaemonEvent::CountdownStarted`], forwarded
    /// through `dbus_worker_stream`. Maps the countdown pill
    /// (`Daemon::sync_countdown_surface`, via this variant's `update` arm).
    CountdownStarted(u32),
    /// Wraps [`modules::countdown::Message`] (just `Tick`) — the pill's own
    /// gated redraw-and-recheck timer, the same shape [`Message::Flash`]
    /// uses for the flash's fade.
    Countdown(modules::countdown::Message),
    /// **Stage 11.** A recording ended badly — `dbus.rs`'s
    /// [`dbus::DaemonEvent::RecordingFailed`], forwarded through
    /// `dbus_worker_stream`. Raises a notice toast
    /// (`modules::toast::ToastStack::push_notice`); the matching
    /// `io.saola.Capture1` `Error` signal was already emitted by the daemon's
    /// recording supervisor, so this variant is purely the on-screen half.
    RecordingFailed(String),
    /// **Stage 12.** A recording ended cleanly and was saved —
    /// `dbus.rs`'s [`dbus::DaemonEvent::RecordingFinished`], forwarded
    /// through `dbus_worker_stream`. Raises the finish toast PLAN.md task 3
    /// asks for (`modules::toast::ToastStack::push_recording`); the
    /// `RecordingFinished` D-Bus signal is emitted separately by the
    /// recording supervisor, same split `RecordingFailed`/`Error` already
    /// has.
    RecordingFinished(String),
    /// **Stage 13.** A non-fatal warning worth a card — `dbus.rs`'s
    /// [`dbus::DaemonEvent::Warning`], forwarded through
    /// `dbus_worker_stream`. Today's only sender is a recording that had to
    /// start without the audio it asked for. Same notice toast
    /// [`Message::RecordingFailed`] raises; unlike that one there is **no**
    /// matching D-Bus signal, because nothing failed (see the event's own
    /// doc comment).
    Warning { title: String, body: String },
}

/// Everything one interactive region selection needs to start, bundled so
/// [`Message`] carries one field instead of three (four, as of Stage 8).
///
/// Derives `Clone` because `#[to_layer_message(multi)]` requires `Message`
/// to — which is also why the reply half is an `mpsc::Sender` (clonable)
/// rather than a `oneshot::Sender` (not), and why the frozen frame is
/// wrapped in `modules::overlay::FrozenFrame` (an `image::Handle` has no
/// `Debug`). Cloning one is cheap: the handle is refcounted `Bytes`, the
/// sender a refcount bump, `OutputInfo` a short string plus numbers, and
/// `focused_window` a `Copy` newtype-over-`u64`.
#[derive(Debug, Clone)]
struct RegionRequest {
    frame: modules::overlay::FrozenFrame,
    output: capture::OutputInfo,
    /// **Stage 8.** See `dbus::DaemonEvent::BeginRegion::focused_window`'s
    /// doc comment — carried straight into `modules::overlay::Overlay::new`.
    focused_window: Option<capture::WindowRef>,
    reply: mpsc::Sender<dbus::RegionOutcome>,
}

#[derive(Debug, Clone)]
enum ShutdownReason {
    /// `SIGTERM` or `SIGINT` arrived — the normal `systemctl stop`/Ctrl-C
    /// path once Stage 17 wires autostart.
    Signal,
    /// `io.saola.Capture1` was already owned by another process —
    /// CLAUDE.md's single-instance rule: "a second `daemon` invocation
    /// exits cleanly when the name is taken."
    AlreadyRunning,
    /// The session bus connection itself failed, or the name request
    /// failed for a reason other than "already taken" — carries the
    /// error's own message since there's no more specific variant worth
    /// adding for what should be a rare, environment-level failure (no
    /// session bus at all).
    DBusUnavailable(String),
    /// **Stage 12.** The tray menu's "Quit daemon" row — `modules::tray`'s
    /// `TrayMenu::event` sends `DaemonEvent::QuitRequested`, which
    /// `dbus_worker_stream` turns into this. Same `iced::exit()` tail every
    /// other reason takes; this is purely which line gets printed.
    TrayQuit,
}

impl ShutdownReason {
    fn log(&self) {
        match self {
            ShutdownReason::Signal => {
                eprintln!("saola-capture: daemon: received SIGTERM/SIGINT — shutting down");
            }
            ShutdownReason::AlreadyRunning => {
                eprintln!(
                    "saola-capture: daemon: another instance already owns {} — exiting",
                    dbus::SERVICE_NAME
                );
            }
            ShutdownReason::DBusUnavailable(err) => {
                eprintln!(
                    "saola-capture: daemon: could not serve {}: {err}",
                    dbus::SERVICE_NAME
                );
            }
            ShutdownReason::TrayQuit => {
                eprintln!("saola-capture: daemon: quit via the tray menu — shutting down");
            }
        }
    }
}

/// The D-Bus worker: connect to the session bus, claim `io.saola.Capture1`
/// (or discover someone already has it), then hold the connection open for
/// the rest of the daemon's life while forwarding `dbus.rs`'s
/// [`dbus::DaemonEvent`]s into this daemon's own `Message` stream.
///
/// **Stage 6 teaching note (why this no longer `.await`s a bare
/// `pending::<()>()`):** Stage 3–5's version parked here doing nothing once
/// serving started, purely to keep `connection` (and therefore the
/// `ObjectServer`) alive for the daemon's whole life — zbus dispatches
/// every incoming method call on its own, off the connection's internal
/// reader task, so there was nothing left for *this* task to poll. Stage 6
/// gives it a real job that keeps exactly the same property: looping on
/// `events_rx.next()` still holds `connection` in scope for as long as the
/// loop runs (forever, on the `Serving` path — `events_tx` is never
/// dropped, since `dbus::serve` moved a clone of it into the long-lived
/// `CaptureService`), while also relaying every [`dbus::DaemonEvent`] a
/// `Screenshot` call sends into `sender` as the matching `Message` variant.
/// If `sender.send(..)` ever fails (the iced runtime shut down), the loop
/// simply stops — there's nothing further to forward to.
fn dbus_worker_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        let connection = match zbus::Connection::session().await {
            Ok(connection) => connection,
            Err(err) => {
                let _ = sender
                    .send(Message::Shutdown(ShutdownReason::DBusUnavailable(
                        err.to_string(),
                    )))
                    .await;
                return;
            }
        };

        // A small bounded channel: `CaptureService::screenshot` only ever
        // offers to it via `try_send` (never blocks a D-Bus reply on this
        // loop keeping up — see that method's doc comment), so a full
        // channel degrades to a logged, dropped event rather than backing
        // up method calls.
        let (events_tx, mut events_rx) = mpsc::channel::<dbus::DaemonEvent>(8);

        match dbus::serve(&connection, events_tx).await {
            Ok(dbus::ServeOutcome::Serving) => {
                while let Some(event) = events_rx.next().await {
                    let message = match event {
                        dbus::DaemonEvent::CaptureTaken { path, thumbnail } => {
                            Message::CaptureTaken {
                                path,
                                thumbnail: Thumbnail(thumbnail),
                            }
                        }
                        dbus::DaemonEvent::BeginRegion {
                            frame,
                            output,
                            focused_window,
                            reply,
                        } => Message::BeginRegion(RegionRequest {
                            frame: modules::overlay::FrozenFrame::new(frame),
                            output,
                            focused_window,
                            reply,
                        }),
                        dbus::DaemonEvent::CountdownStarted { seconds } => {
                            Message::CountdownStarted(seconds)
                        }
                        dbus::DaemonEvent::RecordingFailed { message } => {
                            Message::RecordingFailed(message)
                        }
                        // **Stage 12.** The finish toast — same shape as
                        // `RecordingFailed` above, minus the "why".
                        dbus::DaemonEvent::RecordingFinished { path } => {
                            Message::RecordingFinished(path)
                        }
                        // **Stage 13.** A warning with no failure behind it
                        // — see the event's own doc comment.
                        dbus::DaemonEvent::Warning { title, body } => {
                            Message::Warning { title, body }
                        }
                        // **Stage 12.** The tray's "Quit daemon" menu action
                        // — reuses the existing shutdown path exactly like a
                        // SIGTERM would.
                        dbus::DaemonEvent::QuitRequested => {
                            Message::Shutdown(ShutdownReason::TrayQuit)
                        }
                    };
                    if sender.send(message).await.is_err() {
                        break;
                    }
                }
            }
            Ok(dbus::ServeOutcome::AlreadyRunning) => {
                let _ = sender
                    .send(Message::Shutdown(ShutdownReason::AlreadyRunning))
                    .await;
            }
            Err(err) => {
                let _ = sender
                    .send(Message::Shutdown(ShutdownReason::DBusUnavailable(
                        err.to_string(),
                    )))
                    .await;
            }
        }
    })
}

/// Waits for `SIGTERM` or `SIGINT` — the two ways the daemon is asked to
/// stop (`systemctl stop` once Stage 17 wires autostart, and Ctrl-C when
/// run by hand) — then sends exactly one `Shutdown` message. Mirrors
/// `saola-session::main`'s own signal-wait code (same two signals, same
/// `tokio::signal::unix` API), wrapped as an `iced::Subscription` instead
/// of a bare `tokio::select!` in `main` because this process's only
/// sanctioned place to run async code outside iced's own event loop is
/// exactly this kind of stream (see this module's doc comment on "one
/// runtime").
fn shutdown_signal_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, async |mut sender: mpsc::Sender<Message>| {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(err) => {
                eprintln!("saola-capture: daemon: could not install a SIGTERM handler: {err}");
                return;
            }
        };
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(signal) => signal,
            Err(err) => {
                eprintln!("saola-capture: daemon: could not install a SIGINT handler: {err}");
                return;
            }
        };

        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }

        let _ = sender.send(Message::Shutdown(ShutdownReason::Signal)).await;
    })
}
