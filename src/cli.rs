//! Argument parsing (`clap`, Stage 1's pick) and CLI-flag-over-config
//! resolution (PLAN.md Stage 3, task 2).
//!
//! # The split this module keeps (teaching note)
//!
//! [`Cli`]/[`Command`]/[`ShotArgs`]/[`RecordArgs`]/[`WindowAction`] are the
//! *argv shape* — `clap`'s derive macro turns them straight into a parser,
//! `--help` text and all. Everything below the `// -- resolution --`
//! marker is the *meaning* of what was parsed: [`CaptureOptions`] and
//! [`RecordOptions`] are plain data, built by folding a parsed args struct
//! over a [`crate::config::CaptureConfig`] (flags win, the config file is
//! the fallback, PLAN.md's own stated precedence). Keeping the fold as pure
//! functions (`CaptureOptions::resolve`, `RecordOptions::resolve`) rather
//! than mixing it into `main`'s dispatch is what makes flag-vs-config
//! precedence unit-testable without spawning a process or touching argv —
//! every test below builds a `ShotArgs`/`RecordArgs` value directly, the
//! same way `saola-panel::config`'s tests build a `CliOverrides` directly
//! instead of calling `std::env::args()`.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use zbus::zvariant::OwnedValue;

use crate::config::{CaptureConfig, ImageFormat, VideoPreset};

/// Teaching note: `clap`'s `derive` feature turns this struct into a full
/// argument parser at compile time. `Cli::parse()` in `main` reads
/// `std::env::args()` and — on `--help`, `--version`, or a malformed
/// invocation — prints the right text and exits the process itself
/// (clap's own graceful exit path, not a `panic!`/`unwrap` on a runtime
/// path, so CLAUDE.md's no-panic rule is untouched by it).
#[derive(Parser, Debug)]
#[command(
    name = "saola-capture",
    version,
    about = "Screenshots and screen recording for the Saola desktop environment."
)]
pub struct Cli {
    /// Read `capture.toml` from this directory instead of the
    /// `$SAOLA_CONFIG_DIR`/XDG search path — heads
    /// `config::CaptureConfig::resolve_path`'s precedence chain. `global =
    /// true` so it can appear either before or after the subcommand
    /// (`saola-capture --config-dir ~/scratch shot --fullscreen` and
    /// `saola-capture shot --config-dir ~/scratch --fullscreen` both work),
    /// matching how `saola-panel` exposes the same knob.
    #[arg(long, global = true, value_name = "dir")]
    pub config_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

/// The three run modes from `CLAUDE.md`, flattened into one dispatch enum.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the long-lived layer-shell daemon: selection overlay, camera
    /// flash, toast stack, tray item, capture engine, recording pipeline,
    /// and the `io.saola.Capture1` bus name.
    Daemon,
    /// Open the separate-process app window: main window, history library,
    /// annotation editor.
    Window {
        #[command(subcommand)]
        action: Option<WindowAction>,
    },
    /// Take a screenshot (fullscreen, region, or window) via the daemon, or
    /// in-process and headless with `--no-daemon`.
    Shot(ShotArgs),
    /// Start, stop, or toggle screen recording.
    Record(RecordArgs),
    /// Pick a color from the screen and copy its hex value.
    PickColor,
    /// Open the app window (CLI convenience verb, same as `window` with no
    /// further action).
    Open,
    /// **Internal.** Serve one blob of bytes as the Wayland clipboard
    /// selection until something else takes it over, reading the blob from
    /// stdin. Hidden from `--help`: it is an implementation detail of the
    /// `--no-daemon` capture path, not a verb anyone should type.
    ///
    /// # Why this exists at all (teaching note)
    ///
    /// A Wayland clipboard "copy" is not a write into a shared buffer: the
    /// copying client keeps a `wl_data_source` alive and *serves* the bytes
    /// on demand every time something pastes. When that client exits, the
    /// selection dies with it. `wl-clipboard-rs` handles this by spawning a
    /// background **thread** that serves requests — which is exactly right
    /// inside the long-lived daemon, and useless inside a `shot
    /// --no-daemon` process that is about to exit.
    ///
    /// So the short-lived path spawns *this* verb, detached, and hands it
    /// the bytes down a pipe. It is the same shape `wl-copy` uses (which is
    /// why `wl-copy` appears to "return" while the clipboard keeps working),
    /// implemented in-process so the clipboard doesn't become a third
    /// external-binary dependency — see the `wl-clipboard-rs` survey in
    /// `Cargo.toml` for why spawning `wl-copy` itself was rejected.
    #[command(hide = true)]
    ClipboardServe(ClipboardServeArgs),
}

/// Arguments for the hidden [`Command::ClipboardServe`] verb.
#[derive(Args, Debug, Clone)]
pub struct ClipboardServeArgs {
    /// The MIME type to offer the bytes under (`image/png`).
    #[arg(long, value_name = "type")]
    pub mime: String,
}

/// `saola-capture window edit <path>` — jump straight into the annotation
/// editor with a saved capture loaded, the toast-click flow's target
/// (Architecture: "Toast click: spawn detached `saola-capture window edit
/// <path>`"). No subcommand at all opens the plain main window.
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum WindowAction {
    /// Open the annotation editor on an existing capture file.
    Edit { path: PathBuf },
}

impl WindowAction {
    /// The `OpenWindow(mode s)` D-Bus argument this action resolves to —
    /// `"main"` for the plain window, `"edit:<path>"` for the editor. A
    /// free function of `Option<&WindowAction>` rather than a method on
    /// `Option` itself, since `None` (no action given) is a meaningful
    /// third case with its own string, not an absence to `unwrap_or`
    /// around.
    pub fn dbus_mode(action: Option<&WindowAction>) -> String {
        match action {
            None => "main".to_string(),
            Some(WindowAction::Edit { path }) => format!("edit:{}", path.display()),
        }
    }
}

