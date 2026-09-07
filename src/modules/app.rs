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
//! # The editor (PLAN.md Stage 9 task 3, made real by Stage 14)
//!
//! `window edit <path>` skips the picker entirely and boots straight into
//! [`ViewState::Editor`]: the same paper-window chrome, plus
//! `modules::editor::EditorState` — the crop/arrow/rectangle/ellipse/
//! freehand canvas, undo/redo, and Save/Save As/Copy. This module owns only
//! the *decoding-failed* fallback (a missing or corrupt file still opens a
//! window naming the path, per the no-panic rule) and the plumbing that
//! nests `editor::Message` into this file's own `Message::Editor` and maps
//! `editor::EditorState::view`'s `Element` the same way every other module
//! in this crate nests into its owner (`main.rs`'s `Message::Overlay`, this
//! file's own `Message::WindowOpened`, …). Stage 9's stub (a decoded image
//! at `ContentFit::Contain` and a "tools land in Stage 14" note, no canvas,
//! no save path) is what `modules::editor` replaced; see that module's own
//! doc comment for the canvas architecture, tool-state model, and raster-
//! composition performance notes.
//!
//! # §11 checklist, walked (PLAN.md Stage 9, task 4)
//!
//! **The main window's chrome** (the outer `container::window`-styled container,
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
//! 10. Added a colour? No — ink/ivory/terracotta only, `container::window`'s own
//!     ink border included.
//!
//! **The editor** (`modules::editor`, real as of Stage 14) shares the same
//! header chrome above; its own body walks §11 independently in that
//! module's doc comment — its one terracotta element is the Save button,
//! the live action, matching this window's own Capture/Start Recording
//! button precedent on the Main tab.

use std::path::{Path, PathBuf};

use iced::futures::channel::mpsc;
use iced::futures::{SinkExt, Stream, StreamExt};
use iced::widget::{
    button, column, container, mouse_area, row, rule, scrollable, text, toggler, Space,
};
use iced::{window, Center, Element, Length, Padding, Subscription, Task};
use saola_theme::{Chrome, ColorExt, Surface, Theme};
use zbus::Connection;

use crate::cli::{self, AudioSource, ShotKind};
use crate::config::{CaptureConfig, ImageFormat, VideoPreset};
use crate::dbus::Capture1Proxy;
use crate::modules::{editor, history, picker};

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
///
/// **Stage 14**: the Main tab's fixed, non-resizable footprint (sized by
/// [`window_height`], a small multiple of `sizes.list_row`) is the wrong
/// shape for the editor — a screenshot-sized canvas wants real room, and a
/// user cropping/annotating benefits from being able to enlarge the window.
/// [`WindowMode::Edit`] therefore picks a different, resizable starting size
/// ([`editor_window_size`]) instead of reusing the Main tab's; `mode` is
/// already available here (a plain enum, not yet boxed into the boot
/// closure), so the branch costs nothing.
pub fn run(mode: WindowMode) -> iced::Result {
    let theme = Theme::saola();
    let default_font = saola_theme::convert::ui_font(&theme);
    let (size, resizable) = match &mode {
        WindowMode::Main => (
            iced::Size::new(theme.sizes.popover_width, window_height(&theme)),
            false,
        ),
        WindowMode::Edit(_) => (editor_window_size(&theme), true),
    };

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
        // anything, which would square off `container::window`'s rounded
        // corners against a rectangle nobody asked for. That finding was
        // about `iced_layershell` specifically; applying the same defensive
        // pair here is cheap and untested-but-consistent — see the Stage 9
        // handoff for what live-checking this actually confirmed.
        .transparent(true)
        .resizable(resizable)
        .centered()
        .window_size(size)
        .settings(iced::Settings {
            default_font,
            ..iced::Settings::default()
        })
        .run()
}

/// The editor's own starting window footprint — a second instance of the
/// same gap [`window_height`]'s own doc comment names ("no style-guide
/// token sizes a utility window... directly"), derived the same way: real
/// token multiples, not bare literals. Wide enough to show a mid-size
/// screenshot at a legible scale alongside its toolbar; tall enough for the
/// toolbar rows, canvas and footer without immediately needing the resize
/// this stage also turns on. Not a hard limit — the window is resizable
/// (see [`run`]), so this is a starting point, not a ceiling.
fn editor_window_size(theme: &Theme) -> iced::Size {
    iced::Size::new(
        theme.sizes.popover_width * 3.0,
        theme.sizes.window_header * 2.0 + theme.sizes.list_row * 10.0,
    )
}

