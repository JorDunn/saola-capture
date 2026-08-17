//! `~/.config/saola/capture.toml` — the user-facing capture defaults every
//! CLI verb and the daemon read at boot (PLAN.md Stage 4).
//!
//! # Why `toml`, and why by hand (teaching note)
//!
//! **2026-08-08, Stage 4**: this module was KDL through Stage 3; the
//! config-format decision (made with Jordan, see CLAUDE.md's amendment)
//! moved it to TOML before anything else consumed the KDL shape. See the
//! `toml` vs `toml_edit` vs `basic-toml` survey in `Cargo.toml`'s dependency
//! essay for the crate pick; the short version is that [`toml::Table`] is
//! walked explicitly here — `table.get("save-dir")`, `.as_str()`, … —
//! instead of deriving `serde::Deserialize` on [`CaptureConfig`] itself, for
//! the same two reasons the KDL version gave: a newer-to-Rust reader can
//! trace an explicit walk line by line (CLAUDE.md's teaching-note rule), and
//! a hand-written extractor can name exactly *which* knob was bad
//! ("unrecognized image-format \"jpeg\" — using default") in a way a
//! one-shot "deserialize failed" error cannot. **`toml`'s default features
//! do pull in `serde`** — the crate uses it internally to give `Table`/
//! `Value` their own `Deserialize` impls — but that is `toml` deserializing
//! into its own generic value tree, not this module deriving anything on
//! `CaptureConfig`; the hand-walked, per-knob-warning posture is unchanged
//! from the KDL version.
//!
//! # No wrapper table (a deliberate schema choice)
//!
//! Stage 3's `capture.kdl` wrapped every knob in a top-level `capture { }`
//! node, mirroring the panel's `panel { }`. TOML has no reason to copy that:
//! `capture.toml` is already this app's own file (nothing else will ever
//! read it), so every knob is a **bare top-level key**
//! (`image-format = "webp"`, not `[capture]\nimage-format = "webp"`) —
//! one less level of nesting to walk and to hand-write. Bare keys with a
//! dash parse fine unquoted in TOML (`image-format`, `save-dir`, … are all
//! valid bare keys — TOML's "bare key" grammar allows `-` alongside
//! alphanumerics and `_`), so the kebab-case knob names carry over from the
//! KDL schema unchanged.
//!
//! # Resilience rules (binding — CLAUDE.md's Config bullet)
//!
//! - **No file at all** → [`CaptureConfig::default`], silently. The
//!   expected case for anyone who hasn't written a `capture.toml` yet.
//! - **File present but not valid TOML** ("garbage") → one `eprintln!`
//!   naming the file and the parse error, then the whole config falls back
//!   to [`CaptureConfig::default`] — not a partial merge. A document that
//!   doesn't even parse gives this module nothing safe to partially trust.
//! - **File parses, but one knob's *value* is nonsense** (an unrecognized
//!   `image-format`, a `delay` that isn't a non-negative integer, …) →
//!   warn on that one knob, keep parsing the rest of the document, and
//!   default just that knob. A typo in `delay` must not blank out
//!   `save-dir`.
//! - **A `capture.kdl` is found but no `capture.toml`** → a one-line
//!   migration hint naming both paths, then defaults (a warning, not an
//!   error — see [`CaptureConfig::load_from`]).
//!
//! Every path is unit-tested below (`default_config_parses`,
//! `full_config_parses`, `partial_config_parses`,
//! `garbage_file_falls_back_to_defaults`, `missing_file_falls_back_to_defaults`,
//! plus one nonsense-value test per knob).

use std::fmt;
use std::path::{Path, PathBuf};

use toml::Table;

/// `image-format = "webp"|"png"` — the still-image codec `storage.rs`
/// (Stage 5) encodes into. WebP is the default per Architecture
/// ("space-efficient formats first"); PNG exists for lossless-only
/// workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageFormat {
    #[default]
    Webp,
    Png,
}

impl ImageFormat {
    /// `pub(crate)`, not private: `cli.rs`'s `--format` flag parses against
    /// this exact same vocabulary, so the CLI and the config file can never
    /// silently drift apart on what a valid format string is.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "webp" => Some(Self::Webp),
            "png" => Some(Self::Png),
            _ => None,
        }
    }

    /// The wire spelling, reused by [`crate::dbus`] when a `CaptureOptions`
    /// value has to travel as a D-Bus `a{sv}` string and by `--format`'s
    /// own parser in `cli.rs`, so the vocabulary is defined in exactly one
    /// place.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Webp => "webp",
            Self::Png => "png",
        }
    }
}

impl fmt::Display for ImageFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `video-preset = "hevc"|"av1"|"h264"` — Stage 11's `EncoderSink` preset
/// table key. `av1` is software-encoded (CAPTURE-RESEARCH §3.4) and not
/// realtime at high resolutions; `cli.rs`'s `--help` text says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoPreset {
    #[default]
    Hevc,
    Av1,
    H264,
}

