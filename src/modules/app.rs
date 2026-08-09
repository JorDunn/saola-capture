//! `saola-capture window [edit <path>]` — the separate-process app window
//! (PLAN.md Stage 9; Architecture: "a plain iced multi-window app ... on
//! `Surface::Paper` as a regular niri toplevel. It is a D-Bus client of the
//! daemon: capture buttons call the daemon and hide the window").
//!
//! # Why this is a *plain* `iced::application`, not another `iced_layershell`
//! # daemon (teaching note)
//!
//! Every other surface in this crate (`modules::flash`/`toast`/`overlay`/
//! `countdown`) is a layer-shell surface owned by the daemon process's
//! `iced_layershell::build_pattern::daemon` in `main.rs` — Architecture
//! forces that split ("iced_layershell's daemon hosts layer-shell surfaces
//! only, not xdg toplevels"). This module is the other half: a normal
//! desktop window (rounded, bordered, shadowed — `Surface::Paper`'s own
//! chrome, §7), opened with plain `iced::application`, running in its own
//! process (`main.rs::run_window` spawns it, or execs it directly for
//! `saola-capture window`). It shares nothing with the daemon's event loop
//! or state — every capture request crosses the same `io.saola.Capture1`
//! bus every CLI verb already uses.
//!
//! # The hide/reopen model, as built (PLAN.md Stage 9, task 2)
//!
//! "Saola has no minimize" (Architecture) — pressing Capture doesn't shrink
//! the window to a taskbar, it makes it disappear entirely
//! (`iced::window::Mode::Hidden`) for exactly as long as the daemon needs to
//! answer, then brings it back. Two different signals decide "as long as
//! the daemon needs", because the two D-Bus methods behind them have two
//! different completion shapes:
//!
//! - **`Screenshot`** (Stage 5/7/8) does not return until the whole capture
//!   — including an interactive region drag, if that's the chosen target —
//!   is saved, and it already emits `CaptureTaken` before it returns
//!   (`dbus.rs::CaptureService::screenshot`). So [`start_capture`] hides the
//!   window, awaits the method call directly, and re-shows on its reply —
//!   there is no need to *also* subscribe to the `CaptureTaken` signal, the
//!   method's own return is a strictly-later event than the signal.
//! - **`StartRecording`** does not have that property: PLAN.md Stage 12 is
//!   explicit that the app window's Record tab "now drives real recordings
//!   and hides while recording" — i.e. a real `StartRecording` call returns
//!   almost immediately (recording has *begun*, not *finished*), and only
//!   `RecordingFinished` (fired whenever the recording is later stopped —
//!   from the tray, the CLI, anywhere) marks the actual end. Building that
//!   second wait — a signal subscription parameterized by the live
//!   `zbus::Connection`, plus recorder-state awareness so this window even
//!   knows whether a recording is in flight — is explicitly Stage 12's job,
//!   not this one's; `zbus::Connection` also isn't `Hash`, which is what
//!   `iced::Subscription::run_with` would need to key a per-connection
//!   signal stream, so Stage 12 will need its own answer to that regardless.
//!   Stage 9 wires the Record tab's button to the same "hide, await the
//!   reply, re-show" shape as the Screenshot tab for now — today that reply
//!   is `StartRecording`'s stub `Error` (`dbus.rs` — Stage 11 lands the
//!   real thing; Stage 10 built the capture half but deliberately left this
//!   method a stub), so pressing it hides the window for a moment and shows
//!   that stub message, which is the correct, honest behavior for a method
//!   that isn't implemented yet (CLAUDE.md's no-panic/no-silent-stub rule).
//!
//! **Reopening a window that isn't hidden-but-alive — it's flat out not
//! running — is a separate story**, and is the `open` CLI verb / toast
//! click / tray menu list PLAN.md's task 2 names. All three now funnel
//! through spawning a fresh `saola-capture window [edit <path>]` process:
//! `OpenWindow` (`dbus.rs`, wired for real this stage) and the toast's own
//! `spawn_editor` (`main.rs`, unchanged since Stage 6) both do this, and the
//! future tray menu (Stage 12) gets it for free by calling `OpenWindow` the
//! same way `open` does. This process keeps no registry of "is a window
//! already hidden somewhere" — a second `open` while one is already hidden
//! spawns a second process. Documented as an acceptable v0.1 gap in the
//! Stage 9 handoff, not a silent oversight: building that registry means
//! the *daemon* tracking window-process liveness, which is more than this
//! stage's task list asks for.
//!
//! # The editor stub (PLAN.md Stage 9, task 3)
//!
//! `window edit <path>` skips the picker entirely and boots straight into
//! [`ViewState::Editor`]: the same paper-window chrome, the decoded image
//! (synchronous `image::open` + `to_rgba8` at boot — a screenshot-sized
//! file decodes in well under a frame, so there's no need for the
//! lockscreen wallpaper's async-decode dance here), and a one-line note
//! that the annotation tools land in Stage 14. No canvas, no tool palette,
//! no save path — [`load_image`] and [`editor_view`] are the whole surface
//! Stage 14 replaces.
//!
//! # §11 checklist, walked (PLAN.md Stage 9, task 4)
//!
//! **The main window's chrome** (the outer `paper_window`-styled container,
//! the 46px header):
//! 1. Ivory (`Surface::Paper`) — a window, not shell chrome, per §2.
//! 2. Exactly one terracotta element: the Capture/Start Recording button
//!    (`button::active`) — the live action, matching the toolbar's own
//!    Capture button precedent in `modules::overlay`.
//! 3. Every other control at rest is ivory/fill (`button::rest`,
//!    `button::bare` for Close, `segmented::segment`'s unselected arm,
//!    `toggler`'s off state) — ink-on-paper text throughout, the opposite
//!    of its fill, per the rule.
//! 4. Body/label text is `typography.size.body`/`secondary` ≥ 13px; the
//!    section labels are `size.label` (11px, Mono 500, uppercased in code
//!    since iced 0.14 has no letter-spacing property to add the spec's
//!    0.12em) — no counting readout exists on this surface, so tabular
//!    numerals don't apply here the way they do on the overlay/toast.
//! 5. Corners: `radii.window` (24px) on the outer chrome, `radii.pill` on
//!    every button/toggle/segment — no sub-18px radius anywhere.
//! 6. Zero serif — the "Saola Capture" header title is Sans, not Serif;
//!    this surface has no dialog title or section heading role to earn one.
//! 7. No icons — same deliberate choice `modules::overlay` already made
//!    (text pills read fine for a five-button surface; `src/icons.rs`
//!    still doesn't exist).
//! 8. Animates? No — a plain toplevel window has none of the daemon
//!    surfaces' timed fades; not a violation, since "does it animate" only
//!    binds surfaces that do.
//! 9. N/A — not a popover.
//! 10. Added a colour? No — ink/ivory/terracotta only, `paper_window`'s own
//!     ink border included.
//!
//! **The editor stub** is a strict subset of the same chrome (header +
//! paper body, no controls at all besides Close), so every item above
//! applies to it unchanged; it has no button to be the "one terracotta
//! element", which is fine — §11's rule is "at most one", not "exactly
//! one on every surface that exists".