/// No style-guide token sizes a utility window's height directly (§4's
/// `Sizes` table covers popover/launcher/notification-card *widths*, never
/// a settings-style window) — derived the same way `modules::toast::
/// card_height` derives its own undocumented height: a small multiple of
/// `sizes.list_row` (roughly one row per control group) plus the header and
/// generous padding, rather than a bare literal. If the content overflows
/// this estimate, [`App::main_view`] wraps everything in a `scrollable` —
/// this is a starting size, not a hard clip.
///
/// **The multiple is 12, not the original 8** (found by screenshotting the
/// real window, 2026-08-09): the Screenshot tab alone is four labelled
/// segmented rows plus a toggle plus the Capture button, which already
/// overflowed 8 rows — the primary action was *cut in half* by the bottom
/// window edge on first open, with the scrollbar as the only way to reach
/// it. The Record tab is taller still (two extra labelled rows: Preset and
/// Audio), and since the window is deliberately not resizable and the mode
/// tabs switch in place, the height has to fit the **taller** of the two or
/// pressing "Record" would push Start Recording back under the fold. 12
/// rows fits both with room for the feedback line.
fn window_height(theme: &Theme) -> f32 {
    theme.sizes.window_header + 12.0 * theme.sizes.list_row + 4.0 * theme.sizes.popover_padding
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

    /// **Stage 13.** The inverse, for seeding the picker from
    /// `capture.toml`'s own `audio` knob at boot — so the window opens on
    /// whatever a bare `record start` would have done, exactly like the
    /// delay/cursor/format/preset pickers already do.
    fn from_option(source: Option<AudioSource>) -> Self {
        match source {
            None => AudioChoice::NoAudio,
            Some(AudioSource::Mic) => AudioChoice::Mic,
            Some(AudioSource::System) => AudioChoice::System,
            Some(AudioSource::Both) => AudioChoice::Both,
        }
    }
}