impl VideoPreset {
    /// `pub(crate)` for the same reason as [`ImageFormat::parse`]: `cli.rs`'s
    /// `--preset` flag reuses this exact vocabulary.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "hevc" => Some(Self::Hevc),
            "av1" => Some(Self::Av1),
            "h264" => Some(Self::H264),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
            Self::H264 => "h264",
        }
    }
}

impl fmt::Display for VideoPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `audio = "none"|"mic"|"system"|"both"` and `--audio`'s own vocabulary —
/// **Stage 13**. Which *kind* of audio a recording wants, never a device
/// name: the concrete PulseAudio sources are resolved at record time by
/// `crate::audio::plan_audio` (CAPTURE-RESEARCH §4.4 — "never hardcode: they
/// are hardware-path-derived").
///
/// Lives here rather than in `cli.rs` for the same reason [`ImageFormat`] and
/// [`VideoPreset`] do: the config file and the CLI flag parse against exactly
/// this vocabulary, and having one definition is what stops them drifting.
/// `cli` re-exports it, so `cli::AudioSource` still names this type.
///
/// **There is no `None` variant.** "No audio" is `Option<AudioSource>::None`
/// throughout — the spelling `"none"` is a *parse-level* answer
/// ([`parse_audio`]), which keeps every match on this type total over the
/// three things that actually have a device behind them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSource {
    Mic,
    System,
    Both,
}

impl AudioSource {
    /// The wire/config/flag spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mic => "mic",
            Self::System => "system",
            Self::Both => "both",
        }
    }
}

impl fmt::Display for AudioSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `"none"|"mic"|"system"|"both"` → what the recording should do, with
/// `"none"` mapping to `Some(None)` (understood, and it means silence) and an
/// unrecognized word to `None` (not understood).
///
/// The double `Option` is deliberate and is why this is a free function
/// rather than `AudioSource::parse`: the two "no audio" answers are
/// different, and collapsing them would make `audio = "bluetooth"` silently
/// mean "record no audio" instead of warning.
pub(crate) fn parse_audio(value: &str) -> Option<Option<AudioSource>> {
    match value {
        "none" | "off" => Some(None),
        "mic" => Some(Some(AudioSource::Mic)),
        "system" => Some(Some(AudioSource::System)),
        "both" => Some(Some(AudioSource::Both)),
        _ => None,
    }
}

/// The whole of `capture.toml`, resolved to typed values.
///
/// **Unchanged from Stage 3's KDL-backed version** — same fields, same
/// defaults, same public API. Only the file format and this struct's
/// parsing internals moved; every caller in `main.rs`/`cli.rs` is untouched
/// by this stage.
///
/// **Not `Eq`** since Stage 13: `audio-offset` is a float (seconds), and
/// `f64` has no total equality. Nothing in this crate needs `Eq` on a config
/// — every use is an `assert_eq!` in a test, which `PartialEq` serves.
///
/// `save_dir` is deliberately `Option<PathBuf>`, not a plain `PathBuf` with
/// `~/Pictures/Captures` baked in here: Stage 5's `storage.rs` owns that
/// fallback (PLAN.md Stage 5, item 3 — "save-dir resolution (config →
/// `~/Pictures/Captures` fallback ..., created on demand)"), so this module
/// only reports what the *file* said (or didn't). Every other field has no
/// such downstream owner, so it resolves to a concrete default right here.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureConfig {
    pub save_dir: Option<PathBuf>,
    pub image_format: ImageFormat,
    /// `webp-quality = 1..=100` — libwebp's lossy quality knob, used by
    /// `storage.rs` (PLAN.md Stage 5, item 3: "WebP encode … quality knob
    /// from config"). **Added in Stage 5**, the one schema addition since
    /// Stage 4's migration. Only affects WebP: PNG is lossless and has no
    /// quality dial at all, which is why the knob is named for the format
    /// it actually applies to rather than a generic `image-quality` that
    /// would silently do nothing half the time.
    pub webp_quality: u8,
    pub png_also: bool,
    pub video_preset: VideoPreset,
    /// `vaapi-device = "/dev/dri/renderD129"` — an explicit render node for
    /// the hardware encode presets. **Added in Stage 11** (PLAN.md Stage 11
    /// task 3, and CAPTURE-RESEARCH D6's 2026-08-08 amendment).
    ///
    /// `None`, the default, means "discover it": `encode::ffmpeg_cli` probes
    /// every `/dev/dri/renderD*` with a tiny trial encode and picks the best
    /// one. This knob exists for the cases discovery cannot know about — a
    /// machine where the *other* GPU should do the encoding (a dGPU that is
    /// idle while the iGPU drives the panel, say), or one where a node is
    /// present but misbehaving.
    ///
    /// Deliberately **not validated here**: whether a path is a usable render
    /// node is a runtime question about hardware, not a parse question about
    /// a config file, and this module has a firm rule about only reporting
    /// what the file said. A path that doesn't exist warns and falls back to
    /// full discovery at encoder start (`ffmpeg_cli::choose_encoder`), which
    /// is the same warn-and-default posture every other knob gets, applied at
    /// the layer that can actually check.
    pub vaapi_device: Option<PathBuf>,
    /// `audio = "none"|"mic"|"system"|"both"` — the default for a
    /// `record start` with no `--audio` flag. **Added in Stage 13.**
    ///
    /// `None` (the default) is "record video only", and `--audio none` is how
    /// a CLI invocation overrides a config file that turned audio on — see
    /// `cli::RecordOptions::resolve`.
    pub audio: Option<AudioSource>,
    /// `audio-mic-source = "alsa_input.…"` — an explicit PulseAudio source
    /// name for `--audio mic`/`both`. **Added in Stage 13.**
    ///
    /// `None`, the default, means "use the server's default input".
    /// Deliberately **not validated here**, exactly like [`Self::vaapi_device`]:
    /// whether a name is a real capture device is a runtime question about
    /// hardware, and a name that no longer exists warns and falls back to
    /// discovery in `crate::audio::plan_audio` — the same warn-and-default
    /// rule applied at the layer that can actually check.
    pub audio_mic_source: Option<String>,
    /// `audio-system-source = "….monitor"` — the same knob for the system
    /// audio half. `None` means "the default output's `.monitor`".
    pub audio_system_source: Option<String>,
    /// `audio-offset = 0.0` — seconds added to every audio input's timestamps
    /// (`-itsoffset`), the calibration knob CAPTURE-RESEARCH §4.4's mitigation
    /// 3 asks for. **Added in Stage 13.**
    ///
    /// Positive delays the audio (use it when the sound arrives *early*);
    /// negative advances it. The default is
    /// [`crate::audio::DEFAULT_SYNC_OFFSET`], which is where the measurement
    /// behind that number is written down.
    pub audio_offset: f64,
    pub cursor: bool,
    pub delay: u32,
    pub toasts: bool,
    pub copy: bool,
}

