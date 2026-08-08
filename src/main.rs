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
//! `storage::save_capture`), and the saved path prints to stdout. What is
//! still a stub: `record` (Stages 10–11), `pick-color` (Stage 16), the
//! `window` process (Stage 9), and the two capture *kinds* that need a
//! surface — an interactive `--region` (Stage 7's overlay) and `--window`
//! (Stage 8) — each reporting a clean error naming its stage rather than
//! failing silently.
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

mod capture;
mod cli;
mod config;
mod dbus;
mod storage;

use std::fmt;
use std::future::Future;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use iced::futures::channel::mpsc;
use iced::futures::{SinkExt, Stream};
use iced::widget::Space;
use iced::window;
use iced::{Element, Subscription, Task};
use iced_layershell::build_pattern::daemon;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;

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

/// `record start|stop|toggle`. `toggle` is forwarded as `StartRecording`
/// for now — deciding "start or stop?" needs the daemon's own recorder
/// state (`modules/recorder.rs`'s state machine, Stage 11/12), which
/// doesn't exist yet. Harmless at this stage: `StartRecording` always
/// answers with the same stub `Error` regardless of which action asked
/// for it, so nothing here can silently do the wrong thing — Stage 12 is
/// what makes `toggle` query real state before choosing.
fn run_record(config_dir: Option<&Path>, args: cli::RecordArgs) -> Result<String, CliRunError> {
    let config = load_config(config_dir);
    let options = cli::RecordOptions::resolve(&config, &args)?;

    run_async(async move {
        let connection = connect_to_daemon().await?;
        let proxy = dbus::Capture1Proxy::new(&connection).await?;

        match options.action {
            cli::RecordActionKind::Stop => {
                let path = proxy.stop_recording().await?;
                Ok(path)
            }
            cli::RecordActionKind::Start | cli::RecordActionKind::Toggle => {
                // "fullscreen" is the only kind Stage 3's CLI can express —
                // `--region`/`--window` recording targets are Stage 12.
                proxy
                    .start_recording("fullscreen", options.to_dbus_options())
                    .await?;
                Ok(format!(
                    "recording {} requested (preset={})",
                    options.action.as_str(),
                    options.preset
                ))
            }
        }
    })
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

/// `open`: ask the daemon to raise the main window.
fn run_open() -> Result<String, CliRunError> {
    run_async(async {
        let connection = connect_to_daemon().await?;
        let proxy = dbus::Capture1Proxy::new(&connection).await?;
        proxy.open_window("main").await?;
        Ok("opened the app window".to_string())
    })
}

/// `window [edit <path>]`: the separate-process app window. Still a stub
/// (Stage 9 builds the real `iced` window process) — but it now parses
/// its real argument shape (`edit <path>`) and reports what it would have
/// opened, rather than Stage 1's bare "not implemented" print.
fn run_window(action: Option<&cli::WindowAction>) -> ExitCode {
    let mode = cli::WindowAction::dbus_mode(action);
    println!(
        "saola-capture window: the app window process is not implemented yet (Stage 9) — mode={mode}"
    );
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------
// The daemon: a surfaceless iced_layershell daemon (PLAN.md Stage 3, task
// 4). No layer-shell surface is ever mapped by this stage — Stages 5/6
// spawn the first ones (flash/toast/overlay) on demand.
// ---------------------------------------------------------------------

fn run_daemon() -> ExitCode {
    let result = daemon(Daemon::boot, "saola-capture", Daemon::update, Daemon::view)
        .subscription(Daemon::subscription)
        .settings(Settings {
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

/// Registry of live layer-shell surfaces, keyed by iced's `window::Id`, and
/// what each one is *for* — the shape `saola-panel::main::SurfaceRole`
/// established, copied here ahead of any surface actually existing
/// (PLAN.md Stage 3, task 4: "the SurfaceRole registry in place (no
/// surfaces yet)").
///
/// Currently uninhabited on purpose, rather than a placeholder variant:
/// Stage 6 (flash/toast) and Stage 7 (overlay) add real variants here as
/// they start spawning surfaces, and an uninhabited enum still supports an
/// exhaustive, zero-arm `match` (see `Daemon::view` below) — so the
/// *pattern* this registry uses is proven out now, and adding the first
/// real variant later is a compile error at every match site that isn't
/// updated, never a silently-wrong fallthrough.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceRole {}

/// The daemon's whole state. Empty today — `windows` is always empty until
/// Stage 6/7 spawn a surface — but the field exists now so later stages
/// extend this struct instead of inventing where the registry lives.
#[derive(Debug, Default)]
struct Daemon {
    windows: std::collections::HashMap<window::Id, SurfaceRole>,
}

impl Daemon {
    /// `iced_layershell::build_pattern::daemon`'s boot closure: no
    /// surfaces to spawn yet, so nothing to do beyond building the default
    /// state.
    fn boot() -> (Self, Task<Message>) {
        (Self::default(), Task::none())
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
            // The macro-injected layer-shell control variants (see
            // `Message`'s doc comment) never reach here — this is the
            // same catch-all `saola-panel::main::Panel::update` ends with,
            // for the same reason.
            _ => Task::none(),
        }
    }

    /// No surface this stage ever registers a role for, so every `id` this
    /// is called with (in practice: none — see `LayerShellSettings`'s
    /// `start_mode` comment) falls through to an empty element. The
    /// `Some` arm's `match *role {}` is an exhaustive zero-arm match over
    /// [`SurfaceRole`] — it can never actually run (the map is always
    /// empty until Stage 6/7), but it's the shape those stages' real
    /// `match` arms will replace, kept here as documentation of the
    /// pattern rather than left implicit.
    fn view(&self, id: window::Id) -> Element<'_, Message> {
        match self.windows.get(&id) {
            Some(role) => match *role {},
            None => Space::new().into(),
        }
    }

    /// Two independent workers, batched: the D-Bus service (which owns the
    /// bus name for the daemon's whole life, or reports why it couldn't)
    /// and the SIGTERM/SIGINT wait. Both funnel into the same
    /// `Message::Shutdown` — see that variant's doc comment for why a
    /// unified exit path is the point.
    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run(dbus_worker_stream),
            Subscription::run(shutdown_signal_stream),
        ])
    }
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
        }
    }
}

/// The D-Bus worker: connect to the session bus, claim `io.saola.Capture1`
/// (or discover someone already has it), then hold the connection open for
/// the rest of the daemon's life.
///
/// Teaching note (why this `.await`s forever on success): once
/// `dbus::serve` has registered the object and claimed the name, there is
/// nothing left for *this* task to poll — zbus's `ObjectServer` dispatches
/// every incoming method call on its own, off the `Connection`'s internal
/// reader task, the moment a message arrives. This stream's only remaining
/// job is to keep `connection` (and therefore the `ObjectServer`) alive
/// for as long as the daemon runs; `std::future::pending::<()>()` is an
/// explicit, self-documenting way to do that, rather than an accidental
/// side effect of some other `.await` that happens to never resolve.
fn dbus_worker_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, async |mut sender: mpsc::Sender<Message>| {
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

        match dbus::serve(&connection).await {
            Ok(dbus::ServeOutcome::Serving) => {
                std::future::pending::<()>().await;
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