/// Which screen this process is showing — set once at boot from
/// [`WindowMode`] for `Main`/`Editor` (there is still no in-app navigation
/// *into* the editor; a toast click or `window edit` spawns a whole new
/// process for it, per the module doc comment's own reasoning for why that
/// split exists — an edit target is a whole separate document).
///
/// **Stage 16 adds one real exception**: [`ViewState::History`] *is*
/// reached by in-app navigation (`Message::HistoryRequested`/
/// `Message::HistoryClosed`, `main_view`'s History button), toggled within
/// this same running process rather than spawning a new one. The
/// distinction that makes this the right call and not a quiet violation of
/// the rule above: browsing history has no "document" of its own the way an
/// edit does — it's a view over data this same window can load, act on, and
/// discard, not a separate file another process should own the lifetime of.
enum ViewState {
    Main,
    Editor {
        path: PathBuf,
        /// Built once, at boot, by [`editor::EditorState::load`]. `Err`
        /// renders as an inline message rather than failing to open at all —
        /// a missing or corrupt file is not a reason to crash a window that
        /// could still usefully show the path and let the user close it
        /// (no-panic rule). Boxed: `EditorState` carries the whole
        /// undo/redo-capable document (an `EditorModel` plus a cached
        /// display handle), which made this variant far larger than
        /// `ViewState::Main` — `clippy::large_enum_variant` flags exactly
        /// that, since every `ViewState` value would otherwise pay the
        /// bigger variant's stack size even while sitting in `Main`.
        editor: Result<Box<editor::EditorState>, String>,
    },
    /// **Stage 16.** Boxed for the same `large_enum_variant` reason
    /// `Editor`'s payload is — `history::HistoryModel` carries a whole
    /// loaded library plus per-row decoded thumbnails, which is not a cost
    /// every `ViewState::Main` value should pay to make room for.
    History(Box<history::HistoryModel>),
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
    /// **Stage 12.** The Record tab's own target picker — mirrors
    /// `target` above, but a separate field/type (`cli::RecordKind`, not
    /// `ShotKind` — see that type's doc comment for why) since the two tabs
    /// resolve independently and a mode switch must not clobber either.
    record_target: cli::RecordKind,
    delay: u32,
    cursor: bool,
    format: ImageFormat,
    preset: VideoPreset,
    audio: AudioChoice,
    /// `true` from the moment Capture/Start Recording is pressed until its
    /// D-Bus reply lands — guards against a second press re-hiding an
    /// already-hidden window mid-request.
    busy: bool,
    /// **Stage 12.** `true` from the moment `StartRecording` succeeds (the
    /// recording is *live*, not finished — see the module doc comment's
    /// hide/reopen section) until `RecordingFinished`/`Error` arrives on
    /// [`record_signal_stream`]. `busy` alone can't carry this: it is set the
    /// instant the button is pressed and `Message::RecordingRequested`'s own
    /// arrival would otherwise clear it (the shape every other capture kind
    /// wants), so this is the second flag that keeps the window hidden
    /// *past* that reply, specifically for a recording in flight.
    recording_pending: bool,
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
                editor: editor::EditorState::load(
                    path,
                    &theme,
                    config.image_format,
                    config.webp_quality,
                )
                .map(Box::new),
            },
        };

        let state = App {
            target: ShotKind::Fullscreen,
            record_target: cli::RecordKind::Fullscreen,
            delay: config.delay,
            cursor: config.cursor,
            format: config.image_format,
            preset: config.video_preset,
            audio: AudioChoice::from_option(config.audio),
            theme,
            config,
            connection: None,
            window_id: None,
            view,
            mode: CaptureMode::Screenshot,
            busy: false,
            recording_pending: false,
            feedback: None,
        };

        (state, Task::perform(connect(), Message::Connected))
    }

    fn title(&self) -> String {
        match &self.view {
            ViewState::Main => "Saola Capture".to_string(),
            // A successful Save As (Stage 14) changes what `editor::
            // EditorState` itself considers "the" path, and the title bar
            // should track that rather than staying pinned to whatever
            // `window edit <path>` was originally launched with — otherwise
            // renaming via Save As leaves the header naming a file that's no
            // longer where "Save" (as opposed to "Save As") actually writes.
            ViewState::Editor {
                editor: Ok(state), ..
            } => {
                format!("Saola Capture — {}", file_label(state.path()))
            }
            ViewState::Editor {
                path,
                editor: Err(_),
            } => {
                format!("Saola Capture — {}", file_label(path))
            }
            ViewState::History(_) => "Saola Capture — History".to_string(),
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

    /// Two jobs: learn this process's one window's `Id`, the first time it
    /// opens, and — **Stage 12** — listen for `RecordingFinished`/`Error` on
    /// [`record_signal_stream`]. The latter runs for this window's whole
    /// life (not just while a recording is pending) because it is a plain
    /// zero-argument `fn` pointer, exactly like `main.rs`'s own
    /// `dbus_worker_stream`/`shutdown_signal_stream` — `Subscription::
    /// run_with` would need `zbus::Connection` (or anything holding one) to
    /// be `Hash` to key a *per-connection* subscription, and it isn't (the
    /// module doc comment's "hide/reopen model" section explains why that
    /// was left for this stage). Running it unconditionally, and having
    /// [`App::update`] ignore a signal that arrives while nothing is
    /// pending, is simpler than tearing the subscription down and back up
    /// around every recording.
    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            window::open_events().map(Message::WindowOpened),
            Subscription::run(record_signal_stream),
        ])
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
            Message::RecordTargetSelected(target) => {
                self.record_target = target;
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
            // **Stage 12.** `StartRecording` returning `Ok` means the
            // recording is *live*, not finished (see the module doc
            // comment) — stay hidden and wait for the signal below instead
            // of calling `finish` here. An `Err` (already recording,
            // `--audio` refused, no ffmpeg, …) is the request itself
            // failing, which *is* terminal, so that branch un-hides exactly
            // like every other capture kind's failure does.
            Message::RecordingRequested(Ok(())) => {
                self.recording_pending = true;
                self.feedback = Some(Ok("Recording…".to_string()));
                Task::none()
            }
            Message::RecordingRequested(Err(err)) => self.finish(Err(err)),
            Message::RecordingFinishedSignal(path) => {
                if self.recording_pending {
                    self.recording_pending = false;
                    self.finish(Ok(format!("Recording saved: {path}")))
                } else {
                    // A recording finished that this window didn't start
                    // (the CLI, the tray) — nothing to reopen for.
                    Task::none()
                }
            }
            Message::RecordingErrorSignal(message) => {
                if self.recording_pending {
                    self.recording_pending = false;
                    self.finish(Err(message))
                } else {
                    Task::none()
                }
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
            // **Stage 14.** The nested-`Message` shape every module in this
            // crate uses (`main.rs`'s `Message::Overlay`, this file's own
            // `Message::WindowOpened`, …): delegate to the editor's own
            // `update`, mapping its `Task<editor::Message>` back into this
            // window's `Task<Message>`. A no-op if the editor failed to
            // load (`ViewState::Editor { editor: Err(_), .. }`) or this
            // process isn't even showing the editor — neither is reachable
            // in practice (nothing renders editor controls in that state),
            // but the no-panic rule wants a real branch, not an `unwrap`.
            Message::Editor(message) => match &mut self.view {
                ViewState::Editor {
                    editor: Ok(state), ..
                } => state.update(message).map(Message::Editor),
                _ => Task::none(),
            },
            // **Stage 16.** Loading the library is a handful of file reads
            // plus (for however many screenshots are in it) a WebP/PNG
            // decode each — done synchronously here, on `update`'s own call
            // stack, not through `Task::perform`. Recorded rather than
            // silently accepted: `modules::history`'s own doc comment
            // doesn't cover this cost, and a very large history could make
            // this button visibly stall the window the same class of way
            // `modules::editor`'s worst-case Blur drag does (Stage 15's own
            // recorded-not-fixed finding) — see the Stage 16 handoff.
            Message::HistoryRequested => {
                self.view = ViewState::History(Box::new(history::HistoryModel::load(&self.config)));
                Task::none()
            }
            Message::HistoryClosed => {
                self.view = ViewState::Main;
                Task::none()
            }
            Message::History(message) => match &mut self.view {
                ViewState::History(model) => {
                    let action = model.update(message);
                    self.apply_history_action(action)
                }
                _ => Task::none(),
            },
            Message::PickColorRequested => self.start_pick_color(),
            Message::PickColorFinished(result) => self
                .finish(result.map(|(r, g, b)| format!("Picked {}", picker::rgb_to_hex(r, g, b)))),
        }
    }

    /// Turns a [`history::Action`] into whatever cross-process/background
    /// effect it names — the same "child model returns a value, the parent
    /// interprets it" shape `Message::Toast`'s handling in `main.rs`'s
    /// `Daemon::update` already established for [`crate::modules::toast::
    /// Action`], applied here instead of inline in the `Message::History`
    /// arm above purely so that arm's `match` doesn't have to nest this
    /// deeply.
    fn apply_history_action(&mut self, action: history::Action) -> Task<Message> {
        match action {
            history::Action::None => Task::none(),
            history::Action::Edit(path) => {
                // Same fire-and-forget posture `main.rs::Daemon::update`'s
                // own `modules::toast::Action::Open` arm takes: a spawn
                // failure here is vanishingly unlikely (a missing
                // `current_exe()`, in practice) and there is no existing
                // "surface an error on the History screen" message worth
                // inventing solely for it — logged, not silently dropped.
                if let Err(err) = crate::spawn_editor(&path) {
                    eprintln!(
                        "saola-capture: window: could not open the editor for {}: {err}",
                        path.display()
                    );
                }
                Task::none()
            }
            history::Action::OpenDir(path) => {
                if let Err(err) = crate::open_containing_dir(&path) {
                    eprintln!(
                        "saola-capture: window: could not open the folder containing {}: {err}",
                        path.display()
                    );
                }
                Task::none()
            }
            history::Action::Export { source, format } => {
                let path_for_message = source.clone();
                Task::perform(run_export(source, format), move |result| {
                    Message::History(history::Message::ExportFinished(
                        path_for_message.clone(),
                        result,
                    ))
                })
            }
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
                let options = record_options(
                    &self.config,
                    self.record_target,
                    self.preset,
                    self.audio.to_option(),
                );
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

    /// **Stage 16.** The same hide-then-ask-the-daemon shape
    /// [`Self::start_capture`] uses, for `PickColor` — hiding the window
    /// while picking matters more here than for a screenshot: with the
    /// window still up, it could sit directly over the very pixel the user
    /// is trying to click on niri's own eyedropper cursor.
    fn start_pick_color(&mut self) -> Task<Message> {
        if self.busy {
            return Task::none();
        }
        let Some(connection) = self.connection.clone() else {
            self.feedback = Some(Err(
                "not connected to the daemon yet — try again in a moment".to_string(),
            ));
            return Task::none();
        };

        self.busy = true;
        self.feedback = None;
        let hide = match self.window_id {
            Some(id) => window::set_mode(id, window::Mode::Hidden),
            None => Task::none(),
        };
        let request = Task::perform(request_pick_color(connection), Message::PickColorFinished);
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
            ViewState::Editor {
                editor: Ok(state), ..
            } => state.view(theme).map(Message::Editor),
            ViewState::Editor {
                path,
                editor: Err(err),
            } => editor_error_view(theme, path, err),
            ViewState::History(model) => column![
                history_back_row(theme),
                model.view(theme).map(Message::History),
            ]
            .into(),
        };

        let content = column![head, divider, body];

        // `Length::Fill`, not the Main tab's old `Fixed(popover_width)`: the
        // Main window is genuinely fixed-size (`run`'s `resizable(false)`),
        // so the two were indistinguishable there, but the editor window is
        // resizable (Stage 14) and wider than `popover_width` to begin with
        // — a `Fixed` width here would silently clip `editor::EditorState::
        // view`'s own canvas/toolbar to the Main tab's narrow footprint.
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(saola_theme::style::container::window(theme, Surface::Paper))
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
                // **Stage 12.** Same three targets the Screenshot tab
                // offers, same "never skips the overlay, never names an
                // explicit window" posture (`record_options`'s doc
                // comment) — a Region press hides the window and lets the
                // daemon map the selection overlay, exactly like
                // `shot --region` does.
                sections.push(section_label(theme, "Target"));
                sections.push(segmented_row(
                    theme,
                    &[
                        (cli::RecordKind::Fullscreen, "Full screen"),
                        (cli::RecordKind::Region, "Region"),
                        (cli::RecordKind::Window, "Window"),
                    ],
                    self.record_target,
                    Message::RecordTargetSelected,
                ));

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

                // **Real as of Stage 13** — Architecture's "audio
                // (mic/system/both, Opus)". The picker was already wired
                // through `RecordOptions::to_dbus_options` in Stage 9; what
                // changed is that the daemon now resolves it to real pulse
                // sources instead of refusing the call. Its initial value
                // comes from `capture.toml`'s `audio` knob (`App::boot`), and
                // a device that isn't there degrades the recording to
                // video-only with a warning toast rather than failing the
                // button (`dbus::CaptureService::resolve_audio`) — so there
                // is deliberately no availability check in this process,
                // which would only be a second, staler answer.
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

        // **Stage 16.** Two secondary actions, neither a capture — both
        // `button::rest` (§11's "exactly one terracotta element" stays
        // Capture/Start Recording's, the live action on this screen).
        sections.push(section_label(theme, "More"));
        sections.push(
            row![
                secondary_button(theme, "History", true, Some(Message::HistoryRequested)),
                secondary_button(
                    theme,
                    "Pick Color",
                    !self.busy && self.connection.is_some(),
                    Some(Message::PickColorRequested),
                ),
            ]
            .spacing(theme.sizes.pill_gap)
            .into(),
        );

        if let Some(feedback) = &self.feedback {
            sections.push(feedback_view(theme, feedback));
        } else if self.connection.is_none() {
            sections.push(hint_view(theme, "Connecting to the daemon…"));
        }

        let mut list = column![].spacing(theme.sizes.island_gap);
        for section in sections {
            list = list.push(section);
        }

        // The style is not optional decoration: an unstyled `scrollable`
        // renders iced's *default* scrollbar, which is a near-black rail
        // that ignores the theme entirely and paints straight over
        // `container::window`'s rounded corner. `saola_theme::style::scrollable::
        // rest` is the surface-aware answer (track-role rail, ivory thumb,
        // terracotta while dragged) and already existed in v0.5.0 — it was
        // simply never wired up here.
        scrollable(list.padding(theme.sizes.popover_padding))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(saola_theme::style::scrollable::rest(theme, Surface::Paper))
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
    proxy
        .start_recording(options.kind.as_str(), options.to_dbus_options())
        .await
        .map_err(|err| err.to_string())
}

/// **Stage 16.** `PickColor` from the app window — the exact same daemon
/// call `run_pick_color` (`main.rs`) makes for the CLI, over this window's
/// own connection. The daemon does the clipboard copy and raises the swatch
/// toast on its own (`dbus.rs::CaptureService::pick_color`); this call's
/// return value is only this window's own feedback line.
async fn request_pick_color(connection: Connection) -> Result<(f64, f64, f64), String> {
    let proxy = Capture1Proxy::new(&connection)
        .await
        .map_err(|err| err.to_string())?;
    proxy.pick_color().await.map_err(|err| err.to_string())
}

/// **Stage 16.** Runs a GIF/animated-WebP export off iced's executor thread
/// — see [`run_blocking`]'s own doc comment for why this is a fourth copy of
/// that guard rather than a shared one.
async fn run_export(
    source: PathBuf,
    format: crate::encode::export::AnimatedFormat,
) -> Result<(PathBuf, u64), String> {
    run_blocking(move || {
        crate::encode::export::export_with_size(&source, format).map_err(|err| err.to_string())
    })
    .await
}

/// The one blocking-work guard this file needs — see `modules::editor`'s
/// identically named, identically shaped private function's own doc comment
/// for why this is a deliberate small duplication (one per module that needs
/// it) rather than a shared helper: `dbus::run_blocking` is private to the
/// daemon process, `editor::run_blocking` private to that module, and this
/// is the window process's own copy for [`run_export`].
async fn run_blocking<T, F>(work: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(_) => match tokio::task::spawn_blocking(work).await {
            Ok(result) => result,
            Err(err) => Err(format!("the export task did not finish: {err}")),
        },
        Err(_) => work(),
    }
}