/// `shot [--fullscreen|--region [--geometry WxH+X+Y]|--window]` plus the
/// flags Architecture lists as overriding `capture.toml`.
#[derive(Args, Debug, Clone, Default)]
pub struct ShotArgs {
    /// Capture the whole output. The default when no target flag is given.
    #[arg(long)]
    pub fullscreen: bool,
    /// Capture a selected region — interactively via the overlay, or
    /// exactly via `--geometry` (skips the overlay entirely).
    #[arg(long)]
    pub region: bool,
    /// Capture a single window via niri's own `ScreenshotWindow` action
    /// (CAPTURE-RESEARCH D3). Captures the currently focused window unless
    /// `--window-id` names a different one — niri exposes no pixel position
    /// for tiled windows, so a hover-to-highlight picker isn't
    /// implementable; the region overlay's Window button offers the same
    /// focused-window shortcut interactively.
    #[arg(long)]
    pub window: bool,
    /// Skip the overlay and capture exactly this rectangle. Logical
    /// coordinates, matching slurp/grim's convention. Requires `--region`.
    #[arg(long, value_name = "WxH+X+Y", requires = "region")]
    pub geometry: Option<String>,
    /// Capture exactly this window id (from `niri msg windows`, or the
    /// region overlay's own Window button) instead of resolving the
    /// currently focused one. Requires `--window`. The scriptable
    /// `--window` counterpart to `--geometry` — CAPTURE-RESEARCH D3.
    #[arg(long, value_name = "ID", requires = "window")]
    pub window_id: Option<u64>,

    /// `webp` or `png`. Defaults to `capture.toml`'s `image-format`.
    #[arg(long, value_name = "webp|png")]
    pub format: Option<String>,
    /// Directory to save into. Defaults to `capture.toml`'s `save-dir`, or
    /// `~/Pictures/Captures` if that's unset too (storage.rs, Stage 5).
    #[arg(long)]
    pub output: Option<PathBuf>,
    /// Countdown before the shutter, in whole seconds. Defaults to
    /// `capture.toml`'s `delay`.
    #[arg(long)]
    pub delay: Option<u32>,

    /// Composite the cursor into the capture.
    #[arg(long, conflicts_with = "no_cursor")]
    pub cursor: bool,
    /// Omit the cursor from the capture.
    #[arg(long, conflicts_with = "cursor")]
    pub no_cursor: bool,

    /// Copy the result to the clipboard.
    #[arg(long, conflicts_with = "no_copy")]
    pub copy: bool,
    /// Don't touch the clipboard.
    #[arg(long, conflicts_with = "copy")]
    pub no_copy: bool,

    /// Suppress the toast notification for this capture.
    #[arg(long)]
    pub no_toast: bool,
    /// Capture fully in-process and headless (no flash, no toast, no
    /// daemon round-trip) — the scriptable path.
    #[arg(long)]
    pub no_daemon: bool,
}

/// `record start|stop|toggle [--preset hevc|av1|h264] [--audio mic|system|both]`.
#[derive(Args, Debug, Clone)]
pub struct RecordArgs {
    #[command(subcommand)]
    pub action: RecordAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum RecordAction {
    /// Begin recording.
    Start(RecordStartArgs),
    /// Stop the active recording and save it.
    Stop,
    /// Start if idle, stop if recording — what a single keybind toggles.
    Toggle(RecordStartArgs),
}

#[derive(Args, Debug, Clone, Default)]
pub struct RecordStartArgs {
    /// Encoder preset. `av1` is software-encoded (SVT-AV1) and is not
    /// realtime at high resolutions/frame rates — CAPTURE-RESEARCH §3.4
    /// measured 0.58-0.84x at 2560x1600@60; Stage 11 caps it around 30fps.
    /// Defaults to `capture.toml`'s `video-preset`.
    #[arg(long, value_name = "hevc|av1|h264")]
    pub preset: Option<String>,
    /// Record audio from the microphone, system output, or both. Device
    /// *names* are resolved at record time from `pactl list short sources`
    /// (CAPTURE-RESEARCH: never hardcoded) — omit for a silent recording.
    #[arg(long, value_name = "mic|system|both")]
    pub audio: Option<String>,
    /// Negotiate a screencast, log the format and frame cadence for five
    /// seconds, then tear everything down. **Writes nothing**, saves
    /// nothing, and never touches the daemon — the diagnostic for "does
    /// screen recording work on this machine at all?" (PLAN.md Stage 10).
    #[arg(long)]
    pub dry_run: bool,
    /// Cast one window (a niri-ipc window id, the same id space
    /// `shot --window --window-id` uses) instead of the focused monitor.
    ///
    /// **Only accepted together with `--dry-run` today.** Real window
    /// recording is Stage 12; this exists because `RecordWindow` has its own
    /// documented failure mode — CAPTURE-RESEARCH §5.3: a bogus id is
    /// accepted at `RecordWindow` time and the session then self-destructs
    /// — and a diagnostic that cannot reach that path cannot diagnose it.
    /// Window casts are also damage-driven and can go seconds between
    /// frames (§2.3), which is worth seeing before Stage 12 relies on it.
    #[arg(long, value_name = "ID")]
    pub window_id: Option<u64>,
}

// -- resolution: flags + config -> the values the rest of the app uses --

/// A flag or config value that didn't parse — the CLI-verb equivalent of
/// `config.rs`'s per-knob warnings, except a bad *flag* is fatal to that
/// one invocation (printed to stderr, nonzero exit) rather than falling
/// back to a default the way a bad *config* knob does: a script that typo'd
/// `--format=jpeg` needs to know its capture didn't happen, not silently
/// get a webp it didn't ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError(pub String);

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CliError {}

/// Which of the three shot targets was asked for. Carries no data of its
/// own beyond the variant — [`CaptureOptions::geometry`] is the one piece
/// of per-kind data, and it only ever applies to [`Region`](Self::Region).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShotKind {
    Fullscreen,
    Region,
    Window,
}