use std::path::{Path, PathBuf};

use iced::widget::{
    button, column, container, mouse_area, row, rule, scrollable, text, toggler, Space,
};
use iced::{window, Center, Element, Length, Padding, Subscription, Task};
use saola_theme::{ColorExt, Surface, Theme};
use zbus::Connection;

use crate::cli::{self, AudioSource, ShotKind};
use crate::config::{CaptureConfig, ImageFormat, VideoPreset};
use crate::dbus::Capture1Proxy;

// ---------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------

/// Which view `window` should boot straight into — the typed form of
/// `cli::WindowAction` this module actually wants (no string round-trip:
/// `main.rs` already has a real `Option<&cli::WindowAction>` from argv, so
/// [`window_mode_from_action`] converts it directly. The `"main"`/
/// `"edit:<path>"` *string* shape only exists at the `OpenWindow` D-Bus
/// boundary, in `dbus.rs`, where it has to be — a method argument has no
/// other way to carry this).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowMode {
    Main,
    Edit(PathBuf),
}

/// The inverse of `cli::WindowAction::dbus_mode`, for the one caller
/// (`main.rs::run_window`) that has a real `cli::WindowAction` already and
/// shouldn't have to detour through a string to get a [`WindowMode`].
pub fn window_mode_from_action(action: Option<&cli::WindowAction>) -> WindowMode {
    match action {
        None => WindowMode::Main,
        Some(cli::WindowAction::Edit { path }) => WindowMode::Edit(path.clone()),
    }
}

/// Build and run the window process's whole `iced::application`. Blocks
/// until the window closes (`exit_on_close_request`'s default of `true`
/// covers both this window's own Close button, via `window::close`, and
/// niri's native close keybind).
pub fn run(mode: WindowMode) -> iced::Result {
    let theme = Theme::saola();
    let default_font = saola_theme::convert::ui_font(&theme);
    let width = theme.sizes.popover_width;
    let height = window_height(&theme);

    iced::application(move || App::boot(mode.clone()), App::update, App::view)
        .title(App::title)
        .subscription(App::subscription)
        .theme(App::theme)
        .style(App::style)
        // No system title bar — §7's own words: "Saola's own windows draw a
        // 46px header" (the `header` function below), same posture the
        // layer-shell surfaces take with their own chrome.
        .decorations(false)
        // `transparent` (the window-settings flag) plus `.style` returning
        // a transparent clear color (below) are both needed for the same
        // reason CLAUDE.md's Stage 6 finding documents for layer-shell
        // surfaces: without an explicit transparent clear color, iced
        // paints the theme's own opaque background before drawing
        // anything, which would square off `paper_window`'s rounded
        // corners against a rectangle nobody asked for. That finding was
        // about `iced_layershell` specifically; applying the same defensive
        // pair here is cheap and untested-but-consistent — see the Stage 9
        // handoff for what live-checking this actually confirmed.
        .transparent(true)
        .resizable(false)
        .centered()
        .window_size(iced::Size::new(width, height))
        .settings(iced::Settings {
            default_font,
            ..iced::Settings::default()
        })
        .run()
}