/// **Stage 12.** A standing listener for the two signals that mark a
/// recording's real end — `RecordingFinished(path)` and `Error(message)` —
/// on the *daemon's* `io.saola.Capture1` object, over this window's own
/// independent connection (never the one [`connect`] built: a subscription's
/// stream is `'static` and outlives any particular `Task::perform`, so it
/// has to own its own connection start to finish, the same reason
/// `main.rs::dbus_worker_stream` builds its own rather than borrowing the
/// daemon's).
///
/// A zero-argument `fn` pointer, not a closure — see [`App::subscription`]'s
/// doc comment for why that's what lets this run without `zbus::Connection`
/// needing to be `Hash`. Any failure to connect or subscribe (no session
/// bus, the daemon not owning the name yet) degrades to "never fires" —
/// [`App::update`]'s `Message::RecordingRequested(Ok(()))` arm already knows
/// a recording it started stays pending forever in that case, which is an
/// honest (if quiet) failure mode rather than a panic, matching every other
/// D-Bus-reachability failure in this crate.
fn record_signal_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(4, async |mut sender: mpsc::Sender<Message>| {
        let Ok(connection) = Connection::session().await else {
            return;
        };
        let Ok(proxy) = Capture1Proxy::new(&connection).await else {
            return;
        };
        let (Ok(mut finished), Ok(mut errors)) = (
            proxy.receive_recording_finished().await,
            proxy.receive_error().await,
        ) else {
            return;
        };

        loop {
            let message = tokio::select! {
                signal = finished.next() => {
                    let Some(signal) = signal else { break; };
                    // A malformed body (a daemon version skew, in theory) is
                    // dropped rather than ending the whole listener — the
                    // same "degrade, don't die" posture as a connect
                    // failure above.
                    let Ok(args) = signal.args() else { continue; };
                    Message::RecordingFinishedSignal(args.path().clone())
                }
                signal = errors.next() => {
                    let Some(signal) = signal else { break; };
                    let Ok(args) = signal.args() else { continue; };
                    Message::RecordingErrorSignal(args.message().clone())
                }
            };
            if sender.send(message).await.is_err() {
                break;
            }
        }
    })
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
/// `ShotArgs`-style synthetic-args detour needed here since the knobs the UI
/// actually tracks (target, preset, audio) map straight across. Everything
/// else comes from `config` unchanged, which is the same thing
/// `cli::RecordOptions::resolve` does for the `record` verb — **Stage 11**
/// added three such fields (`cursor`, `output_dir`, `vaapi_device`), and they
/// are read from the same `capture.toml` this process already loaded at boot
/// rather than re-derived, so the app window and the CLI start identical
/// recordings.
///
/// **Stage 12** adds `target`: like the Screenshot tab's own target picker,
/// the app never skips the overlay (`geometry: None`) and never names an
/// explicit window (`window_id: None`) — a `Region` press hides the window
/// and lets the daemon map the same selection overlay `shot --region` does;
/// a `Window` press records whichever window is focused (CAPTURE-RESEARCH
/// D3's no-picker default).
fn record_options(
    config: &CaptureConfig,
    target: cli::RecordKind,
    preset: VideoPreset,
    audio: Option<AudioSource>,
) -> cli::RecordOptions {
    cli::RecordOptions {
        action: cli::RecordActionKind::Start,
        kind: target,
        preset,
        audio,
        // The app window starts *real* recordings; `--dry-run` (Stage 10) is
        // a terminal diagnostic that never reaches the daemon, so there is
        // nothing here for it to mean.
        // **Stage 13.** The device knobs and the A/V calibration offset come
        // from the same `capture.toml` this process loaded at boot, for the
        // same reason `cursor`/`output_dir`/`vaapi_device` do: the daemon
        // reads no config of its own, so whatever the app window does not
        // send, the recording does not get.
        audio_mic_source: config.audio_mic_source.clone(),
        audio_system_source: config.audio_system_source.clone(),
        audio_offset: config.audio_offset,
        dry_run: false,
        geometry: None,
        window_id: None,
        cursor: config.cursor,
        output_dir: config.save_dir.clone(),
        vaapi_device: config.vaapi_device.clone(),
    }
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
    /// **Stage 12.**
    RecordTargetSelected(cli::RecordKind),
    DelaySelected(u32),
    CursorToggled(bool),
    FormatSelected(ImageFormat),
    PresetSelected(VideoPreset),
    AudioSelected(AudioChoice),
    Capture,
    ScreenshotFinished(Result<String, String>),
    RecordingRequested(Result<(), String>),
    /// **Stage 12.** `RecordingFinished(path)` arrived on
    /// [`record_signal_stream`].
    RecordingFinishedSignal(String),
    /// **Stage 12.** `Error(message)` arrived on [`record_signal_stream`].
    RecordingErrorSignal(String),
    DragWindow,
    ClosePressed,
    /// **Stage 14.** Nests the whole editor surface's own message type — see
    /// `Message::Editor`'s `update` arm.
    Editor(editor::Message),
    /// **Stage 16.** The Main tab's "History" button — loads the library and
    /// switches `view` to [`ViewState::History`]; see that variant's own doc
    /// comment for why this is genuine in-app navigation rather than a
    /// spawned process.
    HistoryRequested,
    /// The History screen's own "Back" button — returns to
    /// [`ViewState::Main`]. No daemon call, no `Task` — a plain state change,
    /// which is why (unlike `Capture`) this doesn't go through `start_capture`
    /// /`finish`'s hide-and-reshow machinery at all.
    HistoryClosed,
    /// Nests [`history::Message`] — the same "delegate to the child model's
    /// own `update`, translate its `Action`" shape `Message::Editor` already
    /// uses, except `history::HistoryModel::update` returns a value
    /// ([`history::Action`]) instead of a `Task`, so this arm interprets that
    /// value itself rather than just mapping a `Task`.
    History(history::Message),
    /// **Stage 16.** The Main tab's "Pick Color" button.
    PickColorRequested,
    /// `PickColor`'s D-Bus reply — the daemon already did the clipboard copy
    /// and raised the swatch toast (`dbus.rs::CaptureService::pick_color`);
    /// this is purely this window's own feedback line and hide/reshow, the
    /// same shape [`Message::ScreenshotFinished`] already has.
    PickColorFinished(Result<(f64, f64, f64), String>),
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
    let mut track = row![].spacing(theme.sizes.segment_inset);
    for &(value, label) in options {
        let is_selected = value == selected;
        let content = container(
            text(label)
                .font(saola_theme::convert::ui_font(theme))
                .size(theme.typography.size.secondary),
        )
        // Teaching note: `button` does no alignment of its own in iced 0.14
        // — `iced_core::layout::padded` places the content flush at
        // (padding.left, padding.top) — so a label is centred only because
        // *this* container centres it. Horizontally the segment hugs its
        // label (no explicit button width), which centres it by
        // construction; `align_x` is set anyway so the intent survives if a
        // segment ever gets a fixed width. Vertically it is load-bearing:
        // the button is a fixed `hit_target_bar` tall with zero vertical
        // padding, so without `height(Fill)` + `align_y` the label would sit
        // against the pill's top edge.
        .align_x(Center)
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
                    // The app window's own controls are `Chrome::Window` —
                    // visually a no-op on `Surface::Paper` (the two chromes
                    // are identical there), the correct variant if an ink
                    // app-window mode ever ships.
                    Chrome::Window,
                    is_selected,
                ))
                .on_press(on_select(value)),
        );
    }

    container(track)
        .padding(theme.sizes.segment_inset)
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