/// The default `webp-quality`. 90 is high enough that a screenshot of text
/// stays crisp (libwebp's own `cwebp` default is 75, which visibly softens
/// small type) while still landing a full-screen capture at a fraction of
/// the equivalent PNG.
pub const DEFAULT_WEBP_QUALITY: u8 = 90;

impl Default for CaptureConfig {
    /// WebP, no PNG sidecar, HEVC preset, cursor visible, no delay, toasts
    /// on, clipboard copy on — the Architecture's own stated defaults
    /// ("always-on `--copy` default", "space-efficient formats first").
    /// This is also what an absent `capture.toml` produces, and what a
    /// garbage one falls back to in full.
    fn default() -> Self {
        CaptureConfig {
            save_dir: None,
            image_format: ImageFormat::default(),
            webp_quality: DEFAULT_WEBP_QUALITY,
            png_also: false,
            video_preset: VideoPreset::default(),
            vaapi_device: None,
            audio: None,
            audio_mic_source: None,
            audio_system_source: None,
            audio_offset: crate::audio::DEFAULT_SYNC_OFFSET,
            cursor: true,
            delay: 0,
            toasts: true,
            copy: true,
        }
    }
}

/// A TOML document that failed to parse at all — the one `Err` this module
/// produces. Every other problem (an absent knob, a bad knob value) resolves
/// to a default and is reported with `eprintln!` instead — see the module
/// doc comment's resilience rules.
#[derive(Debug)]
pub struct ConfigError(toml::de::Error);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ConfigError {}

impl CaptureConfig {
    /// Where `capture.toml` lives: the resolved config **directory** joined
    /// with the fixed file name. Resolution order, most-specific first
    /// (identical shape to `saola-panel::config::PanelConfig::resolve_path`,
    /// and to this repo's own CLAUDE.md Config bullet — unchanged by this
    /// stage):
    ///
    /// 1. **`--config-dir <dir>`** — a per-run override.
    /// 2. **`$SAOLA_CONFIG_DIR`** — the Saola desktop's own env var.
    /// 3. **`$XDG_CONFIG_HOME/saola`** — the XDG base-directory spec.
    /// 4. **`~/.config/saola`** — the spec's own fallback for an unset
    ///    `$XDG_CONFIG_HOME`.
    ///
    /// `None` only when nothing in the chain resolves (no flag, no Saola or
    /// XDG var, and no `$HOME`) — treated the same as "no file": defaults.
    pub fn resolve_path(cli_dir: Option<&Path>) -> Option<PathBuf> {
        config_dir_from(
            cli_dir,
            std::env::var_os("SAOLA_CONFIG_DIR"),
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
        .map(|dir| dir.join("capture.toml"))
    }

    /// Load the config at boot from the path [`Self::resolve_path`] gave
    /// the caller (`None` loads pure defaults — a container/CI environment
    /// with no `$HOME` is "no config is possible here", not "broken
    /// config"). Never fails: every error path warns (via `eprintln!`,
    /// matching the panel's convention) and returns a value, never
    /// propagating a `Result` up to `main`.
    pub fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        Self::load_from(path)
    }

