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

/// The whole of `capture.toml`, resolved to typed values.
///
/// **Unchanged from Stage 3's KDL-backed version** — same fields, same
/// defaults, same public API. Only the file format and this struct's
/// parsing internals moved; every caller in `main.rs`/`cli.rs` is untouched
/// by this stage.
///
/// `save_dir` is deliberately `Option<PathBuf>`, not a plain `PathBuf` with
/// `~/Pictures/Captures` baked in here: Stage 5's `storage.rs` owns that
/// fallback (PLAN.md Stage 5, item 3 — "save-dir resolution (config →
/// `~/Pictures/Captures` fallback ..., created on demand)"), so this module
/// only reports what the *file* said (or didn't). Every other field has no
/// such downstream owner, so it resolves to a concrete default right here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureConfig {
    pub save_dir: Option<PathBuf>,
    pub image_format: ImageFormat,
    pub png_also: bool,
    pub video_preset: VideoPreset,
    pub cursor: bool,
    pub delay: u32,
    pub toasts: bool,
    pub copy: bool,
}

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
            png_also: false,
            video_preset: VideoPreset::default(),
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

        let png_also = read_bool(&body, "png-also").unwrap_or(false);

        let video_preset = read_str(&body, "video-preset")
            .and_then(|value| match_or_warn(value, "video-preset", VideoPreset::parse))
            .unwrap_or_default();

        let cursor = read_bool(&body, "cursor").unwrap_or(true);

        let delay = read_delay(&body).unwrap_or(0);

        let toasts = read_bool(&body, "toasts").unwrap_or(true);

        let copy = read_bool(&body, "copy").unwrap_or(true);

        Ok(CaptureConfig {
            save_dir,
            image_format,
            png_also,
            video_preset,
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
            png-also = true
            video-preset = "av1"
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
        assert!(config.png_also);
        assert_eq!(config.video_preset, VideoPreset::Av1);
        assert!(!config.cursor);
        assert_eq!(config.delay, 3);
        assert!(!config.toasts);
        assert!(!config.copy);
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
        assert!(config.cursor);
        assert!(config.toasts);
        assert!(config.copy);
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