/// A `button::rest` pill for a secondary (non-capture) action — **Stage
/// 16**'s History/Pick Color buttons. `enabled: false` disables the button
/// (`on_press_maybe`) rather than hiding it, matching every other
/// availability gate on this screen (`capture_button`'s own `busy`/
/// `connected` gate).
fn secondary_button(
    theme: &Theme,
    label: &str,
    enabled: bool,
    on_press: Option<Message>,
) -> Element<'static, Message> {
    let content = container(
        text(label.to_string())
            .font(saola_theme::convert::ui_font_regular(theme))
            .size(theme.typography.size.secondary),
    )
    .align_x(Center)
    .align_y(Center)
    .height(Length::Fill);

    button(content)
        .height(Length::Fixed(theme.sizes.hit_target_bar))
        // `Chrome::Window`: this button lives inside the app window, not
        // shell chrome. A no-op on `Surface::Paper` today (the two chromes
        // are identical there) — the correct variant if an ink app-window
        // mode ever ships.
        .style(saola_theme::style::button::rest(
            theme,
            Surface::Paper,
            Chrome::Window,
        ))
        .on_press_maybe(enabled.then_some(on_press).flatten())
        .into()
}

fn history_back_row(theme: &Theme) -> Element<'static, Message> {
    let back = button(
        text("← Back")
            .font(saola_theme::convert::ui_font_regular(theme))
            .size(theme.typography.size.secondary),
    )
    .style(saola_theme::style::button::rest(
        theme,
        Surface::Paper,
        Chrome::Window,
    ))
    .on_press(Message::HistoryClosed);

    container(back)
        .padding(Padding {
            top: theme.sizes.pill_gap,
            right: theme.sizes.popover_padding,
            bottom: 0.0,
            left: theme.sizes.popover_padding,
        })
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