/// No style-guide token sizes a utility window's height directly (§4's
/// `Sizes` table covers popover/launcher/notification-card *widths*, never
/// a settings-style window) — derived the same way `modules::toast::
/// card_height` derives its own undocumented height: a small multiple of
/// `sizes.list_row` (roughly one row per control group) plus the header and
/// generous padding, rather than a bare literal. If the content overflows
/// this estimate (the Record tab's extra rows, say), [`App::main_view`]
/// wraps everything in a `scrollable` — this is a starting size, not a hard
/// clip.
fn window_height(theme: &Theme) -> f32 {
    theme.sizes.window_header + 8.0 * theme.sizes.list_row + 4.0 * theme.sizes.popover_padding
}

// ---------------------------------------------------------------------
// State
// ---------------------------------------------------------------------

/// Which of the two capture modes the segmented tabs have selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureMode {
    Screenshot,
    Record,
}

/// The Record tab's audio picker, as a segmented-control-friendly closed
/// set — `Option<cli::AudioSource>` has no `None`-inclusive label of its
/// own, so this is that closed set spelled out, converted to/from
/// `Option<AudioSource>` only at the one point that needs it
/// ([`AudioChoice::to_option`], read by [`record_options`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioChoice {
    NoAudio,
    Mic,
    System,
    Both,
}

impl AudioChoice {
    fn to_option(self) -> Option<AudioSource> {
        match self {
            AudioChoice::NoAudio => None,
            AudioChoice::Mic => Some(AudioSource::Mic),
            AudioChoice::System => Some(AudioSource::System),
            AudioChoice::Both => Some(AudioSource::Both),
        }
    }
}

/// Which screen this process is showing — set once at boot from
/// [`WindowMode`] and never changed afterward (there is no in-app
/// navigation between "main" and "editor"; a toast click or `window edit`
/// spawns a whole new process for the editor instead, per the module doc
/// comment).
enum ViewState {
    Main,
    Editor {
        path: PathBuf,
        /// Decoded once, at boot, by [`load_image`]. `Err` renders as an
        /// inline message rather than failing to open at all — a missing or
        /// corrupt file is not a reason to crash a window that could still
        /// usefully show the path and let the user close it (no-panic
        /// rule).
        image: Result<iced::widget::image::Handle, String>,
    },
}

/// The window process's whole state.
struct App {
    theme: Theme,
    config: CaptureConfig,
    /// `None` until the boot-time `connect` task resolves — see
    /// [`App::boot`]/[`connect`]. The Capture/Record button is disabled the
    /// whole time it's `None` ([`App::main_view`]).
    connection: Option<Connection>,
    /// Set once, from the first (and only) [`window::open_events`] this
    /// process will ever see — a plain `iced::application` opens exactly
    /// one window, so there's nothing to disambiguate.
    window_id: Option<window::Id>,
    view: ViewState,
    mode: CaptureMode,
    target: ShotKind,
    delay: u32,
    cursor: bool,
    format: ImageFormat,
    preset: VideoPreset,
    audio: AudioChoice,
    /// `true` from the moment Capture/Start Recording is pressed until its
    /// D-Bus reply lands — guards against a second press re-hiding an
    /// already-hidden window mid-request.
    busy: bool,
    /// The last thing worth telling the user inline: `Ok` for a completed
    /// capture/connect, `Err` for anything that failed (a bad D-Bus reply,
    /// a connect failure, StartRecording's current stub `Error`). Rendered
    /// as plain text — CLAUDE.md's "severity is carried by wording, not
    /// colour" rule applies here exactly as it does everywhere else in
    /// Saola, so `Err` never paints red.
    feedback: Option<Result<String, String>>,
}

impl App {
    fn boot(mode: WindowMode) -> (App, Task<Message>) {
        let theme = Theme::saola();
        let config = CaptureConfig::load(CaptureConfig::resolve_path(None).as_deref());

        let view = match &mode {
            WindowMode::Main => ViewState::Main,
            WindowMode::Edit(path) => ViewState::Editor {
                path: path.clone(),
                image: load_image(path),
            },
        };

        let state = App {
            target: ShotKind::Fullscreen,
            delay: config.delay,
            cursor: config.cursor,
            format: config.image_format,
            preset: config.video_preset,
            audio: AudioChoice::NoAudio,
            theme,
            config,
            connection: None,
            window_id: None,
            view,
            mode: CaptureMode::Screenshot,
            busy: false,
            feedback: None,
        };

        (state, Task::perform(connect(), Message::Connected))
    }

