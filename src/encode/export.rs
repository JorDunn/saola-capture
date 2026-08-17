//! One-shot GIF / animated-WebP export of an existing recording (PLAN.md
//! Stage 16, task 3) — `modules::history`'s per-recording "Export" action.
//!
//! # A batch job, not an `EncoderSink` (teaching note)
//!
//! Everything else in `encode/` is built around [`super::EncoderSink`]: a
//! *live* sink fed frame-by-frame while a recording is in flight. This
//! module is the opposite shape — `ffmpeg` is handed a path to a file that
//! already exists in full, told to read it start to finish, and waited on
//! synchronously with [`std::process::Command::output`]. There is no stream
//! to keep alive, no backpressure to manage, and (unlike `ffmpeg_cli::
//! FfmpegSink`) no risk of a filled stderr pipe deadlocking a write to
//! stdin — this module never writes to `ffmpeg`'s stdin at all, so
//! `Command::output()`'s own internal stdout/stderr draining (which the
//! standard library documents as deadlock-safe, unlike a hand-rolled
//! `wait()` before reading the pipes) is sufficient with no extra thread.
//!
//! # GIF: the two-pass palette dance
//!
//! A GIF frame is 256-color-indexed. Muxing straight to `gif` output with no
//! palette step makes ffmpeg fall back to a fixed 216-color "web safe"
//! palette, which bands and dithers badly on screen-recording content
//! (text, UI chrome — exactly the high-frequency detail a fixed palette
//! handles worst). The fix is ffmpeg's own documented two-pass recipe,
//! implemented in [`export_gif`]: pass 1 (`palettegen`) analyzes every frame
//! and writes an optimal 256-color palette to a scratch PNG; pass 2
//! (`paletteuse`) re-reads the source and dithers every frame onto that
//! palette. Two full `ffmpeg` invocations, the second reading the first's
//! output — there is no single-pass way to get a good GIF palette.
//!
//! # Animated WebP: one pass
//!
//! `libwebp`'s animation encoder has no equivalent palette step (it is not
//! a strictly palette-indexed format the way GIF is) — `ffmpeg`'s
//! `libwebp_anim` muxer does its own internal quality/compression tuning in
//! a single pass. Note this goes through **ffmpeg's own encoder**, not the
//! `webp` crate already vendored in `Cargo.toml` — that crate wraps
//! `libwebp-sys` for still-image encoding only (see its own survey essay);
//! animation was never in its scope, and ffmpeg is already the sole
//! external CLI this crate uses for anything video-shaped, so reaching for
//! its `libwebp_anim` muxer adds no new dependency at all.
//!
//! # The size warning (PLAN.md task 3: "a size warning teaching-note in the
//! UI copy")
//!
//! Both formats store (an approximation of) every frame independently —
//! neither has HEVC/AV1-style interframe prediction. A 30-second clip that
//! is a few MB as HEVC can easily become tens of MB as GIF or animated
//! WebP. This module does not resize or trim automatically (a silent
//! resolution change would surprise someone who asked for "a GIF of this");
//! `modules::history` is what carries the warning text in the UI, per the
//! task brief. What this module *does* do is report the resulting file's
//! real size on success, so the caller has an actual number rather than a
//! guess.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::ffmpeg_cli::ensure_ffmpeg_available;
use super::EncodeError;

/// How many stderr lines to keep on a failed `ffmpeg` invocation — same
/// number `encode::EncodeError`'s streaming sibling settled on
/// (`ffmpeg_cli::FfmpegSink`'s `MAX_STDERR_TAIL`), for the same reason:
/// generous enough to catch a real error a few lines up from EOF, small
/// enough not to dump ffmpeg's whole banner into a toast.
const STDERR_TAIL_LINES: usize = 40;

/// The frame rate every export is resampled to. A recording's own cadence is
/// damage-driven and can run well past 60 fps in bursts (CAPTURE-RESEARCH
/// §4.5); a GIF or animated WebP at that rate is enormous for no visible
/// benefit on ordinary screen-recording content, so `ffmpeg`'s own `fps`
/// filter downsamples to this before either encoder ever sees a frame.
const EXPORT_FPS: u32 = 12;

/// `libwebp_anim`'s lossy quality, `0..=100`. `75` is ffmpeg's own upstream
/// default for `libwebp` still images — reused here rather than inventing a
/// new number, since nothing about "animated" changes what a reasonable
/// default quality is.
const ANIMATED_WEBP_QUALITY: u32 = 75;