impl ShotKind {
    /// The wire spelling `Screenshot`'s `kind` argument uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fullscreen => "fullscreen",
            Self::Region => "region",
            Self::Window => "window",
        }
    }
}

/// A `--geometry WxH+X+Y` value, parsed. Logical coordinates (matching
/// slurp/grim — CAPTURE-RESEARCH D2), converted to physical pixels by
/// whichever backend consumes it (Stage 5/7), not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
}

impl Geometry {
    /// Parses `WxH+X+Y` — width and height are unsigned, `X`/`Y` may be
    /// negative (a monitor to the left of or above the origin). The format
    /// always writes the two `+` separators literally, even when the
    /// coordinate that follows is negative (`800x600+-50+30`), so splitting
    /// on the first two `+` characters is always correct — the `-` sign
    /// belongs to the number, never to the separator.
    pub fn parse(raw: &str) -> Result<Self, CliError> {
        let bad = || CliError(format!("--geometry: expected WxH+X+Y, got {raw:?}"));

        let mut parts = raw.splitn(3, '+');
        let wh = parts.next().ok_or_else(bad)?;
        let x = parts.next().ok_or_else(bad)?;
        let y = parts.next().ok_or_else(bad)?;

        let (width, height) = wh.split_once('x').ok_or_else(bad)?;
        let width: u32 = width.parse().map_err(|_| bad())?;
        let height: u32 = height.parse().map_err(|_| bad())?;
        if width == 0 || height == 0 {
            return Err(CliError(format!(
                "--geometry: width and height must be positive, got {raw:?}"
            )));
        }
        let x: i32 = x.parse().map_err(|_| bad())?;
        let y: i32 = y.parse().map_err(|_| bad())?;

        Ok(Geometry {
            width,
            height,
            x,
            y,
        })
    }
}

/// Which target flags were actually given, folded into the one kind the
/// rest of the pipeline cares about. Pulled out of [`CaptureOptions::resolve`]
/// so the mutual-exclusivity logic is unit-testable on its own.
fn resolve_shot_kind(
    args: &ShotArgs,
) -> Result<(ShotKind, Option<Geometry>, Option<u64>), CliError> {
    let chosen = [args.fullscreen, args.region, args.window]
        .iter()
        .filter(|&&set| set)
        .count();
    if chosen > 1 {
        return Err(CliError(
            "choose at most one of --fullscreen, --region, --window".to_string(),
        ));
    }

    // `requires = "region"`/`requires = "window"` on the clap side already
    // reject `--geometry`/`--window-id` without their target flag when
    // parsed from real argv — these checks are what make the rule enforced
    // (and testable) for a `ShotArgs` built directly in a test, which
    // bypasses clap entirely.
    if args.geometry.is_some() && !args.region {
        return Err(CliError("--geometry requires --region".to_string()));
    }
    if args.window_id.is_some() && !args.window {
        return Err(CliError("--window-id requires --window".to_string()));
    }

    let geometry = args.geometry.as_deref().map(Geometry::parse).transpose()?;

    let kind = if args.region {
        ShotKind::Region
    } else if args.window {
        ShotKind::Window
    } else {
        // No target flag at all: fullscreen. Chosen over an interactive
        // region pick as the default because it needs no daemon-side
        // picker to produce a result — the safer default for scripts and
        // for `--no-daemon`, and `Print`'s own bind is `shot --fullscreen`
        // explicitly rather than relying on this default anyway.
        ShotKind::Fullscreen
    };

    Ok((kind, geometry, args.window_id))
}

/// A resolved `--cursor`/`--no-cursor`-shaped pair against a config
/// default. clap's `conflicts_with` already prevents both flags being true
/// out of real argv; a `ShotArgs` built directly in a test could still set
/// both, which is treated as "no override" rather than a panic — the flag
/// resolution equivalent of `config.rs`'s "a bad value degrades, it never
/// crashes" rule.
fn resolve_bool_override(set_true: bool, set_false: bool, default: bool) -> bool {
    match (set_true, set_false) {
        (true, false) => true,
        (false, true) => false,
        _ => default,
    }
}

/// The fully resolved options for one `shot` invocation: every
/// `capture.toml` knob that applies to a screenshot, with any flag
/// `ShotArgs` carried overriding it. This is what `main.rs`'s dispatch
/// hands to the D-Bus call (via [`Self::to_dbus_options`]) or to the
/// in-process path (`--no-daemon`, Stage 5).
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureOptions {
    pub kind: ShotKind,
    pub geometry: Option<Geometry>,
    /// `--window-id`, only meaningful when `kind == ShotKind::Window`. `None`
    /// means "resolve the focused window at capture time" (CAPTURE-RESEARCH
    /// D3's no-picker default) — the [`Geometry`]-shaped counterpart to
    /// `geometry` above, same "skip the interactive step" role for `--window`
    /// that `--geometry` plays for `--region`.
    pub window_id: Option<u64>,
    pub format: ImageFormat,
    /// libwebp's lossy quality, `1..=100`. Config-only (no CLI flag) —
    /// see `config::CaptureConfig::webp_quality`.
    pub webp_quality: u8,
    pub png_also: bool,
    /// `None` defers to `storage.rs`'s `~/Pictures/Captures` fallback
    /// (Stage 5) — neither `capture.toml`'s `save-dir` nor `--output` was
    /// given. Kept as an `Option` all the way through rather than resolved
    /// here, so this module never has to know that fallback path (PLAN.md
    /// assigns it to Stage 5's `storage.rs` alone).
    pub output_dir: Option<PathBuf>,
    pub delay: u32,
    pub cursor: bool,
    pub copy: bool,
    pub toast: bool,
    pub no_daemon: bool,
}