    fn load_from(path: &Path) -> Self {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            // Covers both "the file doesn't exist" (the common case) and
            // any other I/O error (permissions, …) — both degrade to
            // defaults silently, same as the panel's loader. A missing
            // `capture.toml` is also the trigger for the one-time
            // `capture.kdl`-migration hint below (item 4, Stage 4's own
            // job) — a sibling `capture.kdl` next to an absent
            // `capture.toml` almost certainly means an un-migrated Stage 3
            // config, so the hint names both paths without treating it as
            // an error (defaults still apply either way).
            Err(_) => {
                warn_if_stale_kdl_sibling(path);
                return Self::default();
            }
        };
        match Self::parse(&contents) {
            Ok(config) => config,
            Err(err) => {
                eprintln!(
                    "saola-capture: {} is not valid TOML ({err}) — using defaults",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Parse a `capture.toml` document's contents into a [`CaptureConfig`].
    ///
    /// Returns `Err` **only** if `contents` isn't valid TOML at all — every
    /// other problem (an absent knob, a bad knob value) resolves to a
    /// default and is reported with `eprintln!`. This is the function the
    /// unit tests below exercise directly.
    pub fn parse(contents: &str) -> Result<Self, ConfigError> {
        // No `[capture]` wrapper table (see the module doc comment): every
        // knob is a bare top-level key, so `body` is just the parsed
        // top-level table itself — no `.get("capture")` indirection like
        // the KDL version needed.
        let body: Table = contents.parse().map_err(ConfigError)?;

        let save_dir = read_str(&body, "save-dir").map(expand_tilde);

        let image_format = read_str(&body, "image-format")
            .and_then(|value| match_or_warn(value, "image-format", ImageFormat::parse))
            .unwrap_or_default();

        let webp_quality = read_webp_quality(&body).unwrap_or(DEFAULT_WEBP_QUALITY);

        let png_also = read_bool(&body, "png-also").unwrap_or(false);

        let video_preset = read_str(&body, "video-preset")
            .and_then(|value| match_or_warn(value, "video-preset", VideoPreset::parse))
            .unwrap_or_default();

        // Same `expand_tilde` treatment `save-dir` gets: nothing else in the
        // process will expand it, and `~/dev/...` is a plausible thing to
        // write even though render nodes live under `/dev`.
        let vaapi_device = read_str(&body, "vaapi-device").map(expand_tilde);

        // **Stage 13.** `audio` is the one knob whose "off" answer is also a
        // legal *value*, so it goes through `parse_audio`'s double `Option`
        // rather than `match_or_warn`'s single one: an unrecognized word must
        // warn and default (to "no audio"), while the word `"none"` must
        // resolve to "no audio" silently, and those two paths would be
        // indistinguishable otherwise.
        let audio = match read_str(&body, "audio") {
            Some(value) => match_or_warn(value, "audio", parse_audio).flatten(),
            None => None,
        };
        // Device names are taken as written — see the fields' own doc
        // comments for why this module deliberately does not check them.
        let audio_mic_source = read_str(&body, "audio-mic-source").map(str::to_string);
        let audio_system_source = read_str(&body, "audio-system-source").map(str::to_string);
        let audio_offset = read_audio_offset(&body).unwrap_or(crate::audio::DEFAULT_SYNC_OFFSET);

        let cursor = read_bool(&body, "cursor").unwrap_or(true);

        let delay = read_delay(&body).unwrap_or(0);

        let toasts = read_bool(&body, "toasts").unwrap_or(true);

        let copy = read_bool(&body, "copy").unwrap_or(true);

        Ok(CaptureConfig {
            save_dir,
            image_format,
            webp_quality,
            png_also,
            video_preset,
            vaapi_device,
            audio,
            audio_mic_source,
            audio_system_source,
            audio_offset,
            cursor,
            delay,
            toasts,
            copy,
        })
    }
}

/// Stage 4's migration hint (PLAN.md item 4): a `capture.toml` that doesn't
/// exist yet is unremarkable on its own (the common "haven't configured
/// anything" case), but if a **sibling `capture.kdl`** sits right next to
/// where `capture.toml` would go, it's very likely a Stage-3-era config that
/// nobody has ported — worth one `eprintln!` naming both paths so the fix is
/// obvious, without turning it into an error (defaults still apply exactly
/// as they would for any other missing file).
fn warn_if_stale_kdl_sibling(toml_path: &Path) {
    let kdl_path = toml_path.with_file_name("capture.kdl");
    if kdl_path.is_file() {
        eprintln!(
            "saola-capture: found {} but no {} — capture.kdl is no longer read (Stage 4 \
             migrated config to TOML); copy its knobs into {} using the same names, or \
             delete it to stop seeing this hint — using defaults for now",
            kdl_path.display(),
            toml_path.display(),
            toml_path.display()
        );
    }
}

/// The testable core of [`CaptureConfig::resolve_path`]'s directory chain:
/// takes every environment variable as a plain argument instead of reading
/// the environment itself, so precedence can be unit-tested without
/// mutating (and thereby racing every other test in this binary against)
/// the process's real environment. Identical logic to the panel's
/// `config_dir_from` — an env var set to the **empty string** is treated
/// as unset and falls through to the next rung (the XDG spec's own rule
/// for `$XDG_CONFIG_HOME`, applied uniformly to `$SAOLA_CONFIG_DIR` too).
fn config_dir_from(
    cli: Option<&Path>,
    saola: Option<std::ffi::OsString>,
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(dir) = cli {
        return Some(dir.to_path_buf());
    }
    if let Some(saola) = saola {
        if !saola.is_empty() {
            return Some(PathBuf::from(saola));
        }
    }
    if let Some(xdg) = xdg {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("saola"));
        }
    }
    home.filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".config/saola"))
}