/// The two export formats PLAN.md Stage 16 task 3 names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimatedFormat {
    Gif,
    AnimatedWebp,
}

impl AnimatedFormat {
    pub fn extension(self) -> &'static str {
        match self {
            AnimatedFormat::Gif => "gif",
            AnimatedFormat::AnimatedWebp => "webp",
        }
    }

    /// The label `modules::history`'s buttons show — kept here rather than
    /// duplicated in the UI module, so the two can't say different things
    /// about the same format.
    pub fn label(self) -> &'static str {
        match self {
            AnimatedFormat::Gif => "GIF",
            AnimatedFormat::AnimatedWebp => "Animated WebP",
        }
    }
}

impl fmt::Display for AnimatedFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Everything that can go wrong on top of [`EncodeError`] — one extra
/// variant this batch path needs that the streaming sink never did: running
/// out of collision-free filenames (`ffmpeg_cli`'s sinks always write a
/// freshly [`crate::storage::allocate_recording_path`]-allocated path, which
/// cannot already exist; an export target is *derived* from an existing
/// recording's own name, so a repeat export of the same recording is a real
/// collision this module has to resolve itself — see [`unique_export_path`]).
#[derive(Debug)]
pub enum ExportError {
    Encode(EncodeError),
    NoFreeFilename(PathBuf),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::Encode(err) => write!(f, "{err}"),
            ExportError::NoFreeFilename(dir) => write!(
                f,
                "could not find an unused export filename in {} — is it full of exports \
                 already?",
                dir.display()
            ),
        }
    }
}

impl std::error::Error for ExportError {}

impl From<EncodeError> for ExportError {
    fn from(err: EncodeError) -> Self {
        ExportError::Encode(err)
    }
}

/// Exports `source` (an existing recording) to `format`, returning the
/// written path. `source` is untouched; the export always lands beside it
/// with a fresh, collision-free name (see [`unique_export_path`]).
pub fn export(source: &Path, format: AnimatedFormat) -> Result<PathBuf, ExportError> {
    ensure_ffmpeg_available()?;
    let out = unique_export_path(source, format)?;

    let result = match format {
        AnimatedFormat::Gif => export_gif(source, &out),
        AnimatedFormat::AnimatedWebp => export_animated_webp(source, &out),
    };

    if let Err(err) = result {
        // Don't leave a half-written (or zero-byte, on an early failure)
        // export sitting next to the recording it came from.
        let _ = fs::remove_file(&out);
        return Err(err.into());
    }

    match fs::metadata(&out) {
        Ok(meta) if meta.len() > 0 => Ok(out),
        _ => {
            let _ = fs::remove_file(&out);
            Err(ExportError::Encode(EncodeError::EmptyOutput(out)))
        }
    }
}

/// [`export`], plus the resulting file's size — what `modules::history`
/// actually wants to show ("Exported: clip.gif (18.4 MB)"), so it does not
/// need a second `fs::metadata` call of its own.
pub fn export_with_size(
    source: &Path,
    format: AnimatedFormat,
) -> Result<(PathBuf, u64), ExportError> {
    let path = export(source, format)?;
    let bytes = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    Ok((path, bytes))
}

// ---------------------------------------------------------------------
// Filenames
// ---------------------------------------------------------------------