impl CaptureOptions {
    /// Folds `args` over `config`: an explicitly-given flag wins, an absent
    /// one falls through to the config value (which is already a concrete
    /// default by the time it reaches here — see `config.rs`).
    pub fn resolve(config: &CaptureConfig, args: &ShotArgs) -> Result<Self, CliError> {
        let (kind, geometry, window_id) = resolve_shot_kind(args)?;

        let format = match &args.format {
            Some(raw) => ImageFormat::parse(raw).ok_or_else(|| {
                CliError(format!(
                    "--format: unrecognized image format {raw:?} (expected webp or png)"
                ))
            })?,
            None => config.image_format,
        };

        let output_dir = args.output.clone().or_else(|| config.save_dir.clone());
        let delay = args.delay.unwrap_or(config.delay);
        let cursor = resolve_bool_override(args.cursor, args.no_cursor, config.cursor);
        let copy = resolve_bool_override(args.copy, args.no_copy, config.copy);
        let toast = if args.no_toast { false } else { config.toasts };

        Ok(CaptureOptions {
            kind,
            geometry,
            window_id,
            format,
            webp_quality: config.webp_quality,
            png_also: config.png_also,
            output_dir,
            delay,
            cursor,
            copy,
            toast,
            no_daemon: args.no_daemon,
        })
    }

    /// The resolved options as a D-Bus `a{sv}` map — Architecture: CLI
    /// flags "travel as the D-Bus `a{sv}` options map". Every entry is
    /// optional-on-failure rather than `.expect()`-ed: `OwnedValue`'s
    /// primitive `From` impls are infallible for the types used here, but
    /// building the map defensively (skip, don't crash, on the
    /// unreachable failure case) matches CLAUDE.md's no-panic rule rather
    /// than leaning on that guarantee.
    pub fn to_dbus_options(&self) -> HashMap<String, OwnedValue> {
        let mut options = HashMap::new();
        options.insert(
            "format".to_string(),
            OwnedValue::from(fixed_str(self.format.as_str())),
        );
        options.insert(
            "webp-quality".to_string(),
            OwnedValue::from(u32::from(self.webp_quality)),
        );
        options.insert("png-also".to_string(), OwnedValue::from(self.png_also));
        if let Some(dir) = &self.output_dir {
            options.insert(
                "output".to_string(),
                OwnedValue::from(fixed_str(dir.to_string_lossy().into_owned())),
            );
        }
        options.insert("delay".to_string(), OwnedValue::from(self.delay));
        options.insert("cursor".to_string(), OwnedValue::from(self.cursor));
        options.insert("copy".to_string(), OwnedValue::from(self.copy));
        options.insert("toast".to_string(), OwnedValue::from(self.toast));
        if let Some(geometry) = self.geometry {
            options.insert(
                "geometry".to_string(),
                OwnedValue::from(fixed_str(format!(
                    "{}x{}+{}+{}",
                    geometry.width, geometry.height, geometry.x, geometry.y
                ))),
            );
        }
        if let Some(window_id) = self.window_id {
            options.insert("window-id".to_string(), OwnedValue::from(window_id));
        }
        options
    }
}

/// The **decode** half of [`CaptureOptions::to_dbus_options`] — what the
/// daemon does with the `a{sv}` map a CLI verb sent it (Stage 5; the Stage 3
/// stub only counted the entries).
///
/// # Why this is deliberately forgiving (teaching note)
///
/// Every value here has already been resolved once, on the *caller's* side,
/// against that caller's own `capture.toml` — the map is a record of a
/// decision already made, not user input being validated for the first time.
/// So a missing or wrong-typed key falls back to the same hardcoded default
/// [`crate::config::CaptureConfig::default`] uses, rather than failing the
/// call: a newer CLI talking to an older daemon (or a `busctl` invocation
/// that sent five of the eight keys by hand) should still take a screenshot.
///
/// The one exception is `kind`, which is not an option at all but the
/// method's own first argument, and has no sensible default — an
/// unrecognized kind is a hard error.
impl CaptureOptions {
    pub fn from_dbus_options(
        kind: &str,
        options: &HashMap<String, OwnedValue>,
    ) -> Result<Self, CliError> {
        let defaults = CaptureConfig::default();

        let kind = match kind {
            "fullscreen" => ShotKind::Fullscreen,
            "region" => ShotKind::Region,
            "window" => ShotKind::Window,
            other => {
                return Err(CliError(format!(
                    "unrecognized screenshot kind {other:?} (expected fullscreen, region, \
                     or window)"
                )))
            }
        };

        let format = option_str(options, "format")
            .and_then(|raw| ImageFormat::parse(&raw))
            .unwrap_or(defaults.image_format);

        // A `geometry` that doesn't parse is dropped rather than fatal, for
        // the same "already validated upstream" reason — and dropping it
        // means a `--region` falls through to the same clean "needs the
        // overlay" error an omitted geometry would give, not a wrong crop.
        let geometry = option_str(options, "geometry").and_then(|raw| Geometry::parse(&raw).ok());
        let window_id = option_u64(options, "window-id");

        Ok(CaptureOptions {
            kind,
            geometry,
            window_id,
            format,
            webp_quality: option_u32(options, "webp-quality")
                .and_then(|value| u8::try_from(value).ok())
                .filter(|quality| (1..=100).contains(quality))
                .unwrap_or(defaults.webp_quality),
            png_also: option_bool(options, "png-also").unwrap_or(defaults.png_also),
            output_dir: option_str(options, "output").map(PathBuf::from),
            delay: option_u32(options, "delay").unwrap_or(defaults.delay),
            cursor: option_bool(options, "cursor").unwrap_or(defaults.cursor),
            copy: option_bool(options, "copy").unwrap_or(defaults.copy),
            toast: option_bool(options, "toast").unwrap_or(defaults.toasts),
            // `--no-daemon` never travels over the bus: by definition a call
            // that reached the daemon did not take the no-daemon path.
            no_daemon: false,
        })
    }
}