/// A leading `~/` (or a bare `~`) expands against `$HOME`; anything else
/// passes through unchanged. Same minimal scope as the panel's
/// `expand_tilde` — no `~user/` form, no crate dependency.
pub fn expand_tilde(path: &str) -> PathBuf {
    expand_tilde_with_home(path, std::env::var_os("HOME"))
}

/// The testable core of [`expand_tilde`]: `$HOME` as a plain argument
/// rather than read from the environment, for the same race-free-unit-test
/// reason as [`config_dir_from`].
fn expand_tilde_with_home(path: &str, home: Option<std::ffi::OsString>) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home {
            return PathBuf::from(home).join(rest);
        }
    } else if path == "~" {
        if let Some(home) = home {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(path)
}

/// Applies `parser` to `value`; on `None`, warns (naming the offending knob
/// and the value that didn't match) and returns `None` so the caller falls
/// back to that knob's default — the per-knob half of the resilience rules.
fn match_or_warn<T>(value: &str, knob: &str, parser: fn(&str) -> Option<T>) -> Option<T> {
    let parsed = parser(value);
    if parsed.is_none() {
        eprintln!("saola-capture: capture.toml: unrecognized {knob} \"{value}\" — using default");
    }
    parsed
}

/// `table.get(name)` as a string, if the key exists and its value is a TOML
/// string. A key present but holding a non-string value falls through to
/// `None` — same "absent knob" fallback path as a genuinely missing key.
fn read_str<'a>(table: &'a Table, name: &str) -> Option<&'a str> {
    table.get(name)?.as_str()
}

/// `table.get(name)` as a `bool`. A present-but-non-boolean value (`cursor
/// = "yes"`, say) warns and falls back to that knob's default, same as an
/// unrecognized `image-format`.
fn read_bool(table: &Table, name: &str) -> Option<bool> {
    let value = table.get(name)?;
    match value.as_bool() {
        Some(b) => Some(b),
        None => {
            eprintln!(
                "saola-capture: capture.toml: {name} {value} is not a boolean — using default"
            );
            None
        }
    }
}

/// `delay = <secs>` as a non-negative integer count of seconds. Only TOML
/// integers qualify (a fractional delay isn't meaningful at the countdown
/// granularity Stage 8 builds), and only non-negative ones — a negative
/// delay means nothing. Both warn and default, the same per-knob rule
/// every other bad value gets.
fn read_delay(table: &Table) -> Option<u32> {
    let value = table.get("delay")?;
    match value.as_integer() {
        Some(secs) if secs >= 0 => Some(secs.min(u32::MAX as i64) as u32),
        _ => {
            eprintln!(
                "saola-capture: capture.toml: delay {value} is not a non-negative integer — using default"
            );
            None
        }
    }
}

/// `webp-quality = <1..=100>`. Same per-knob rule as [`read_delay`]: only a
/// TOML integer qualifies, and only one inside libwebp's own accepted range
/// — a `0` would be a legal libwebp value but produces an unusable image, so
/// the floor is 1 and anything outside warns and defaults rather than being
/// silently clamped (a clamp would hide a typo'd `webp-quality = 900`).
fn read_webp_quality(table: &Table) -> Option<u8> {
    let value = table.get("webp-quality")?;
    match value.as_integer() {
        Some(quality) if (1..=100).contains(&quality) => Some(quality as u8),
        _ => {
            eprintln!(
                "saola-capture: capture.toml: webp-quality {value} is not an integer in 1..=100 \
                 — using default ({DEFAULT_WEBP_QUALITY})"
            );
            None
        }
    }
}

/// `audio-offset = <seconds>` — **Stage 13**'s A/V calibration knob.
///
/// Accepts a TOML float *or* integer (`audio-offset = 0` is a reasonable
/// thing to write), and only values inside ±[`MAX_AUDIO_OFFSET`]: a correction
/// larger than that is a typo (a millisecond value written as seconds, say),
/// and applying it would silently produce a recording whose audio is seconds
/// out — far worse than the small residual it was meant to fix. Out of range,
/// non-finite, or the wrong type all warn and default, the same per-knob rule
/// every other value gets.
fn read_audio_offset(table: &Table) -> Option<f64> {
    let value = table.get("audio-offset")?;
    let seconds = value
        .as_float()
        .or_else(|| value.as_integer().map(|whole| whole as f64));
    match seconds {
        Some(seconds) if seconds.is_finite() && seconds.abs() <= MAX_AUDIO_OFFSET => Some(seconds),
        _ => {
            eprintln!(
                "saola-capture: capture.toml: audio-offset {value} is not a number of seconds \
                 within ±{MAX_AUDIO_OFFSET} — using default"
            );
            None
        }
    }
}