/// **Stage 14.** The one case left in this file's own hands: `path` failed
/// to decode at all (`modules::editor::EditorState::load` returned `Err`) —
/// a missing or corrupt file still opens a window naming the path, rather
/// than crashing (no-panic rule). A successfully loaded editor renders
/// through `editor::EditorState::view` instead; this is not that view.
fn editor_error_view(theme: &Theme, path: &Path, err: &str) -> Element<'static, Message> {
    let caption = text(path.display().to_string())
        .font(saola_theme::convert::mono_font(theme))
        .size(theme.typography.size.meta)
        .color(theme.on_paper.tertiary.into_iced());

    let message = hint_view(theme, &format!("Could not open this image: {err}"));

    container(
        column![caption, message]
            .spacing(theme.sizes.gap_tight)
            .padding(theme.sizes.popover_padding),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Center)
    .align_y(Center)
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
        let options = record_options(
            &CaptureConfig::default(),
            cli::RecordKind::Fullscreen,
            VideoPreset::Av1,
            Some(AudioSource::Both),
        );
        assert_eq!(options.action, cli::RecordActionKind::Start);
        assert_eq!(options.kind, cli::RecordKind::Fullscreen);
        assert_eq!(options.preset, VideoPreset::Av1);
        assert_eq!(options.audio, Some(AudioSource::Both));
    }

    #[test]
    fn record_options_carries_the_chosen_target_with_no_geometry_or_window_id() {
        for target in [
            cli::RecordKind::Fullscreen,
            cli::RecordKind::Region,
            cli::RecordKind::Window,
        ] {
            let options =
                record_options(&CaptureConfig::default(), target, VideoPreset::Hevc, None);
            assert_eq!(options.kind, target);
            assert_eq!(options.geometry, None, "the app never skips the overlay");
            assert_eq!(
                options.window_id, None,
                "the app never picks a window id directly"
            );
        }
    }

    // -- AudioChoice --------------------------------------------------------

    #[test]
    fn audio_choice_round_trips_through_option_audio_source() {
        assert_eq!(AudioChoice::NoAudio.to_option(), None);
        assert_eq!(AudioChoice::Mic.to_option(), Some(AudioSource::Mic));
        assert_eq!(AudioChoice::System.to_option(), Some(AudioSource::System));
        assert_eq!(AudioChoice::Both.to_option(), Some(AudioSource::Both));
    }

    // `load_image` moved to `modules::editor::load_canvas` in Stage 14 —
    // that module's own tests cover the missing-file case now.

    // -- window_height ------------------------------------------------------

    #[test]
    fn window_height_is_taller_than_the_header_alone() {
        let theme = Theme::saola();
        assert!(window_height(&theme) > theme.sizes.window_header);
    }

    // -- editor_window_size ---------------------------------------------------

    #[test]
    fn editor_window_size_is_wider_than_the_main_popover() {
        let theme = Theme::saola();
        let size = editor_window_size(&theme);
        assert!(size.width > theme.sizes.popover_width);
        assert!(size.height > theme.sizes.window_header);
    }
}