/// `zvariant` reading helpers. Each clones the `OwnedValue` because
/// `TryFrom<OwnedValue>` consumes it, and each swallows a type mismatch into
/// `None` so the caller's `unwrap_or(default)` is the single place the
/// fallback is spelled out.
fn option_str(options: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    String::try_from(options.get(key)?.clone()).ok()
}

fn option_bool(options: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    bool::try_from(options.get(key)?.clone()).ok()
}

fn option_u32(options: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    u32::try_from(options.get(key)?.clone()).ok()
}

fn option_u64(options: &HashMap<String, OwnedValue>, key: &str) -> Option<u64> {
    u64::try_from(options.get(key)?.clone()).ok()
}

/// `zvariant::Str<'static>` from an owned `String`, the shape
/// `OwnedValue`'s `Str` conversion wants (see `zvariant::owned_value`'s
/// `to_value!` macro — it takes `Str<'a>`, not a bare `String`/`&str`).
fn fixed_str(value: impl Into<String>) -> zbus::zvariant::Str<'static> {
    zbus::zvariant::Str::from(value.into())
}

/// `mic`/`system`/`both` — never a hardcoded device name (CAPTURE-RESEARCH:
/// resolved from `pactl list short sources` at record time, Stage 13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSource {
    Mic,
    System,
    Both,
}

impl AudioSource {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "mic" => Some(Self::Mic),
            "system" => Some(Self::System),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mic => "mic",
            Self::System => "system",
            Self::Both => "both",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordActionKind {
    Start,
    Stop,
    Toggle,
}

/// The fully resolved options for one `record` invocation. `audio` has no
/// config-file counterpart (`capture.toml` carries no audio knob) — it is
/// `None` unless `--audio` was given, meaning "record video only."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordOptions {
    pub action: RecordActionKind,
    pub preset: VideoPreset,
    pub audio: Option<AudioSource>,
    /// `--dry-run` (Stage 10). **Deliberately absent from
    /// [`Self::to_dbus_options`]**: a dry run never reaches the daemon, so
    /// putting it on the wire would add a key to the frozen
    /// `io.saola.Capture1` contract that nothing would ever read. Only
    /// meaningful with `start`/`toggle`; `record stop` has no args at all,
    /// so clap rejects `--dry-run` there before this type sees it.
    pub dry_run: bool,
    /// `--window-id` (Stage 10, dry-run only — see [`RecordStartArgs::
    /// window_id`]). Also absent from [`Self::to_dbus_options`], for the
    /// same reason `dry_run` is: nothing on the daemon side reads it yet,
    /// and Stage 12 will decide the real wire shape for a window recording
    /// target when it builds one.
    pub window_id: Option<u64>,
    /// `cursor` from `capture.toml` — whether the pointer is composited into
    /// the cast (`screencast::CursorMode`). **Added in Stage 11.** No CLI
    /// flag of its own yet: `shot`'s `--cursor`/`--no-cursor` live on
    /// `ShotArgs`, and adding the pair to `record` is a Stage 12 UX decision,
    /// not something a recording pipeline needs to invent.
    pub cursor: bool,
    /// `save-dir` from `capture.toml`. Recordings land beside screenshots —
    /// see `storage::allocate_recording_path`. **Added in Stage 11.**
    pub output_dir: Option<PathBuf>,
    /// `vaapi-device` from `capture.toml`. **Added in Stage 11.**
    ///
    /// Travels over the bus for the same reason every other knob does: the
    /// **CLI process** is the one that reads `capture.toml` (including any
    /// `--config-dir` override), and the daemon deliberately loads no config
    /// of its own — an option map is "a record of a decision already made"
    /// (see [`CaptureOptions::from_dbus_options`]'s teaching note). Keeping
    /// that true for recording means `--config-dir` works for `record` exactly
    /// as it does for `shot`, with no second config-loading path to drift.
    pub vaapi_device: Option<PathBuf>,
}

impl RecordOptions {
    pub fn resolve(config: &CaptureConfig, args: &RecordArgs) -> Result<Self, CliError> {
        let (action, start_args) = match &args.action {
            RecordAction::Start(a) => (RecordActionKind::Start, Some(a)),
            RecordAction::Stop => (RecordActionKind::Stop, None),
            RecordAction::Toggle(a) => (RecordActionKind::Toggle, Some(a)),
        };

        let preset = match start_args.and_then(|a| a.preset.as_deref()) {
            Some(raw) => VideoPreset::parse(raw).ok_or_else(|| {
                CliError(format!(
                    "--preset: unrecognized preset {raw:?} (expected hevc, av1, or h264)"
                ))
            })?,
            None => config.video_preset,
        };

        let audio = match start_args.and_then(|a| a.audio.as_deref()) {
            Some(raw) => Some(AudioSource::parse(raw).ok_or_else(|| {
                CliError(format!(
                    "--audio: unrecognized source {raw:?} (expected mic, system, or both)"
                ))
            })?),
            None => None,
        };

        let dry_run = start_args.is_some_and(|a| a.dry_run);
        let window_id = start_args.and_then(|a| a.window_id);
        // Rejected rather than ignored: a flag that silently does nothing is
        // the shape of bug this repo's per-knob-error convention exists to
        // prevent, and a script that asked to record a specific window needs
        // to learn it recorded the whole monitor instead.
        if window_id.is_some() && !dry_run {
            return Err(CliError(
                "--window-id: window recording lands in Stage 12 — today this flag is only \
                 accepted with --dry-run"
                    .to_string(),
            ));
        }

        Ok(RecordOptions {
            action,
            preset,
            audio,
            dry_run,
            window_id,
            cursor: config.cursor,
            output_dir: config.save_dir.clone(),
            vaapi_device: config.vaapi_device.clone(),
        })
    }