/// The largest `audio-offset` this module will accept, in seconds. Generous
/// enough for any real device-latency correction (the measured residual on
/// this machine is single-digit milliseconds), small enough that an obviously
/// wrong number is caught.
const MAX_AUDIO_OFFSET: f64 = 5.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// An absent file (an empty document, which is what `load_from` sees
    /// when the real file is missing and falls back before ever calling
    /// `parse`) yields exactly the hardcoded defaults.
    #[test]
    fn default_config_parses() {
        let config = CaptureConfig::parse("").expect("an empty document is valid TOML");
        assert_eq!(config, CaptureConfig::default());
    }

    /// Every knob PLAN.md lists, set to non-default values, all land
    /// correctly. Bare top-level keys, no `[capture]` wrapper table (Stage
    /// 4's schema decision).
    #[test]
    fn full_config_parses() {
        let toml = r#"
            save-dir = "~/Pictures/Screenshots"
            image-format = "png"
            webp-quality = 72
            png-also = true
            video-preset = "av1"
            vaapi-device = "/dev/dri/renderD129"
            audio = "both"
            audio-mic-source = "alsa_input.usb-Blue_Yeti"
            audio-system-source = "alsa_output.hdmi.monitor"
            audio-offset = 0.08
            cursor = false
            delay = 3
            toasts = false
            copy = false
        "#;
        let config = CaptureConfig::parse(toml).expect("well-formed TOML");

        assert_eq!(
            config.save_dir,
            Some(expand_tilde("~/Pictures/Screenshots"))
        );
        assert_eq!(config.image_format, ImageFormat::Png);
        assert_eq!(config.webp_quality, 72);
        assert!(config.png_also);
        assert_eq!(config.video_preset, VideoPreset::Av1);
        assert_eq!(
            config.vaapi_device,
            Some(PathBuf::from("/dev/dri/renderD129"))
        );
        assert_eq!(config.audio, Some(AudioSource::Both));
        assert_eq!(
            config.audio_mic_source.as_deref(),
            Some("alsa_input.usb-Blue_Yeti")
        );
        assert_eq!(
            config.audio_system_source.as_deref(),
            Some("alsa_output.hdmi.monitor")
        );
        assert_eq!(config.audio_offset, 0.08);
        assert!(!config.cursor);
        assert_eq!(config.delay, 3);
        assert!(!config.toasts);
        assert!(!config.copy);
    }

    // -- Stage 13: the audio knobs -------------------------------------

    /// The default is silence, and it is reached by three different routes:
    /// an absent knob, the explicit word, and a nonsense value.
    #[test]
    fn audio_defaults_to_none_however_it_is_spelled_or_mis_spelled() {
        for toml in ["", r#"audio = "none""#, r#"audio = "bluetooth""#] {
            let config = CaptureConfig::parse(toml).expect("well-formed TOML");
            assert_eq!(config.audio, None, "{toml}");
        }
    }

    #[test]
    fn audio_accepts_each_real_source() {
        for (value, expected) in [
            ("mic", AudioSource::Mic),
            ("system", AudioSource::System),
            ("both", AudioSource::Both),
        ] {
            let config =
                CaptureConfig::parse(&format!("audio = \"{value}\"")).expect("well-formed TOML");
            assert_eq!(config.audio, Some(expected));
            assert_eq!(expected.as_str(), value, "the vocabulary round-trips");
        }
    }

    /// A bad `audio` value must not blank the *other* audio knobs — the
    /// per-knob fallback rule, checked on the knobs most likely to be written
    /// together.
    #[test]
    fn a_nonsense_audio_value_keeps_the_device_overrides() {
        let toml = r#"
            audio = "surround"
            audio-mic-source = "alsa_input.usb"
        "#;
        let config = CaptureConfig::parse(toml).expect("well-formed TOML");
        assert_eq!(config.audio, None);
        assert_eq!(config.audio_mic_source.as_deref(), Some("alsa_input.usb"));
    }

    /// Whether a name is a real device is not this module's question — see
    /// the fields' doc comments. It parses; `audio::plan_audio` warns.
    #[test]
    fn an_audio_source_that_cannot_exist_still_parses() {
        let config =
            CaptureConfig::parse(r#"audio-system-source = "nope.monitor""#).expect("valid TOML");
        assert_eq!(config.audio_system_source.as_deref(), Some("nope.monitor"));
    }

    #[test]
    fn a_non_string_audio_source_falls_back_to_discovery() {
        let config = CaptureConfig::parse("audio-mic-source = 7").expect("valid TOML");
        assert_eq!(config.audio_mic_source, None);
    }

    #[test]
    fn audio_offset_accepts_floats_integers_and_negatives() {
        for (toml, expected) in [
            ("audio-offset = 0.08", 0.08),
            ("audio-offset = 0", 0.0),
            ("audio-offset = -0.25", -0.25),
            ("audio-offset = 5.0", MAX_AUDIO_OFFSET),
        ] {
            let config = CaptureConfig::parse(toml).expect("well-formed TOML");
            assert_eq!(config.audio_offset, expected, "{toml}");
        }
    }

    #[test]
    fn an_out_of_range_or_wrong_typed_audio_offset_falls_back_to_the_default() {
        for toml in [
            "audio-offset = 500",
            "audio-offset = -12.5",
            r#"audio-offset = "0.1s""#,
            "audio-offset = nan",
        ] {
            let config = CaptureConfig::parse(toml).expect("well-formed TOML");
            assert_eq!(
                config.audio_offset,
                crate::audio::DEFAULT_SYNC_OFFSET,
                "{toml} must warn and default"
            );
        }
    }

    /// A config that only overrides a couple of knobs leaves the rest at
    /// their defaults — knob-by-knob fallback, not "any knob present
    /// disables all defaults".
    #[test]
    fn partial_config_parses() {
        let toml = r#"
            image-format = "png"
            delay = 5
        "#;
        let config = CaptureConfig::parse(toml).expect("well-formed TOML");

        assert_eq!(config.image_format, ImageFormat::Png);
        assert_eq!(config.delay, 5);
        assert_eq!(config.save_dir, None);
        assert!(!config.png_also);
        assert_eq!(config.video_preset, VideoPreset::default());
        assert_eq!(config.vaapi_device, None);
        assert_eq!(config.audio, None);
        assert_eq!(config.audio_mic_source, None);
        assert_eq!(config.audio_system_source, None);
        assert_eq!(config.audio_offset, crate::audio::DEFAULT_SYNC_OFFSET);
        assert!(config.cursor);
        assert!(config.toasts);
        assert!(config.copy);
    }

    /// A `vaapi-device` that isn't a string falls through to `None` —
    /// discovery — rather than failing the whole document, the same
    /// per-knob rule every other knob gets. **Stage 11.**
    #[test]
    fn a_non_string_vaapi_device_falls_back_to_discovery() {
        let config = CaptureConfig::parse("vaapi-device = 128").expect("well-formed TOML");
        assert_eq!(config.vaapi_device, None);
    }

    /// Whether the path is a real render node is deliberately *not* this
    /// module's question — see the field's doc comment. A nonsense path
    /// parses fine here and is warned about (and ignored) at encoder start.
    #[test]
    fn a_vaapi_device_that_cannot_exist_still_parses() {
        let config =
            CaptureConfig::parse(r#"vaapi-device = "/dev/dri/renderD999""#).expect("valid TOML");
        assert_eq!(
            config.vaapi_device,
            Some(PathBuf::from("/dev/dri/renderD999"))
        );
    }

    /// Syntactically invalid TOML is the one case `parse` itself rejects.
    #[test]
    fn garbage_is_rejected_by_parse() {
        let result = CaptureConfig::parse("this is not = valid [[[ toml");
        assert!(result.is_err());
    }

    /// `load_from`'s fallback path, exercised end to end against a temp
    /// file: a malformed file degrades to full defaults, with a warning.
    #[test]
    fn garbage_file_falls_back_to_defaults() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "saola-capture-test-garbage-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, "this is not = valid [[[ toml").unwrap();

        let config = CaptureConfig::load_from(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(config, CaptureConfig::default());
    }

    /// A missing file is not an error at all — same defaults, no crash.
    #[test]
    fn missing_file_falls_back_to_defaults() {
        let path = std::env::temp_dir().join("saola-capture-test-definitely-missing.toml");
        std::fs::remove_file(&path).ok();

        let config = CaptureConfig::load_from(&path);

        assert_eq!(config, CaptureConfig::default());
    }

    /// A missing `capture.toml` with a sibling `capture.kdl` still resolves
    /// to defaults (not an error) — this only proves the migration-hint
    /// path doesn't change the resolved config, since the hint itself is
    /// `eprintln!`-only and not something a unit test can assert on
    /// directly without capturing stderr.
    #[test]
    fn missing_toml_with_stale_kdl_sibling_still_falls_back_to_defaults() {
        let dir = std::env::temp_dir();
        let toml_path = dir.join(format!(
            "saola-capture-test-migration-{}.toml",
            std::process::id()
        ));
        let kdl_path = toml_path.with_file_name(format!(
            "saola-capture-test-migration-{}.kdl",
            std::process::id()
        ));
        // Not a real sibling name collision (the two share the process-id
        // stem, not the fixed "capture" stem `warn_if_stale_kdl_sibling`
        // actually checks for), so this only exercises `load_from`'s
        // missing-file branch safely under `cargo test`'s parallel runs —
        // see `warn_if_stale_kdl_sibling`'s own doc comment for the real
        // fixed-name behavior, which is by construction (`with_file_name
        // ("capture.kdl")`) and not independently unit-tested here to avoid
        // a shared fixed path across parallel test threads.
        std::fs::remove_file(&toml_path).ok();
        std::fs::remove_file(&kdl_path).ok();

        let config = CaptureConfig::load_from(&toml_path);

        assert_eq!(config, CaptureConfig::default());
    }

    /// A nonsense `image-format` warns and defaults, but leaves the rest of
    /// the document intact — proves per-knob (not whole-document) fallback.
    #[test]
    fn nonsense_image_format_falls_back_to_default_and_keeps_the_rest() {
        let toml = r#"
            image-format = "jpeg"
            delay = 7
        "#;
        let config = CaptureConfig::parse(toml).expect("well-formed TOML");
        assert_eq!(config.image_format, ImageFormat::default());
        assert_eq!(config.delay, 7, "a bad image-format must not blank delay");
    }

    #[test]
    fn nonsense_video_preset_falls_back_to_default() {
        let toml = r#"video-preset = "prores""#;
        let config = CaptureConfig::parse(toml).expect("well-formed TOML");
        assert_eq!(config.video_preset, VideoPreset::default());
    }

    #[test]
    fn nonsense_bool_falls_back_to_default() {
        let toml = r#"cursor = "sure""#;
        let config = CaptureConfig::parse(toml).expect("well-formed TOML");
        assert!(
            config.cursor,
            "a non-boolean cursor value keeps the default"
        );
    }

    #[test]
    fn out_of_range_webp_quality_falls_back_to_the_default() {
        for toml in [
            "webp-quality = 0",
            "webp-quality = 101",
            "webp-quality = -5",
        ] {
            let config = CaptureConfig::parse(toml).expect("well-formed TOML");
            assert_eq!(
                config.webp_quality, DEFAULT_WEBP_QUALITY,
                "{toml} must warn and default"
            );
        }
    }

    #[test]
    fn fractional_webp_quality_falls_back_to_the_default() {
        let config = CaptureConfig::parse("webp-quality = 82.5").expect("well-formed TOML");
        assert_eq!(config.webp_quality, DEFAULT_WEBP_QUALITY);
    }

    #[test]
    fn webp_quality_accepts_the_range_boundaries() {
        assert_eq!(
            CaptureConfig::parse("webp-quality = 1")
                .expect("valid")
                .webp_quality,
            1
        );
        assert_eq!(
            CaptureConfig::parse("webp-quality = 100")
                .expect("valid")
                .webp_quality,
            100
        );
    }

    #[test]
    fn negative_delay_falls_back_to_default() {
        let toml = "delay = -1";
        let config = CaptureConfig::parse(toml).expect("well-formed TOML");
        assert_eq!(config.delay, 0);
    }

    #[test]
    fn fractional_delay_falls_back_to_default() {
        let toml = "delay = 1.5";
        let config = CaptureConfig::parse(toml).expect("well-formed TOML");
        assert_eq!(config.delay, 0, "delay is whole seconds only");
    }

    // -- resolve_path precedence --------------------------------------

    #[test]
    fn config_dir_precedence_cli_wins() {
        let dir = config_dir_from(
            Some(Path::new("/cli/dir")),
            Some("/saola/dir".into()),
            Some("/xdg/dir".into()),
            Some("/home/jordan".into()),
        );
        assert_eq!(dir, Some(PathBuf::from("/cli/dir")));
    }

    #[test]
    fn config_dir_precedence_saola_over_xdg_and_home() {
        let dir = config_dir_from(
            None,
            Some("/saola/dir".into()),
            Some("/xdg/dir".into()),
            Some("/home/jordan".into()),
        );
        assert_eq!(dir, Some(PathBuf::from("/saola/dir")));
    }

    #[test]
    fn config_dir_precedence_xdg_over_home() {
        let dir = config_dir_from(
            None,
            None,
            Some("/xdg/dir".into()),
            Some("/home/jordan".into()),
        );
        assert_eq!(dir, Some(PathBuf::from("/xdg/dir/saola")));
    }

    #[test]
    fn config_dir_falls_back_to_home() {
        let dir = config_dir_from(None, None, None, Some("/home/jordan".into()));
        assert_eq!(dir, Some(PathBuf::from("/home/jordan/.config/saola")));
    }

    #[test]
    fn config_dir_empty_env_vars_are_treated_as_unset() {
        // `VAR=` in a shell one-liner clears a variable, not names a
        // directory — the XDG spec's own rule, applied uniformly.
        let dir = config_dir_from(
            None,
            Some("".into()),
            Some("".into()),
            Some("/home/jordan".into()),
        );
        assert_eq!(dir, Some(PathBuf::from("/home/jordan/.config/saola")));
    }

    #[test]
    fn config_dir_none_when_nothing_resolves() {
        let dir = config_dir_from(None, None, None, None);
        assert_eq!(dir, None);
    }

    #[test]
    fn tilde_expands_against_home() {
        assert_eq!(
            expand_tilde_with_home("~/Pictures", Some("/home/jordan".into())),
            PathBuf::from("/home/jordan/Pictures")
        );
        assert_eq!(
            expand_tilde_with_home("~", Some("/home/jordan".into())),
            PathBuf::from("/home/jordan")
        );
        assert_eq!(
            expand_tilde_with_home("/already/absolute", Some("/home/jordan".into())),
            PathBuf::from("/already/absolute")
        );
    }
}