    fn title(&self) -> String {
        match &self.view {
            ViewState::Main => "Saola Capture".to_string(),
            ViewState::Editor { path, .. } => format!("Saola Capture — {}", file_label(path)),
        }
    }

    fn theme(&self) -> iced::Theme {
        saola_theme::to_iced_theme(&self.theme)
    }

    /// See [`run`]'s doc comment on `.transparent(true)` for why this
    /// mirrors `main.rs::Daemon::style` even though it's never (yet) been
    /// caught live the way that layer-shell version was.
    fn style(&self, theme: &iced::Theme) -> iced::theme::Style {
        iced::theme::Style {
            background_color: iced::Color::TRANSPARENT,
            ..iced::theme::default(theme)
        }
    }

    /// Just one job: learn this process's one window's `Id`, the first time
    /// it opens. Nothing else in this window needs a subscription — there
    /// is no raw-input, animation-tick, or D-Bus-signal listening the way
    /// the daemon's surfaces need (see the module doc comment on why a
    /// `RecordingFinished` signal subscription is deliberately not built
    /// yet).
    fn subscription(&self) -> Subscription<Message> {
        window::open_events().map(Message::WindowOpened)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WindowOpened(id) => {
                self.window_id = Some(id);
                Task::none()
            }
            Message::Connected(Ok(connection)) => {
                self.connection = Some(connection);
                Task::none()
            }
            Message::Connected(Err(err)) => {
                self.feedback = Some(Err(err));
                Task::none()
            }
            Message::ModeSelected(mode) => {
                self.mode = mode;
                Task::none()
            }
            Message::TargetSelected(target) => {
                self.target = target;
                Task::none()
            }
            Message::DelaySelected(delay) => {
                self.delay = delay;
                Task::none()
            }
            Message::CursorToggled(cursor) => {
                self.cursor = cursor;
                Task::none()
            }
            Message::FormatSelected(format) => {
                self.format = format;
                Task::none()
            }
            Message::PresetSelected(preset) => {
                self.preset = preset;
                Task::none()
            }
            Message::AudioSelected(audio) => {
                self.audio = audio;
                Task::none()
            }
            Message::Capture => self.start_capture(),
            Message::ScreenshotFinished(result) => {
                self.finish(result.map(|path| format!("Saved: {path}")))
            }
            Message::RecordingRequested(result) => {
                self.finish(result.map(|()| "Recording requested.".to_string()))
            }
            Message::DragWindow => match self.window_id {
                Some(id) => window::drag(id),
                None => Task::none(),
            },
            Message::ClosePressed => match self.window_id {
                Some(id) => window::close(id),
                // No window Id yet (a very early close race) — nothing to
                // ask the compositor to destroy, so just end the process.
                None => iced::exit(),
            },
        }
    }

    /// Hide, then ask the daemon — see the module doc comment's "hide/reopen
    /// model" section for why the two modes' waits differ and why that's
    /// fine at this stage.
    fn start_capture(&mut self) -> Task<Message> {
        if self.busy {
            return Task::none();
        }
        let Some(connection) = self.connection.clone() else {
            self.feedback = Some(Err(
                "not connected to the daemon yet — try again in a moment".to_string(),
            ));
            return Task::none();
        };

        let request = match self.mode {
            CaptureMode::Screenshot => {
                match resolve_capture_options(
                    &self.config,
                    self.target,
                    self.delay,
                    self.cursor,
                    self.format,
                ) {
                    Ok(options) => Task::perform(
                        request_screenshot(connection, options),
                        Message::ScreenshotFinished,
                    ),
                    Err(err) => {
                        // Every UI-driven flag combination is internally
                        // consistent by construction (see
                        // `resolve_capture_options`'s tests), so this arm is
                        // not reachable in practice — but the no-panic rule
                        // means it still needs a real value, not an
                        // `.expect()`.
                        self.feedback = Some(Err(err.to_string()));
                        return Task::none();
                    }
                }
            }
            CaptureMode::Record => {
                let options = record_options(self.preset, self.audio.to_option());
                Task::perform(
                    request_recording(connection, options),
                    Message::RecordingRequested,
                )
            }
        };

        self.busy = true;
        self.feedback = None;
        let hide = match self.window_id {
            Some(id) => window::set_mode(id, window::Mode::Hidden),
            None => Task::none(),
        };
        Task::batch([hide, request])
    }

    fn finish(&mut self, result: Result<String, String>) -> Task<Message> {
        self.busy = false;
        self.feedback = Some(result);
        match self.window_id {
            Some(id) => window::set_mode(id, window::Mode::Windowed),
            None => Task::none(),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let theme = &self.theme;
        let head = header(theme, &self.title());
        let divider =
            rule::horizontal(1.0).style(saola_theme::style::rule::rest(theme, Surface::Paper));
        let body = match &self.view {
            ViewState::Main => self.main_view(),
            ViewState::Editor { path, image } => editor_view(theme, path, image),
        };

        let content = column![head, divider, body];

        container(content)
            .width(Length::Fixed(theme.sizes.popover_width))
            .height(Length::Fill)
            .style(saola_theme::style::container::paper_window(theme))
            .into()
    }

    fn main_view(&self) -> Element<'_, Message> {
        let theme = &self.theme;
        let mut sections: Vec<Element<'_, Message>> = Vec::new();

        sections.push(section_label(theme, "Mode"));
        sections.push(segmented_row(
            theme,
            &[
                (CaptureMode::Screenshot, "Screenshot"),
                (CaptureMode::Record, "Record"),
            ],
            self.mode,
            Message::ModeSelected,
        ));

        match self.mode {
            CaptureMode::Screenshot => {
                sections.push(section_label(theme, "Target"));
                sections.push(segmented_row(
                    theme,
                    &[
                        (ShotKind::Fullscreen, "Full screen"),
                        (ShotKind::Region, "Region"),
                        (ShotKind::Window, "Window"),
                    ],
                    self.target,
                    Message::TargetSelected,
                ));

                sections.push(section_label(theme, "Format"));
                sections.push(segmented_row(
                    theme,
                    &[(ImageFormat::Webp, "WebP"), (ImageFormat::Png, "PNG")],
                    self.format,
                    Message::FormatSelected,
                ));
            }
            CaptureMode::Record => {
                sections.push(section_label(theme, "Preset"));
                sections.push(segmented_row(
                    theme,
                    &[
                        (VideoPreset::Hevc, "HEVC"),
                        (VideoPreset::Av1, "AV1"),
                        (VideoPreset::H264, "H.264"),
                    ],
                    self.preset,
                    Message::PresetSelected,
                ));

                // Architecture: "audio (mic/system/both, Opus) — audio is
                // inert until Stage 13". The picker is real (the chosen
                // value reaches `RecordOptions::to_dbus_options`); what's
                // inert is the daemon side — `StartRecording` doesn't
                // consume `audio` yet, same stub posture as everything else
                // Stage 11/13 land.
                sections.push(section_label(theme, "Audio"));
                sections.push(segmented_row(
                    theme,
                    &[
                        (AudioChoice::NoAudio, "No audio"),
                        (AudioChoice::Mic, "Mic"),
                        (AudioChoice::System, "System"),
                        (AudioChoice::Both, "Both"),
                    ],
                    self.audio,
                    Message::AudioSelected,
                ));
            }
        }

        sections.push(section_label(theme, "Delay"));
        sections.push(segmented_row(
            theme,
            &[(0u32, "No delay"), (3, "3s"), (5, "5s"), (10, "10s")],
            self.delay,
            Message::DelaySelected,
        ));

        sections.push(cursor_toggle(theme, self.cursor));
        sections.push(capture_button(
            theme,
            self.mode,
            self.busy,
            self.connection.is_some(),
        ));

        if let Some(feedback) = &self.feedback {
            sections.push(feedback_view(theme, feedback));
        } else if self.connection.is_none() {
            sections.push(hint_view(theme, "Connecting to the daemon…"));
        }

        let mut list = column![].spacing(theme.sizes.island_gap);
        for section in sections {
            list = list.push(section);
        }

        scrollable(list.padding(theme.sizes.popover_padding))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

// ---------------------------------------------------------------------
// D-Bus (async helpers run via `Task::perform`)
// ---------------------------------------------------------------------

/// Boot-time connect: the session bus plus the same auto-spawn-and-wait
/// every CLI verb goes through (`dbus::ensure_daemon_running`) — opening
/// the app window should be no less resilient to "no daemon running yet"
/// than `shot`/`record`/`open` already are.
async fn connect() -> Result<Connection, String> {
    let connection = Connection::session().await.map_err(|err| err.to_string())?;
    crate::dbus::ensure_daemon_running(&connection)
        .await
        .map_err(|err| err.to_string())?;
    Ok(connection)
}

async fn request_screenshot(
    connection: Connection,
    options: cli::CaptureOptions,
) -> Result<String, String> {
    let proxy = Capture1Proxy::new(&connection)
        .await
        .map_err(|err| err.to_string())?;
    proxy
        .screenshot(options.kind.as_str(), options.to_dbus_options())
        .await
        .map_err(|err| err.to_string())
}

async fn request_recording(
    connection: Connection,
    options: cli::RecordOptions,
) -> Result<(), String> {
    let proxy = Capture1Proxy::new(&connection)
        .await
        .map_err(|err| err.to_string())?;
    // "fullscreen" is the only recording target the wire protocol can
    // express today — the same limitation `main.rs::run_record`'s own
    // comment documents; `--region`/`--window` recording is Stage 12.
    proxy
        .start_recording("fullscreen", options.to_dbus_options())
        .await
        .map_err(|err| err.to_string())
}

// ---------------------------------------------------------------------
// Pure option-resolution (unit-tested below)
// ---------------------------------------------------------------------

/// Folds the UI's screenshot state through the exact same `cli::ShotArgs` →
/// `cli::CaptureOptions` path `shot`'s own CLI parsing uses, by building a
/// synthetic [`cli::ShotArgs`] and calling [`cli::CaptureOptions::resolve`]
/// on it — rather than hand-rolling a second, parallel "flags fold over
/// config" implementation for the app window to maintain. Starting from
/// `ShotArgs::default()` (not naming every field) means a future flag added
/// to `ShotArgs` doesn't silently need a matching edit here to keep
/// compiling.
fn resolve_capture_options(
    config: &CaptureConfig,
    target: ShotKind,
    delay: u32,
    cursor: bool,
    format: ImageFormat,
) -> Result<cli::CaptureOptions, cli::CliError> {
    let mut args = cli::ShotArgs::default();
    match target {
        ShotKind::Fullscreen => args.fullscreen = true,
        ShotKind::Region => args.region = true,
        ShotKind::Window => args.window = true,
    }
    args.format = Some(format.as_str().to_string());
    args.delay = Some(delay);
    args.cursor = cursor;
    args.no_cursor = !cursor;

    cli::CaptureOptions::resolve(config, &args)
}

/// The Record tab's UI state, folded into a [`cli::RecordOptions`] — no
/// `ShotArgs`-style synthetic-args detour needed here since
/// `RecordOptions`'s fields are already exactly what the UI tracks (no
/// config-file precedence to replay: `capture.toml` carries no audio knob,
/// per `cli::RecordOptions`'s own doc comment, and the preset picker's
/// initial value already came from `config.video_preset` in
/// [`App::boot`]).
fn record_options(preset: VideoPreset, audio: Option<AudioSource>) -> cli::RecordOptions {
    cli::RecordOptions {
        action: cli::RecordActionKind::Start,
        preset,
        audio,
        // The app window starts *real* recordings; `--dry-run` (Stage 10)
        // is a terminal diagnostic that never reaches the daemon, so there
        // is nothing here for it to mean — and `window_id` rides along with
        // it (it is dry-run-only until Stage 12 builds real window
        // recording, which is also when this tab grows a target picker).
        dry_run: false,
        window_id: None,
    }
}

/// Decode a saved capture into an iced image handle, synchronously — see
/// the module doc comment's "editor stub" section for why this doesn't need
/// the lockscreen wallpaper's async-decode treatment. `::image::open` (a
/// leading `::` forces crate-root resolution) rather than a bare
/// `image::open`, because `iced::widget::image` is already in scope as a
/// module in this file (for `image::Handle`) and would otherwise shadow the
/// `image` crate's own name for path lookups in this function.
fn load_image(path: &Path) -> Result<iced::widget::image::Handle, String> {
    let decoded = ::image::open(path)
        .map_err(|err| err.to_string())?
        .into_rgba8();
    let (width, height) = decoded.dimensions();
    Ok(iced::widget::image::Handle::from_rgba(
        width,
        height,
        decoded.into_raw(),
    ))
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

// ---------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Message {
    WindowOpened(window::Id),
    Connected(Result<Connection, String>),
    ModeSelected(CaptureMode),
    TargetSelected(ShotKind),
    DelaySelected(u32),
    CursorToggled(bool),
    FormatSelected(ImageFormat),
    PresetSelected(VideoPreset),
    AudioSelected(AudioChoice),
    Capture,
    ScreenshotFinished(Result<String, String>),
    RecordingRequested(Result<(), String>),
    DragWindow,
    ClosePressed,
}

// ---------------------------------------------------------------------
// View helpers
// ---------------------------------------------------------------------

/// §7's 46px header: the window title, drag-by-header (matching the OS
/// convention `.decorations(false)` gives up), and a text Close pill — the
/// window's only affordance for going away short of niri's own close
/// keybind, since there is no system close button with decorations off.
///
/// **`mouse_area` wrapping a `button`, and why the Close click doesn't
/// *also* start a drag**: CLAUDE.md's iced-0.14-gotchas note (found in
/// Stage 7, reused verbatim here) — "iced correctly skips [the wrapping
/// `mouse_area`'s] `on_press`] when a child button already captured" a
/// press. The Close button has `on_press` set, so it captures its own
/// clicks; every other pixel of the header bar falls through to this
/// `mouse_area` and starts a window drag.
fn header(theme: &Theme, title: &str) -> Element<'static, Message> {
    let label = text(title.to_string())
        .font(saola_theme::convert::ui_font(theme))
        .size(theme.typography.size.body)
        .color(theme.on_paper.primary.into_iced());

    let close = button(
        text("Close")
            .font(saola_theme::convert::ui_font(theme))
            .size(theme.typography.size.secondary),
    )
    .style(saola_theme::style::button::bare(theme, Surface::Paper))
    .on_press(Message::ClosePressed);

    let bar = row![label, Space::new().width(Length::Fill), close]
        .align_y(Center)
        .padding(Padding {
            top: 0.0,
            right: theme.sizes.popover_padding,
            bottom: 0.0,
            left: theme.sizes.popover_padding,
        });

    let bar = container(bar)
        .width(Length::Fill)
        .height(Length::Fixed(theme.sizes.window_header))
        .align_y(Center);

    mouse_area(bar).on_press(Message::DragWindow).into()
}