    /// The **decode** half of [`Self::to_dbus_options`] — the daemon's
    /// `StartRecording` (Stage 11). Same deliberately forgiving posture as
    /// [`CaptureOptions::from_dbus_options`]: every value was already resolved
    /// against the caller's own `capture.toml`, so a missing or wrong-typed
    /// key falls back to the same default rather than failing the call.
    ///
    /// `kind` is the method's own first argument. Only `"fullscreen"` is
    /// accepted today — `region`/`window` recording is Stage 12
    /// (CAPTURE-RESEARCH D8), and an unrecognized kind is a hard error rather
    /// than a silent fullscreen recording nobody asked for.
    pub fn from_dbus_options(
        kind: &str,
        options: &HashMap<String, OwnedValue>,
    ) -> Result<Self, CliError> {
        let defaults = CaptureConfig::default();

        if kind != "fullscreen" {
            return Err(CliError(format!(
                "recording kind {kind:?} is not supported yet — only \"fullscreen\" works today \
                 (region and window recording land in Stage 12)"
            )));
        }

        let preset = option_str(options, "preset")
            .and_then(|raw| VideoPreset::parse(&raw))
            .unwrap_or(defaults.video_preset);
        let audio = option_str(options, "audio").and_then(|raw| AudioSource::parse(&raw));

        Ok(RecordOptions {
            action: RecordActionKind::Start,
            preset,
            audio,
            // Neither travels over the bus, by construction — a dry run never
            // reaches the daemon and window targets are Stage 12.
            dry_run: false,
            window_id: None,
            cursor: option_bool(options, "cursor").unwrap_or(defaults.cursor),
            output_dir: option_str(options, "output").map(PathBuf::from),
            vaapi_device: option_str(options, "vaapi-device").map(PathBuf::from),
        })
    }