/// `source` with its extension swapped for `format`'s, suffixed `-1`, `-2`,
/// … the same way [`crate::storage`]'s own collision handling does, if the
/// bare name is already taken (a repeat export of the same recording, or a
/// GIF and a WebP export of the same clip landing on the same stem).
fn unique_export_path(source: &Path, format: AnimatedFormat) -> Result<PathBuf, ExportError> {
    let base = source.with_extension(format.extension());
    if !base.exists() {
        return Ok(base);
    }

    let dir = base
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let stem = base
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("export")
        .to_string();
    let extension = format.extension();

    for attempt in 1..1000u32 {
        let candidate = dir.join(format!("{stem}-{attempt}.{extension}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(ExportError::NoFreeFilename(dir))
}

// ---------------------------------------------------------------------
// ffmpeg invocations
// ---------------------------------------------------------------------

/// `fps=<EXPORT_FPS>` — the one filter stage both formats share, factored out
/// so the two encoders' filter chains cannot quietly drift on the frame rate.
fn fps_filter() -> String {
    format!("fps={EXPORT_FPS}")
}

fn export_gif(source: &Path, out: &Path) -> Result<(), EncodeError> {
    // A scratch palette PNG next to the source, dot-prefixed like every
    // other transient file this crate writes (`storage::write_atomically`'s
    // own `.name.part` convention) — same filesystem as `source`, so no
    // cross-mount surprises, removed in every exit path below.
    let palette = source.with_file_name(format!(
        ".{}-palette.png",
        source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("export")
    ));

    let pass1 = run_ffmpeg(gif_palettegen_args(source, &palette));
    if pass1.is_err() {
        let _ = fs::remove_file(&palette);
        return pass1;
    }

    let pass2 = run_ffmpeg(gif_paletteuse_args(source, &palette, out));
    let _ = fs::remove_file(&palette);
    pass2
}

fn export_animated_webp(source: &Path, out: &Path) -> Result<(), EncodeError> {
    run_ffmpeg(animated_webp_args(source, out))
}

/// Pure argument builders — kept separate from [`run_ffmpeg`] itself so the
/// exact command lines are unit-testable without spawning `ffmpeg`, the same
/// split `encode/mod.rs`'s `ffmpeg_args`/`filter_chain` already use for the
/// live recording path. Each pushes plain owned [`OsString`]s onto a `Vec`
/// by hand rather than building one through an array literal — a `-vf`
/// filter string is computed (`format!`) while its neighbors are `'static`
/// literals, and an array literal needs one uniform element type, which a
/// borrowed `&str` literal and a `&format!(..)` (a `&String`) are not
/// without an extra `as_str()` at every call site; pushing one at a time
/// sidesteps that entirely and reads as a literal transcript of the
/// argv this builds.
fn gif_palettegen_args(source: &Path, palette: &Path) -> Vec<OsString> {
    let mut out = Vec::new();
    push(&mut out, "-y");
    push(&mut out, "-i");
    push_path(&mut out, source);
    push(&mut out, "-vf");
    push(
        &mut out,
        &format!("{},palettegen=stats_mode=diff", fps_filter()),
    );
    push_path(&mut out, palette);
    out
}

fn gif_paletteuse_args(source: &Path, palette: &Path, out_path: &Path) -> Vec<OsString> {
    let mut out = Vec::new();
    push(&mut out, "-y");
    push(&mut out, "-i");
    push_path(&mut out, source);
    push(&mut out, "-i");
    push_path(&mut out, palette);
    push(&mut out, "-lavfi");
    push(
        &mut out,
        &format!("{}[x];[x][1:v]paletteuse=dither=sierra2_4a", fps_filter()),
    );
    push(&mut out, "-loop");
    push(&mut out, "0");
    push_path(&mut out, out_path);
    out
}

fn animated_webp_args(source: &Path, out_path: &Path) -> Vec<OsString> {
    let mut out = Vec::new();
    push(&mut out, "-y");
    push(&mut out, "-i");
    push_path(&mut out, source);
    push(&mut out, "-vf");
    push(&mut out, &fps_filter());
    push(&mut out, "-loop");
    push(&mut out, "0");
    push(&mut out, "-lossless");
    push(&mut out, "0");
    push(&mut out, "-q:v");
    push(&mut out, &ANIMATED_WEBP_QUALITY.to_string());
    push(&mut out, "-an");
    push_path(&mut out, out_path);
    out
}

fn push(args: &mut Vec<OsString>, value: &str) {
    args.push(OsString::from(value));
}

fn push_path(args: &mut Vec<OsString>, path: &Path) {
    args.push(path.as_os_str().to_owned());
}

/// Runs `ffmpeg` with `args`, waits for it, and turns a non-zero exit into
/// [`EncodeError::Died`] with the stderr tail — the one thing every export
/// invocation shares. `Command::output()` (not a manual `spawn` + `wait` +
/// separately-read pipes) is deliberate: see this module's doc comment on
/// why the streaming sink's stderr-drain thread has no equivalent need here.
fn run_ffmpeg(args: Vec<OsString>) -> Result<(), EncodeError> {
    let output = Command::new("ffmpeg")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(EncodeError::Spawn)?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail: Vec<String> = stderr
        .lines()
        .rev()
        .take(STDERR_TAIL_LINES)
        .map(str::to_string)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    Err(EncodeError::Died {
        code: output.status.code(),
        tail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- AnimatedFormat --------------------------------------------------

    #[test]
    fn extensions_match_the_format() {
        assert_eq!(AnimatedFormat::Gif.extension(), "gif");
        assert_eq!(AnimatedFormat::AnimatedWebp.extension(), "webp");
    }

    // -- unique_export_path -----------------------------------------------

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "saola-capture-export-test-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            fs::create_dir_all(&dir).expect("scratch dir");
            TempDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_fresh_stem_needs_no_suffix() {
        let dir = TempDir::new("fresh");
        let source = dir.path().join("Recording_2026-08-08_12-00-00.mkv");
        let out = unique_export_path(&source, AnimatedFormat::Gif).expect("no collision");
        assert_eq!(out, dir.path().join("Recording_2026-08-08_12-00-00.gif"));
    }

    #[test]
    fn a_taken_stem_gets_a_dash_one_suffix() {
        let dir = TempDir::new("taken");
        let source = dir.path().join("Recording_2026-08-08_12-00-00.mkv");
        fs::write(dir.path().join("Recording_2026-08-08_12-00-00.gif"), b"x").expect("write");

        let out = unique_export_path(&source, AnimatedFormat::Gif).expect("finds -1");
        assert_eq!(out, dir.path().join("Recording_2026-08-08_12-00-00-1.gif"));
    }

    #[test]
    fn gif_and_webp_exports_of_the_same_source_never_collide_with_each_other() {
        let dir = TempDir::new("two-formats");
        let source = dir.path().join("Recording_2026-08-08_12-00-00.mkv");

        let gif = unique_export_path(&source, AnimatedFormat::Gif).expect("gif path");
        let webp = unique_export_path(&source, AnimatedFormat::AnimatedWebp).expect("webp path");
        assert_ne!(gif, webp);
        assert_eq!(gif.extension().and_then(|e| e.to_str()), Some("gif"));
        assert_eq!(webp.extension().and_then(|e| e.to_str()), Some("webp"));
    }

    // -- argument builders -------------------------------------------------

    #[test]
    fn gif_palettegen_reads_the_source_and_writes_the_palette() {
        let source = Path::new("/tmp/clip.mkv");
        let palette = Path::new("/tmp/.clip-palette.png");
        let built = gif_palettegen_args(source, palette);

        assert_eq!(built.first(), Some(&OsString::from("-y")));
        assert!(built.contains(&OsString::from(source.as_os_str())));
        assert!(built.contains(&OsString::from(palette.as_os_str())));
        assert!(built
            .iter()
            .any(|arg| arg.to_string_lossy().contains("palettegen")));
        assert!(built
            .iter()
            .any(|arg| arg.to_string_lossy().contains(&format!("fps={EXPORT_FPS}"))));
    }

    #[test]
    fn gif_paletteuse_reads_both_inputs_and_writes_the_final_gif() {
        let source = Path::new("/tmp/clip.mkv");
        let palette = Path::new("/tmp/.clip-palette.png");
        let out = Path::new("/tmp/clip.gif");
        let built = gif_paletteuse_args(source, palette, out);

        assert!(built.contains(&OsString::from(source.as_os_str())));
        assert!(built.contains(&OsString::from(palette.as_os_str())));
        assert!(built.contains(&OsString::from(out.as_os_str())));
        assert!(built
            .iter()
            .any(|arg| arg.to_string_lossy().contains("paletteuse")));
        // Infinite loop, per an animated export's whole point.
        let loop_index = built
            .iter()
            .position(|arg| arg == "-loop")
            .expect("a -loop flag");
        assert_eq!(built.get(loop_index + 1), Some(&OsString::from("0")));
    }

    #[test]
    fn animated_webp_args_are_lossy_and_carry_no_audio() {
        let source = Path::new("/tmp/clip.mkv");
        let out = Path::new("/tmp/clip.webp");
        let built = animated_webp_args(source, out);

        assert!(built.contains(&OsString::from("-an")), "no audio stream");
        assert!(built.contains(&OsString::from("-lossless")));
        let lossless_index = built.iter().position(|arg| arg == "-lossless").unwrap();
        assert_eq!(built.get(lossless_index + 1), Some(&OsString::from("0")));
        assert!(built.contains(&OsString::from(out.as_os_str())));
    }

    // `run_ffmpeg` itself (a real spawn) is deliberately not unit-tested —
    // same posture `ffmpeg_cli.rs`'s own `run_probe`/`FfmpegSink::start` take
    // (per that module's test module, only pure helpers are exercised
    // directly): a real invocation needs a real `ffmpeg` binary, which
    // `cargo test` cannot assume, and there is no way to fake `Command::new`
    // without an indirection this module has no other reason to carry.
    // `ensure_ffmpeg_available` (the up-front missing-binary check `export`
    // calls before any of this) is already covered by `ffmpeg_cli.rs`'s own
    // tests.
}