/// One uppercase Mono-500 section label (§3's "Section label" row) — iced
/// 0.14 has no letter-spacing property, so the spec's `0.12em` tracking is
/// approximated by the uppercase transform alone, which carries most of the
/// same "this is a label, not content" signal on its own.
fn section_label(theme: &Theme, label: &str) -> Element<'static, Message> {
    text(label.to_uppercase())
        .font(saola_theme::convert::mono_font_medium(theme))
        .size(theme.typography.size.label)
        .color(theme.on_paper.tertiary.into_iced())
        .into()
}

/// A row of ivory/terracotta pill buttons over a `segmented::track` — the
/// building block every closed-set control in this window uses (mode,
/// target, format, preset, audio, delay). `on_select` is a plain `Fn`
/// (never `FnOnce`), called once per option to build that option's press
/// message — the tuple-variant constructors this module's call sites pass
/// (`Message::TargetSelected`, etc.) are themselves `Fn(T) -> Message`, so
/// no closure needs to be written by hand at any call site.
fn segmented_row<T, F>(
    theme: &Theme,
    options: &[(T, &'static str)],
    selected: T,
    on_select: F,
) -> Element<'static, Message>
where
    T: Copy + PartialEq + 'static,
    F: Fn(T) -> Message + 'static,
{
    let mut track = row![];
    for &(value, label) in options {
        let is_selected = value == selected;
        let content = container(
            text(label)
                .font(saola_theme::convert::ui_font(theme))
                .size(theme.typography.size.secondary),
        )
        .align_y(Center)
        .height(Length::Fill);

        track = track.push(
            button(content)
                .height(Length::Fixed(theme.sizes.hit_target_bar))
                .padding(Padding {
                    top: 0.0,
                    right: theme.sizes.island_gap,
                    bottom: 0.0,
                    left: theme.sizes.island_gap,
                })
                .style(saola_theme::style::segmented::segment(
                    theme,
                    Surface::Paper,
                    is_selected,
                ))
                .on_press(on_select(value)),
        );
    }

    container(track)
        .style(saola_theme::style::segmented::track(theme, Surface::Paper))
        .into()
}

fn cursor_toggle(theme: &Theme, cursor: bool) -> Element<'static, Message> {
    row![
        toggler(cursor)
            .on_toggle(Message::CursorToggled)
            .style(saola_theme::style::toggles::toggler(theme, Surface::Paper)),
        text("Include cursor")
            .font(saola_theme::convert::ui_font_regular(theme))
            .size(theme.typography.size.secondary)
            .color(theme.on_paper.primary.into_iced()),
    ]
    .spacing(theme.sizes.pill_gap)
    .align_y(Center)
    .into()
}

fn capture_button(
    theme: &Theme,
    mode: CaptureMode,
    busy: bool,
    connected: bool,
) -> Element<'static, Message> {
    let label = match mode {
        CaptureMode::Screenshot => "Capture",
        CaptureMode::Record => "Start Recording",
    };

    let content = container(
        text(label)
            .font(saola_theme::convert::ui_font(theme))
            .size(theme.typography.size.body),
    )
    .width(Length::Fill)
    .align_x(Center)
    .align_y(Center)
    .height(Length::Fill);

    button(content)
        .width(Length::Fill)
        .height(Length::Fixed(theme.sizes.hit_target_touch))
        .style(saola_theme::style::button::active(theme, Surface::Paper))
        .on_press_maybe((!busy && connected).then_some(Message::Capture))
        .into()
}