    /// The `options` map for `StartRecording`'s `a{sv}` argument — same
    /// defensive-insert shape as [`CaptureOptions::to_dbus_options`].
    pub fn to_dbus_options(&self) -> HashMap<String, OwnedValue> {
        let mut options = HashMap::new();
        options.insert(
            "preset".to_string(),
            OwnedValue::from(fixed_str(self.preset.as_str())),
        );
        if let Some(audio) = self.audio {
            options.insert(
                "audio".to_string(),
                OwnedValue::from(fixed_str(audio.as_str())),
            );
        }
        options.insert("cursor".to_string(), OwnedValue::from(self.cursor));
        if let Some(dir) = &self.output_dir {
            options.insert(
                "output".to_string(),
                OwnedValue::from(fixed_str(dir.to_string_lossy().into_owned())),
            );
        }
        if let Some(device) = &self.vaapi_device {
            options.insert(
                "vaapi-device".to_string(),
                OwnedValue::from(fixed_str(device.to_string_lossy().into_owned())),
            );
        }
        options
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(mutate: impl FnOnce(&mut ShotArgs)) -> ShotArgs {
        let mut args = ShotArgs::default();
        mutate(&mut args);
        args
    }

    // -- ShotKind / geometry resolution --------------------------------

    #[test]
    fn no_target_flag_defaults_to_fullscreen() {
        let (kind, geometry, window_id) = resolve_shot_kind(&ShotArgs::default()).unwrap();
        assert_eq!(kind, ShotKind::Fullscreen);
        assert_eq!(geometry, None);
        assert_eq!(window_id, None);
    }

    #[test]
    fn explicit_fullscreen() {
        let args = shot(|a| a.fullscreen = true);
        let (kind, ..) = resolve_shot_kind(&args).unwrap();
        assert_eq!(kind, ShotKind::Fullscreen);
    }

    #[test]
    fn region_without_geometry_is_interactive() {
        let args = shot(|a| a.region = true);
        let (kind, geometry, _) = resolve_shot_kind(&args).unwrap();
        assert_eq!(kind, ShotKind::Region);
        assert_eq!(geometry, None);
    }

    #[test]
    fn region_with_geometry_skips_the_overlay() {
        let args = shot(|a| {
            a.region = true;
            a.geometry = Some("600x450+100+100".to_string());
        });
        let (kind, geometry, _) = resolve_shot_kind(&args).unwrap();
        assert_eq!(kind, ShotKind::Region);
        assert_eq!(
            geometry,
            Some(Geometry {
                width: 600,
                height: 450,
                x: 100,
                y: 100
            })
        );
    }

    #[test]
    fn window_kind() {
        let args = shot(|a| a.window = true);
        let (kind, _, window_id) = resolve_shot_kind(&args).unwrap();
        assert_eq!(kind, ShotKind::Window);
        assert_eq!(window_id, None, "no --window-id given");
    }

    #[test]
    fn window_with_explicit_id_skips_the_picker() {
        let args = shot(|a| {
            a.window = true;
            a.window_id = Some(42);
        });
        let (kind, _, window_id) = resolve_shot_kind(&args).unwrap();
        assert_eq!(kind, ShotKind::Window);
        assert_eq!(window_id, Some(42));
    }

    #[test]
    fn fullscreen_and_region_together_is_an_error() {
        let args = shot(|a| {
            a.fullscreen = true;
            a.region = true;
        });
        assert!(resolve_shot_kind(&args).is_err());
    }

    #[test]
    fn geometry_without_region_is_an_error() {
        // Simulates a `ShotArgs` built directly (bypassing clap's own
        // `requires = "region"` enforcement) — the pure resolver must
        // still catch it.
        let args = shot(|a| a.geometry = Some("100x100+0+0".to_string()));
        assert!(resolve_shot_kind(&args).is_err());
    }

    #[test]
    fn window_id_without_window_is_an_error() {
        // Same shape as `geometry_without_region_is_an_error`, bypassing
        // clap's own `requires = "window"` enforcement.
        let args = shot(|a| a.window_id = Some(7));
        assert!(resolve_shot_kind(&args).is_err());
    }

    #[test]
    fn geometry_parses_negative_offsets() {
        let g = Geometry::parse("800x600+-50+-30").unwrap();
        assert_eq!(
            g,
            Geometry {
                width: 800,
                height: 600,
                x: -50,
                y: -30
            }
        );
    }

    #[test]
    fn geometry_rejects_zero_dimensions() {
        assert!(Geometry::parse("0x600+0+0").is_err());
        assert!(Geometry::parse("600x0+0+0").is_err());
    }

    #[test]
    fn geometry_rejects_garbage() {
        assert!(Geometry::parse("not-a-geometry").is_err());
        assert!(Geometry::parse("600x450").is_err());
    }

    // -- CaptureOptions::resolve precedence ------------------------------

    #[test]
    fn capture_options_default_to_config_when_no_flags_given() {
        let config = CaptureConfig::default();
        let options = CaptureOptions::resolve(&config, &ShotArgs::default()).unwrap();
        assert_eq!(options.format, config.image_format);
        assert_eq!(options.delay, config.delay);
        assert_eq!(options.cursor, config.cursor);
        assert_eq!(options.copy, config.copy);
        assert_eq!(options.toast, config.toasts);
        assert_eq!(options.output_dir, None);
        assert!(!options.no_daemon);
    }

    #[test]
    fn flags_override_config() {
        // Every field below already matches `CaptureConfig::default()` —
        // spelled out anyway so the "flags win" assertions read against
        // concrete values rather than an opaque `default()` call.
        let config = CaptureConfig::default();
        assert_eq!(config.image_format, ImageFormat::Webp);
        assert_eq!(config.delay, 0);
        assert!(config.cursor);
        assert!(config.copy);
        assert!(config.toasts);

        let args = shot(|a| {
            a.format = Some("png".to_string());
            a.delay = Some(5);
            a.no_cursor = true;
            a.no_copy = true;
            a.no_toast = true;
            a.output = Some(PathBuf::from("/tmp/shots"));
        });

        let options = CaptureOptions::resolve(&config, &args).unwrap();
        assert_eq!(options.format, ImageFormat::Png);
        assert_eq!(options.delay, 5);
        assert!(!options.cursor);
        assert!(!options.copy);
        assert!(!options.toast);
        assert_eq!(options.output_dir, Some(PathBuf::from("/tmp/shots")));
    }

    #[test]
    fn cursor_flag_overrides_a_config_default_of_false() {
        let config = CaptureConfig {
            cursor: false,
            ..CaptureConfig::default()
        };
        let args = shot(|a| a.cursor = true);
        let options = CaptureOptions::resolve(&config, &args).unwrap();
        assert!(
            options.cursor,
            "--cursor must win over a false config default"
        );
    }

    #[test]
    fn unrecognized_format_flag_is_an_error() {
        let config = CaptureConfig::default();
        let args = shot(|a| a.format = Some("jpeg".to_string()));
        assert!(CaptureOptions::resolve(&config, &args).is_err());
    }

    #[test]
    fn output_dir_falls_back_to_config_save_dir() {
        let config = CaptureConfig {
            save_dir: Some(PathBuf::from("/home/jordan/Pictures/Screenshots")),
            ..CaptureConfig::default()
        };
        let options = CaptureOptions::resolve(&config, &ShotArgs::default()).unwrap();
        assert_eq!(
            options.output_dir,
            Some(PathBuf::from("/home/jordan/Pictures/Screenshots"))
        );
    }

    // -- RecordOptions::resolve ------------------------------------------

    #[test]
    fn record_start_defaults_to_config_preset_and_no_audio() {
        let config = CaptureConfig::default();
        assert_eq!(config.video_preset, VideoPreset::Hevc);
        let args = RecordArgs {
            action: RecordAction::Start(RecordStartArgs::default()),
        };
        let options = RecordOptions::resolve(&config, &args).unwrap();
        assert_eq!(options.action, RecordActionKind::Start);
        assert_eq!(options.preset, VideoPreset::Hevc);
        assert_eq!(options.audio, None);
    }

    #[test]
    fn record_start_flags_override_config() {
        let config = CaptureConfig::default();
        let args = RecordArgs {
            action: RecordAction::Start(RecordStartArgs {
                preset: Some("av1".to_string()),
                audio: Some("both".to_string()),
                ..RecordStartArgs::default()
            }),
        };
        let options = RecordOptions::resolve(&config, &args).unwrap();
        assert_eq!(options.preset, VideoPreset::Av1);
        assert_eq!(options.audio, Some(AudioSource::Both));
    }

    #[test]
    fn record_stop_carries_no_preset_or_audio_override() {
        let config = CaptureConfig {
            video_preset: VideoPreset::H264,
            ..CaptureConfig::default()
        };
        let args = RecordArgs {
            action: RecordAction::Stop,
        };
        let options = RecordOptions::resolve(&config, &args).unwrap();
        assert_eq!(options.action, RecordActionKind::Stop);
        assert_eq!(options.preset, VideoPreset::H264);
        assert_eq!(options.audio, None);
    }

    #[test]
    fn unrecognized_preset_is_an_error() {
        let config = CaptureConfig::default();
        let args = RecordArgs {
            action: RecordAction::Start(RecordStartArgs {
                preset: Some("prores".to_string()),
                ..RecordStartArgs::default()
            }),
        };
        assert!(RecordOptions::resolve(&config, &args).is_err());
    }

    #[test]
    fn unrecognized_audio_source_is_an_error() {
        let config = CaptureConfig::default();
        let args = RecordArgs {
            action: RecordAction::Start(RecordStartArgs {
                audio: Some("bluetooth".to_string()),
                ..RecordStartArgs::default()
            }),
        };
        assert!(RecordOptions::resolve(&config, &args).is_err());
    }

    // -- --dry-run / --window-id (Stage 10) ---------------------------------

    #[test]
    fn record_start_defaults_to_a_real_run_on_the_focused_monitor() {
        let config = CaptureConfig::default();
        let args = RecordArgs {
            action: RecordAction::Start(RecordStartArgs::default()),
        };
        let options = RecordOptions::resolve(&config, &args).unwrap();
        assert!(!options.dry_run);
        assert_eq!(options.window_id, None);
    }

    #[test]
    fn dry_run_survives_resolution_on_start_and_toggle() {
        let config = CaptureConfig::default();
        for action in [
            RecordAction::Start(RecordStartArgs {
                dry_run: true,
                ..RecordStartArgs::default()
            }),
            RecordAction::Toggle(RecordStartArgs {
                dry_run: true,
                ..RecordStartArgs::default()
            }),
        ] {
            let options = RecordOptions::resolve(&config, &RecordArgs { action }).unwrap();
            assert!(options.dry_run);
        }
    }

    #[test]
    fn a_window_id_is_accepted_only_alongside_dry_run() {
        let config = CaptureConfig::default();

        let allowed = RecordArgs {
            action: RecordAction::Start(RecordStartArgs {
                dry_run: true,
                window_id: Some(16),
                ..RecordStartArgs::default()
            }),
        };
        assert_eq!(
            RecordOptions::resolve(&config, &allowed).unwrap().window_id,
            Some(16)
        );

        // Rejected, not silently ignored — a script that asked for a window
        // must not quietly get the whole monitor instead.
        let refused = RecordArgs {
            action: RecordAction::Start(RecordStartArgs {
                window_id: Some(16),
                ..RecordStartArgs::default()
            }),
        };
        let err = RecordOptions::resolve(&config, &refused).unwrap_err();
        assert!(err.to_string().contains("--window-id"), "{err}");
        assert!(err.to_string().contains("--dry-run"), "{err}");
    }

    #[test]
    fn neither_dry_run_nor_window_id_travels_over_dbus() {
        // Both are CLI-process-only (Stage 10); adding either to the `a{sv}`
        // map would grow the frozen `io.saola.Capture1` contract with a key
        // nothing reads.
        let config = CaptureConfig::default();
        let args = RecordArgs {
            action: RecordAction::Start(RecordStartArgs {
                dry_run: true,
                window_id: Some(16),
                ..RecordStartArgs::default()
            }),
        };
        let map = RecordOptions::resolve(&config, &args)
            .unwrap()
            .to_dbus_options();
        assert!(!map.contains_key("dry-run"));
        assert!(!map.contains_key("dry_run"));
        assert!(!map.contains_key("window-id"));
        assert!(!map.contains_key("window_id"));
    }

    // -- a{sv} option maps --------------------------------------------------

    #[test]
    fn capture_options_to_dbus_options_carries_the_resolved_values() {
        let config = CaptureConfig::default();
        let args = shot(|a| {
            a.region = true;
            a.geometry = Some("600x450+100+100".to_string());
        });
        let options = CaptureOptions::resolve(&config, &args).unwrap();
        let map = options.to_dbus_options();

        assert_eq!(String::try_from(map["format"].clone()).unwrap(), "webp");
        assert_eq!(
            bool::try_from(map["cursor"].clone()).unwrap(),
            options.cursor
        );
        assert_eq!(bool::try_from(map["copy"].clone()).unwrap(), options.copy);
        assert_eq!(
            String::try_from(map["geometry"].clone()).unwrap(),
            "600x450+100+100"
        );
        assert!(!map.contains_key("output"), "no --output was given");
    }

    #[test]
    fn record_options_to_dbus_options_omits_audio_when_not_given() {
        let config = CaptureConfig::default();
        let args = RecordArgs {
            action: RecordAction::Start(RecordStartArgs::default()),
        };
        let options = RecordOptions::resolve(&config, &args).unwrap();
        let map = options.to_dbus_options();

        assert_eq!(String::try_from(map["preset"].clone()).unwrap(), "hevc");
        assert!(!map.contains_key("audio"));
    }

    #[test]
    fn record_options_to_dbus_options_carries_audio_when_given() {
        let config = CaptureConfig::default();
        let args = RecordArgs {
            action: RecordAction::Start(RecordStartArgs {
                audio: Some("system".to_string()),
                ..RecordStartArgs::default()
            }),
        };
        let options = RecordOptions::resolve(&config, &args).unwrap();
        let map = options.to_dbus_options();
        assert_eq!(String::try_from(map["audio"].clone()).unwrap(), "system");
    }

    // -- WindowAction::dbus_mode ------------------------------------------

    #[test]
    fn window_mode_defaults_to_main() {
        assert_eq!(WindowAction::dbus_mode(None), "main");
    }

    #[test]
    fn window_mode_edit_carries_the_path() {
        let action = WindowAction::Edit {
            path: PathBuf::from("/home/jordan/Pictures/Captures/shot.webp"),
        };
        assert_eq!(
            WindowAction::dbus_mode(Some(&action)),
            "edit:/home/jordan/Pictures/Captures/shot.webp"
        );
    }

    // -- clap wiring sanity ------------------------------------------------

    /// Not a resolution test — a guard that the derive macro actually
    /// builds a valid parser (a common way to break `clap::Args`/
    /// `clap::Subcommand` derives is a conflicting attribute that only
    /// fails at `Command::debug_assert`'s internal validation, not at
    /// plain `cargo build`).
    #[test]
    fn the_clap_command_graph_is_internally_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_a_representative_invocation() {
        let cli = Cli::parse_from([
            "saola-capture",
            "shot",
            "--region",
            "--geometry",
            "600x450+100+100",
            "--no-cursor",
            "--format",
            "png",
        ]);
        let Command::Shot(args) = cli.command else {
            panic!("expected the shot subcommand");
        };
        assert!(args.region);
        assert_eq!(args.geometry.as_deref(), Some("600x450+100+100"));
        assert!(args.no_cursor);
        assert_eq!(args.format.as_deref(), Some("png"));
    }
}