fn feedback_view(theme: &Theme, feedback: &Result<String, String>) -> Element<'static, Message> {
    let message = match feedback {
        Ok(message) => message.clone(),
        Err(message) => message.clone(),
    };
    hint_view(theme, &message)
}

fn hint_view(theme: &Theme, message: &str) -> Element<'static, Message> {
    text(message.to_string())
        .font(saola_theme::convert::ui_font_regular(theme))
        .size(theme.typography.size.secondary)
        .color(theme.on_paper.secondary.into_iced())
        .into()
}

/// The (still-stub, PLAN.md Stage 9 task 3) editor view: header chrome plus
/// the decoded image at `ContentFit::Contain`, the file path in mono
/// beneath it, and a one-line note naming the stage that fills the rest in.
fn editor_view(
    theme: &Theme,
    path: &Path,
    image: &Result<iced::widget::image::Handle, String>,
) -> Element<'static, Message> {
    let picture: Element<'static, Message> = match image {
        Ok(handle) => iced::widget::image(handle.clone())
            .content_fit(iced::ContentFit::Contain)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        Err(err) => container(hint_view(
            theme,
            &format!("Could not open this image: {err}"),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Center)
        .align_y(Center)
        .into(),
    };

    let caption = text(path.display().to_string())
        .font(saola_theme::convert::mono_font(theme))
        .size(theme.typography.size.meta)
        .color(theme.on_paper.tertiary.into_iced());

    let note = hint_view(
        theme,
        "Editing tools land in Stage 14 — this is a preview only.",
    );

    let footer = column![caption, note]
        .spacing(4.0)
        .padding(theme.sizes.popover_padding);

    column![
        container(picture)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(theme.sizes.popover_padding),
        footer,
    ]
    .into()
}

// ---------------------------------------------------------------------
// Tests — pure logic only; everything else here needs a compositor and/or
// a live session bus, per CLAUDE.md's testing rule.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- window_mode_from_action --------------------------------------

    #[test]
    fn no_action_is_the_main_window() {
        assert_eq!(window_mode_from_action(None), WindowMode::Main);
    }

    #[test]
    fn edit_action_carries_the_path() {
        let action = cli::WindowAction::Edit {
            path: PathBuf::from("/tmp/shot.webp"),
        };
        assert_eq!(
            window_mode_from_action(Some(&action)),
            WindowMode::Edit(PathBuf::from("/tmp/shot.webp"))
        );
    }

    // -- resolve_capture_options ----------------------------------------

    #[test]
    fn each_target_resolves_to_its_own_shot_kind_with_no_geometry_or_window_id() {
        let config = CaptureConfig::default();
        for (target, expected) in [
            (ShotKind::Fullscreen, ShotKind::Fullscreen),
            (ShotKind::Region, ShotKind::Region),
            (ShotKind::Window, ShotKind::Window),
        ] {
            let options =
                resolve_capture_options(&config, target, 0, true, ImageFormat::Webp).unwrap();
            assert_eq!(options.kind, expected);
            assert_eq!(options.geometry, None, "the app never skips the overlay");
            assert_eq!(
                options.window_id, None,
                "the app never picks a window id directly"
            );
        }
    }

    #[test]
    fn cursor_and_delay_and_format_all_carry_through() {
        let config = CaptureConfig::default();
        let options =
            resolve_capture_options(&config, ShotKind::Fullscreen, 5, false, ImageFormat::Png)
                .unwrap();
        assert_eq!(options.delay, 5);
        assert!(!options.cursor);
        assert_eq!(options.format, ImageFormat::Png);
    }

    #[test]
    fn cursor_true_overrides_a_false_config_default() {
        let config = CaptureConfig {
            cursor: false,
            ..CaptureConfig::default()
        };
        let options =
            resolve_capture_options(&config, ShotKind::Fullscreen, 0, true, ImageFormat::Webp)
                .unwrap();
        assert!(options.cursor, "the UI's toggle state must win");
    }

    // -- record_options ---------------------------------------------------

    #[test]
    fn record_options_always_requests_start() {
        let options = record_options(VideoPreset::Av1, Some(AudioSource::Both));
        assert_eq!(options.action, cli::RecordActionKind::Start);
        assert_eq!(options.preset, VideoPreset::Av1);
        assert_eq!(options.audio, Some(AudioSource::Both));
    }

    // -- AudioChoice --------------------------------------------------------

    #[test]
    fn audio_choice_round_trips_through_option_audio_source() {
        assert_eq!(AudioChoice::NoAudio.to_option(), None);
        assert_eq!(AudioChoice::Mic.to_option(), Some(AudioSource::Mic));
        assert_eq!(AudioChoice::System.to_option(), Some(AudioSource::System));
        assert_eq!(AudioChoice::Both.to_option(), Some(AudioSource::Both));
    }

    // -- load_image ---------------------------------------------------------

    #[test]
    fn load_image_reports_a_clean_error_for_a_missing_file() {
        let result = load_image(Path::new("/nonexistent/definitely-not-a-file.webp"));
        assert!(result.is_err(), "a missing file must not panic");
    }

    // -- window_height ------------------------------------------------------

    #[test]
    fn window_height_is_taller_than_the_header_alone() {
        let theme = Theme::saola();
        assert!(window_height(&theme) > theme.sizes.window_header);
    }
}
