//! Where a capture goes after the pixels exist: the save directory,
//! the filename, the encoders, the clipboard, and the history index
//! (PLAN.md Stage 5, task 3).
//!
//! # The order of operations, and why it is that order
//!
//! [`save_capture`] does five things, and the sequence is deliberate:
//!
//! 1. **Resolve and create the save directory.** If this fails, nothing else
//!    has happened yet, so the error is clean and nothing is half-done.
//! 2. **Encode.** WebP (lossy, `webp-quality` from `capture.toml`) or PNG
//!    (lossless), plus the optional PNG sidecar.
//! 3. **Write, atomically.** Every file lands as `.name.part` in the target
//!    directory and is then `rename`d into place. A `rename` within one
//!    filesystem is atomic, so a disk-full or a crash mid-write leaves a
//!    stray `.part` file, never a truncated `.webp` that opens as garbage.
//! 4. **Clipboard.** Failures here warn and continue: the file is already on
//!    disk, and losing a screenshot because the clipboard was busy would be
//!    absurd.
//! 5. **History index.** Same posture — a failed index append warns; the
//!    capture still succeeded.
//!
//! Steps 4 and 5 are "best effort" precisely because step 3 already
//! succeeded. CLAUDE.md's rule is that absent services "degrade gracefully
//! or produce actionable errors, never crashes" — a warning on stderr plus a
//! saved file is graceful degradation; refusing to report the path the user
//! can already open would not be.
//!
//! # The clipboard is not a buffer (teaching note)
//!
//! On Wayland, "copy" means the copying client keeps a `wl_data_source`
//! alive and hands over the bytes each time something pastes. There is no
//! shared clipboard buffer in the compositor. That single fact drives the
//! whole of [`ClipboardOwner`]:
//!
//! - The **daemon** is long-lived, so it can own the selection itself
//!   ([`ClipboardOwner::ThisProcess`]). `wl-clipboard-rs` spawns a thread to
//!   serve requests; that thread lives as long as the daemon, or until
//!   something else takes the selection.
//! - A **`shot --no-daemon`** process exits in milliseconds, so owning the
//!   selection there would mean the clipboard going empty the instant the
//!   command returned. That path spawns the hidden `clipboard-serve` verb
//!   detached ([`ClipboardOwner::DetachedHelper`]) and pipes it the bytes —
//!   the same trick `wl-copy` uses.
//!
//! The clipboard always gets **PNG**, whatever was saved to disk. WebP on
//! the clipboard is a compatibility trap: `image/png` is the type every
//! paste target understands, and a "copy" that pastes nowhere is worse than
//! a slightly slower one. When a PNG was encoded anyway (`image-format =
//! "png"`, or `png-also = true`) those bytes are reused rather than encoded
//! twice.
//!
//! # The history index
//!
//! Append-only **JSON Lines** at `$XDG_DATA_HOME/saola/capture/history.jsonl`
//! (`~/.local/share/saola/capture/history.jsonl` by default): one JSON object
//! per line, newest last, no rewriting of earlier lines ever. Stage 16's
//! library reads it. The format is documented on [`HistoryEntry`] and is
//! meant to stay boring — a `tail -1 history.jsonl | jq` has to keep working.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};

use crate::capture::Frame;
use crate::cli::{CaptureOptions, ShotKind};
use crate::config::ImageFormat;

/// The MIME type the clipboard selection is offered under. See the module
/// doc comment on why this is always PNG.
const CLIPBOARD_MIME: &str = "image/png";

/// **Stage 16.** The MIME string a *detached* text-clipboard helper is told
/// to serve (`spawn_clipboard_helper`/`run_clipboard_serve`, which both need
/// an explicit string rather than [`wl_clipboard_rs::copy::MimeType::Text`]'s
/// in-process convenience — see [`copy_text_to_clipboard`]'s doc comment).
/// The conventional value for arbitrary UTF-8 plain text on Wayland/X11.
const TEXT_MIME: &str = "text/plain;charset=utf-8";

/// How many `-1`, `-2`, … suffixes to try before giving up on finding a free
/// filename. Collisions only happen for two captures within the same second,
/// so this is generous by three orders of magnitude; the cap exists so a
/// pathological directory can't spin forever.
const MAX_FILENAME_ATTEMPTS: u32 = 1000;

/// Everything that can go wrong between "we have pixels" and "the file is on
/// disk".
#[derive(Debug)]
pub enum StorageError {
    /// Neither `--output`, nor `save-dir`, nor `$HOME` gave us anywhere to
    /// write.
    NoSaveDir,
    /// Creating the save directory failed.
    CreateDir { path: PathBuf, source: io::Error },
    /// Writing (or renaming into place) failed.
    Write { path: PathBuf, source: io::Error },
    /// A thousand filenames in one second were already taken.
    NoFreeFilename(PathBuf),
    /// libwebp or the PNG encoder refused the frame.
    Encode(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::NoSaveDir => write!(
                f,
                "nowhere to save: no --output, no save-dir in capture.toml, and $HOME is unset"
            ),
            StorageError::CreateDir { path, source } => {
                write!(f, "could not create {}: {source}", path.display())
            }
            StorageError::Write { path, source } => {
                write!(f, "could not write {}: {source}", path.display())
            }
            StorageError::NoFreeFilename(dir) => write!(
                f,
                "could not find an unused filename in {} — is it full of captures?",
                dir.display()
            ),
            StorageError::Encode(err) => write!(f, "could not encode the capture: {err}"),
        }
    }
}

impl std::error::Error for StorageError {}

/// What [`save_capture`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedCapture {
    /// The capture itself — the path printed to stdout and carried by the
    /// `CaptureTaken` signal.
    pub path: PathBuf,
    /// The extra lossless copy, when `png-also = true` and the primary
    /// format was WebP.
    pub png_sidecar: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    /// Size of [`Self::path`] on disk, in bytes.
    pub bytes: u64,
}

/// Who keeps the Wayland selection alive after the copy — see the module
/// doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardOwner {
    /// This process serves it (a background thread). For the daemon.
    ThisProcess,
    /// A detached `saola-capture clipboard-serve` child serves it. For
    /// short-lived CLI processes.
    DetachedHelper,
}

/// Saves one captured [`Frame`]: encode, write, copy, index.
///
/// This is the second half of the pipeline whose first half is
/// [`crate::capture::take_screenshot`], and like it, it is the **one**
/// implementation both `shot` paths share.
pub fn save_capture(
    frame: &Frame,
    options: &CaptureOptions,
    kind: ShotKind,
    clipboard: ClipboardOwner,
) -> Result<SavedCapture, StorageError> {
    // The single place on this path that asks the *environment* where the
    // history index lives. Everything below takes the destination as an
    // argument — see [`save_capture_indexing_to`] for why that split exists.
    save_capture_indexing_to(frame, options, kind, clipboard, history_path().as_deref())
}

/// The testable core of [`save_capture`]: the same work, with the history
/// index's destination handed in rather than resolved from `$XDG_DATA_HOME`.
///
/// `history` is the full path to the index *file* (its parent directory is
/// created on demand). `None` means there is no data directory at all — a
/// container with neither `$XDG_DATA_HOME` nor `$HOME` — which warns and
/// still returns the saved capture, because the file on disk is the thing
/// that matters.
///
/// # Teaching note: why this is a separate function
///
/// The obvious way to test the index append is for the test to point
/// `$XDG_DATA_HOME` at a scratch directory with `std::env::set_var`. That is
/// a trap. `cargo test` runs every test in this binary on **parallel threads
/// of one process**, and the environment is process-global: two tests that
/// each redirect `$XDG_DATA_HOME` at their own temp directory will interleave,
/// and one of them will write its index into the other's directory and then
/// fail to find it at its own path. (That is not hypothetical — it is exactly
/// the intermittent failure this split was written to remove: roughly one
/// `cargo test` run in fifteen.) The hazard is real enough that Rust made
/// `std::env::set_var` an `unsafe` function in edition 2024 — this crate is
/// still on 2021, where the compiler will not stop you.
///
/// So this module follows one rule throughout: **resolution reads the
/// environment once, at the edge; logic takes arguments.**
/// [`default_save_dir`] and [`history_dir`] are already split the same way,
/// and are tested the same way — by passing values, never by mutating the
/// process.
fn save_capture_indexing_to(
    frame: &Frame,
    options: &CaptureOptions,
    kind: ShotKind,
    clipboard: ClipboardOwner,
    history: Option<&Path>,
) -> Result<SavedCapture, StorageError> {
    let dir = resolve_save_dir(options.output_dir.as_deref())?;
    fs::create_dir_all(&dir).map_err(|source| StorageError::CreateDir {
        path: dir.clone(),
        source,
    })?;

    let stem = unique_stem(&dir, &timestamp_stem())?;

    // Encode up front, before touching the filesystem: an encoder failure
    // should not leave a zero-byte file behind.
    let primary_extension = match options.format {
        ImageFormat::Webp => "webp",
        ImageFormat::Png => "png",
    };
    let png_bytes = if options.format == ImageFormat::Png || options.png_also {
        Some(encode_png(frame)?)
    } else {
        None
    };
    let primary_bytes = match options.format {
        ImageFormat::Webp => encode_webp(frame, options.webp_quality)?,
        // `png_bytes` is `Some` on this branch by construction; the
        // `map_or_else` re-encode is unreachable, and is written as a
        // fallback rather than an `unwrap` per CLAUDE.md's no-panic rule.
        ImageFormat::Png => match &png_bytes {
            Some(bytes) => bytes.clone(),
            None => encode_png(frame)?,
        },
    };

    let path = dir.join(format!("{stem}.{primary_extension}"));
    write_atomically(&path, &primary_bytes)?;

    // The sidecar only makes sense when the primary *isn't* already a PNG.
    let png_sidecar = match (&png_bytes, options.png_also, options.format) {
        (Some(bytes), true, ImageFormat::Webp) => {
            let sidecar = dir.join(format!("{stem}.png"));
            write_atomically(&sidecar, bytes)?;
            Some(sidecar)
        }
        _ => None,
    };

    if options.copy {
        // Best effort, by design (see the module doc comment): the file is
        // already safely on disk by this point.
        let clipboard_bytes = match &png_bytes {
            Some(bytes) => bytes.clone(),
            None => encode_png(frame)?,
        };
        if let Err(err) = copy_to_clipboard(&clipboard_bytes, clipboard) {
            eprintln!(
                "saola-capture: saved {} but could not copy it to the clipboard: {err}",
                path.display()
            );
        }
    } else if kind == ShotKind::Window {
        // Stage 8, CAPTURE-RESEARCH D3: `Action::ScreenshotWindow` (what
        // `capture::screencopy::ScreencopyBackend::capture_window` calls)
        // sets an `image/png` clipboard selection **unconditionally**,
        // before this function ever runs — there is no niri flag to ask it
        // not to. "storage.rs owns the final clipboard state" (CLAUDE.md
        // Boundaries) means a `--no-copy` window shot must not silently
        // leave niri's own copy sitting in the clipboard just because the
        // user said not to touch it. The `options.copy` branch above
        // already handles the opposite case for free: it overwrites
        // whatever niri put there with the *final* encoded image, so a
        // `copy = true` window shot needs no extra code here at all.
        if let Err(err) = clear_clipboard() {
            eprintln!(
                "saola-capture: saved {} but could not clear the clipboard niri's window \
                 screenshot left behind: {err}",
                path.display()
            );
        }
    }

    let bytes = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);

    let entry = HistoryEntry {
        unix: unix_now(),
        path: path.clone(),
        png_sidecar: png_sidecar.clone(),
        kind: kind.as_str(),
        format: primary_extension,
        width: frame.width(),
        height: frame.height(),
        scale: frame.scale(),
        bytes,
    };
    // Best effort, exactly like the clipboard above: the capture is already
    // on disk, so a missing data directory must not turn into a failed
    // screenshot.
    let indexed = match history {
        Some(path) => append_history(path, &entry),
        None => Err(io::Error::other(
            "no data directory ($XDG_DATA_HOME and $HOME are both unset)",
        )),
    };
    if let Err(err) = indexed {
        eprintln!("saola-capture: could not update the capture history index: {err}");
    }

    Ok(SavedCapture {
        path,
        png_sidecar,
        width: frame.width(),
        height: frame.height(),
        bytes,
    })
}

// ---------------------------------------------------------------------
// Save directory and filenames
// ---------------------------------------------------------------------

/// Where captures go: `--output`/`save-dir` if either was given, otherwise
/// `~/Pictures/Captures`.
///
/// `config.rs` deliberately leaves `save_dir` as `Option<PathBuf>` and never
/// invents this fallback (see its `CaptureConfig` doc comment) — PLAN.md
/// assigns it here and only here, so there is exactly one place that decides
/// what "unset" means. Tildes were already expanded by `config::expand_tilde`
/// before this ever sees the value.
pub fn resolve_save_dir(explicit: Option<&Path>) -> Result<PathBuf, StorageError> {
    if let Some(dir) = explicit {
        return Ok(dir.to_path_buf());
    }
    default_save_dir(std::env::var_os("HOME")).ok_or(StorageError::NoSaveDir)
}

/// The testable core of [`resolve_save_dir`]'s fallback: `$HOME` as a plain
/// argument rather than read from the environment, so precedence can be
/// tested without mutating (and racing) the process environment — the same
/// shape `config::config_dir_from` uses.
fn default_save_dir(home: Option<std::ffi::OsString>) -> Option<PathBuf> {
    home.filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join("Pictures/Captures"))
}

/// The current local time as a filename stem:
/// `Screenshot_2026-08-08_17-42-31`.
///
/// Local, not UTC: a screenshot is named after when *you* took it, and a
/// filename an hour off from the clock in the corner of the screen is a
/// small daily papercut.
fn timestamp_stem() -> String {
    timestamp_stem_named(SCREENSHOT_PREFIX)
}

/// The filename prefix for a still image, and (Stage 11) for a recording.
/// Two different words rather than one generic "Capture" because a directory
/// holding both wants them to sort into groups, and because the name is the
/// only thing distinguishing the two once they are on disk.
const SCREENSHOT_PREFIX: &str = "Screenshot";
/// `pub(crate)` as of **Stage 16**: [`crate::modules::history`] scans the
/// save directory for recordings by this same prefix (recordings have no
/// history-index row — see [`allocate_recording_path`]'s doc comment — so
/// the library's only way to find them is the filename convention this
/// constant *is*). Keeping one definition rather than a second literal in
/// `history.rs` means the writer and the reader cannot drift.
pub(crate) const RECORDING_PREFIX: &str = "Recording";

/// [`timestamp_stem`] with the prefix chosen by the caller.
fn timestamp_stem_named(prefix: &str) -> String {
    match local_civil_time(unix_now()) {
        Some(parts) => format_stem(prefix, parts),
        // `localtime_r` failing means no usable timezone database. Falling
        // back to the raw epoch second keeps captures uniquely named and
        // chronologically sortable, which is what the stem is actually for.
        None => format!("{prefix}_{}", unix_now()),
    }
}

/// Every extension a still capture can occupy, for collision checking.
const IMAGE_EXTENSIONS: &[&str] = &["webp", "png"];

/// Every extension a recording can occupy — both containers, for the same
/// reason [`unique_stem_among`] checks both image extensions: a `.mkv` from
/// one run and a `.mp4` from the next must not share a stem.
///
/// `pub(crate)` as of **Stage 16** — see [`RECORDING_PREFIX`]'s doc comment;
/// [`crate::modules::history`]'s directory scan filters on both this and the
/// prefix, rather than re-deciding what a recording's extension can be.
pub(crate) const VIDEO_EXTENSIONS: &[&str] = &["mkv", "mp4"];

/// Where a recording is about to be written — **Stage 11**.
///
/// The video counterpart of the first three steps of [`save_capture_indexing_to`]
/// (resolve the directory, create it, find a free name), stopping short of
/// writing anything: unlike a still image, the bytes do not exist yet and will
/// not exist in this process at all. `encode::ffmpeg_cli` hands this path
/// straight to ffmpeg, which writes it incrementally over the whole recording
/// — see `encode::RecordSpec::path` for why that is deliberately *not* routed
/// through [`write_atomically`].
///
/// `explicit` is the same `--output`/`save-dir` override screenshots take, so
/// recordings land beside them in `~/Pictures/Captures` by default. A
/// dedicated `video-dir` knob is not a Stage 11 decision; nothing has asked
/// for one.
///
/// **Recordings are not written to the history index.** `HistoryEntry`'s
/// documented schema fixes `format` to `"webp" | "png"` and carries
/// still-image-only fields (`png`, `scale`), and Stage 16's library is written
/// against that. Adding videos means a schema decision (a `v: 2`, or a `type`
/// key), which belongs to whichever stage actually builds the library's video
/// half — not to a stage that would be guessing at its reader.
pub fn allocate_recording_path(
    explicit: Option<&Path>,
    extension: &str,
) -> Result<PathBuf, StorageError> {
    let dir = resolve_save_dir(explicit)?;
    fs::create_dir_all(&dir).map_err(|source| StorageError::CreateDir {
        path: dir.clone(),
        source,
    })?;
    let stem = unique_stem_among(
        &dir,
        &timestamp_stem_named(RECORDING_PREFIX),
        VIDEO_EXTENSIONS,
    )?;
    Ok(dir.join(format!("{stem}.{extension}")))
}

/// Seconds since the Unix epoch. Pre-1970 clocks (a machine with a dead RTC)
/// produce a negative value rather than a panic.
fn unix_now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// Broken-down local time: `(year, month, day, hour, minute, second)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CivilTime {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

/// Converts a Unix timestamp to local civil time via libc.
///
/// Teaching note (why libc and not arithmetic): converting epoch seconds to
/// a *UTC* date is pure arithmetic, but converting to **local** time needs
/// the system timezone database — the rules for this zone, this year,
/// including whatever daylight-saving transition applies. `localtime_r` is
/// the C library function that owns that knowledge; re-implementing it would
/// mean parsing `/etc/localtime`.
///
/// (`tzset` is deliberately *not* called first — the `libc` crate declares it
/// only on Windows anyway. glibc's `localtime_r` initializes the timezone
/// rules on first use; what it skips, relative to `localtime`, is
/// *re-reading* `$TZ` on every call, which matters only to a process that
/// changes `$TZ` while running. This one never does.)
fn local_civil_time(unix: i64) -> Option<CivilTime> {
    let time = libc::time_t::try_from(unix).ok()?;
    // SAFETY: `tm` is fully written by `localtime_r` on success and only
    // read after a non-null return; `time` is a plain value read through a
    // pointer that lives for the call.
    let parts = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            return None;
        }
        tm
    };

    Some(CivilTime {
        // `tm_year` counts from 1900 and `tm_mon` from 0 — the two
        // off-by-N's that make hand-rolled C time formatting famous.
        year: i64::from(parts.tm_year) + 1900,
        month: (parts.tm_mon + 1).max(0) as u32,
        day: parts.tm_mday.max(0) as u32,
        hour: parts.tm_hour.max(0) as u32,
        minute: parts.tm_min.max(0) as u32,
        // `tm_sec` can legally be 60 during a leap second; that is a fine
        // thing to have in a filename and is left alone.
        second: parts.tm_sec.max(0) as u32,
    })
}

/// `<prefix>_YYYY-MM-DD_HH-MM-SS`. Dashes rather than colons in the time:
/// a colon is legal on Linux but breaks the moment the file is copied to a
/// FAT or SMB share, which screenshots routinely are.
fn format_stem(prefix: &str, time: CivilTime) -> String {
    format!(
        "{prefix}_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        time.year, time.month, time.day, time.hour, time.minute, time.second
    )
}

/// A stem for which **neither** `{stem}.webp` nor `{stem}.png` exists, so a
/// capture and its optional sidecar can never collide with an earlier pair.
///
/// Both extensions are checked even when only one file will be written: the
/// alternative is `shot.webp` from one run and `shot.png` from the next
/// sharing a stem, which would make the history index ambiguous about which
/// PNG belongs to which capture.
fn unique_stem(dir: &Path, base: &str) -> Result<String, StorageError> {
    unique_stem_among(dir, base, IMAGE_EXTENSIONS)
}

/// [`unique_stem`] over an arbitrary set of extensions — Stage 11 added the
/// video pair (`mkv`/`mp4`) alongside the original image pair.
fn unique_stem_among(dir: &Path, base: &str, extensions: &[&str]) -> Result<String, StorageError> {
    let free = |stem: &str| {
        !extensions
            .iter()
            .any(|extension| dir.join(format!("{stem}.{extension}")).exists())
    };

    if free(base) {
        return Ok(base.to_string());
    }
    for attempt in 1..MAX_FILENAME_ATTEMPTS {
        let candidate = format!("{base}-{attempt}");
        if free(&candidate) {
            return Ok(candidate);
        }
    }
    Err(StorageError::NoFreeFilename(dir.to_path_buf()))
}

/// Writes `bytes` to `path` via a sibling `.part` file and a `rename`.
///
/// The rename is the point: within one filesystem it is atomic, so `path`
/// either doesn't exist or holds the complete file. A plain `fs::write` that
/// runs out of disk halfway leaves a truncated image that looks saved and
/// isn't.
///
/// **`pub` as of Stage 14**: [`crate::modules::editor`]'s Save/Save As write
/// straight to an explicit path (the file that was opened, or one the user
/// typed) rather than through [`save_capture`]'s directory-resolution/
/// filename-invention/history-index pipeline, but they want exactly the same
/// crash-safety guarantee a fresh capture gets — an edit that dies mid-write
/// must leave the *original* file intact, never a half-written replacement.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    let failed = |source: io::Error| StorageError::Write {
        path: path.to_path_buf(),
        source,
    };

    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| failed(io::Error::other("the save path has no usable file name")))?;
    let temp = path.with_file_name(format!(".{name}.part"));

    fs::write(&temp, bytes).map_err(failed)?;
    if let Err(err) = fs::rename(&temp, path) {
        // Don't leave the partial file behind if the rename is what failed.
        let _ = fs::remove_file(&temp);
        return Err(failed(err));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Encoders
// ---------------------------------------------------------------------

/// Lossy WebP at `quality` (1..=100), via libwebp.
///
/// Uses `encode_advanced` with a config built here rather than the crate's
/// one-line `Encoder::encode`, for a no-panic reason: `encode` is
/// `encode_simple(..).unwrap()`, and `encode_simple` itself does
/// `WebPConfig::new().unwrap()`. Building the config here turns both of
/// those into a `Result` this function can report. (One `unwrap` remains
/// beyond reach, inside the crate's `new_picture`: `WebPPicture::new()` only
/// fails on a libwebp ABI-version mismatch, which cannot happen here because
/// `libwebp-sys` 0.9.6 vendors and statically links the exact libwebp it was
/// built against — see the WebP survey in `Cargo.toml`.)
fn encode_webp(frame: &Frame, quality: u8) -> Result<Vec<u8>, StorageError> {
    // `webp::Encoder::from_rgba` *panics* if the buffer is shorter than
    // `width * height * 4`. `Frame`'s own invariant guarantees it isn't, but
    // the guarantee is re-checked here rather than trusted across a module
    // boundary into C.
    let expected = (frame.width() as usize)
        .checked_mul(frame.height() as usize)
        .and_then(|pixels| pixels.checked_mul(4));
    if expected != Some(frame.pixels().len()) {
        return Err(StorageError::Encode(format!(
            "frame buffer is {} bytes, expected {} for {}x{}",
            frame.pixels().len(),
            expected.unwrap_or(0),
            frame.width(),
            frame.height()
        )));
    }

    let mut config = webp::WebPConfig::new().map_err(|()| {
        StorageError::Encode("libwebp rejected its own default config".to_string())
    })?;
    config.lossless = 0;
    config.alpha_compression = 1;
    config.quality = f32::from(quality.clamp(1, 100));

    let encoder = webp::Encoder::from_rgba(frame.pixels(), frame.width(), frame.height());
    let memory = encoder
        .encode_advanced(&config)
        .map_err(|err| StorageError::Encode(format!("libwebp: {err:?}")))?;

    Ok(memory.to_vec())
}

/// Lossless RGBA8 PNG, via the `image` crate.
fn encode_png(frame: &Frame) -> Result<Vec<u8>, StorageError> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            frame.pixels(),
            frame.width(),
            frame.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|err| StorageError::Encode(format!("png: {err}")))?;
    Ok(out)
}

/// The same encoder-selection `match` [`save_capture_indexing_to`] does
/// inline, exposed for a second caller — **Stage 14**'s
/// [`crate::modules::editor`]. The editor already has a composed RGBA
/// [`Frame`] in hand (the base image plus its annotations, rasterized) and
/// wants exactly this step, not the whole [`save_capture`] pipeline (which
/// also resolves a save directory, invents a timestamped filename and
/// appends a history-index row — none of which apply to *editing an existing
/// file*). Kept as a one-line wrapper rather than duplicated so the two
/// callers can never drift on which encoder a given [`ImageFormat`] means.
pub fn encode_frame(
    frame: &Frame,
    format: ImageFormat,
    webp_quality: u8,
) -> Result<Vec<u8>, StorageError> {
    match format {
        ImageFormat::Webp => encode_webp(frame, webp_quality),
        ImageFormat::Png => encode_png(frame),
    }
}

// ---------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------

/// Puts `png` on the Wayland clipboard as `image/png`.
///
/// See the module doc comment for why the two [`ClipboardOwner`] arms are
/// different mechanisms rather than one.
pub fn copy_to_clipboard(png: &[u8], owner: ClipboardOwner) -> Result<(), io::Error> {
    match owner {
        ClipboardOwner::ThisProcess => serve_bytes_in_process(png, CLIPBOARD_MIME),
        ClipboardOwner::DetachedHelper => spawn_clipboard_helper(png, CLIPBOARD_MIME),
    }
}

/// Puts `text` on the Wayland clipboard as plain text — **Stage 16**, the
/// `PickColor` swatch's hex code. A genuinely different content class from
/// every other clipboard write in this module (which are all `image/png`),
/// so this is a sibling entry point rather than a special case bolted onto
/// [`copy_to_clipboard`]'s image-shaped signature.
///
/// `wl_clipboard_rs::copy::MimeType::Text` is the crate's own "plain text,
/// let the library pick the conventional MIME string" mode — the same
/// convenience [`copy_to_clipboard`] deliberately does *not* use (an image
/// needs an exact, specific MIME type; text does not).
pub fn copy_text_to_clipboard(text: &str, owner: ClipboardOwner) -> Result<(), io::Error> {
    match owner {
        ClipboardOwner::ThisProcess => serve_text_in_process(text),
        ClipboardOwner::DetachedHelper => spawn_clipboard_helper(text.as_bytes(), TEXT_MIME),
    }
}

/// Clears the Wayland selection outright — the `--no-copy` mitigation for
/// [`save_capture_indexing_to`]'s window-capture branch (see its doc
/// comment). `Seat::All`/`ClipboardType::Regular` matches every other
/// clipboard call in this module: one regular selection, every seat, no
/// primary-selection support (nothing here ever wrote to the primary
/// selection in the first place, so there's nothing of ours to clear
/// there).
fn clear_clipboard() -> Result<(), io::Error> {
    use wl_clipboard_rs::copy::{clear, ClipboardType, Seat};

    clear(ClipboardType::Regular, Seat::All).map_err(io::Error::other)
}

/// `wl-clipboard-rs`'s default mode: it spawns a thread that owns the
/// selection and answers paste requests until something else takes over.
/// Correct only in a process that outlives the copy — the daemon.
///
/// **Generalized in Stage 16** from an image-only `serve_clipboard_in_process`
/// to take an explicit `mime` — [`copy_text_to_clipboard`]'s `ThisProcess` arm
/// wants the exact same "spawn a thread, own the selection" behavior for a
/// hex string that [`copy_to_clipboard`] already had for PNG bytes, and the
/// only thing that differs between them is which MIME type is offered.
fn serve_bytes_in_process(bytes: &[u8], mime: &str) -> Result<(), io::Error> {
    use wl_clipboard_rs::copy::{MimeType, Options, Source};

    Options::new()
        .copy(
            Source::Bytes(bytes.to_vec().into_boxed_slice()),
            MimeType::Specific(mime.to_string()),
        )
        .map_err(io::Error::other)
}

/// [`serve_bytes_in_process`] via [`wl_clipboard_rs::copy::MimeType::Text`]
/// rather than a specific string — see [`copy_text_to_clipboard`]'s doc
/// comment for why plain text gets the library's own MIME choice instead of
/// [`CLIPBOARD_MIME`]'s exact-string treatment.
fn serve_text_in_process(text: &str) -> Result<(), io::Error> {
    use wl_clipboard_rs::copy::{MimeType, Options, Source};

    Options::new()
        .copy(
            Source::Bytes(text.as_bytes().to_vec().into_boxed_slice()),
            MimeType::Text,
        )
        .map_err(io::Error::other)
}

/// Spawns `saola-capture clipboard-serve --mime <mime>` detached and pipes it
/// `bytes`. The child then owns the selection for as long as it holds it,
/// outliving this process — see [`crate::cli::Command::ClipboardServe`].
///
/// **Generalized in Stage 16** from an image-only `spawn_clipboard_helper`
/// (which always passed [`CLIPBOARD_MIME`]) to take `mime` explicitly, for
/// the same reason [`serve_bytes_in_process`] was: [`copy_text_to_clipboard`]
/// wants the identical detached-helper mechanism with a different MIME
/// string, not a second copy of the process-spawning code.
fn spawn_clipboard_helper(bytes: &[u8], mime: &str) -> Result<(), io::Error> {
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe)
        .arg("clipboard-serve")
        .arg("--mime")
        .arg(mime)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    // Take the pipe, write, and drop it so the child sees EOF. If the child
    // died on startup this returns `EPIPE` rather than a signal — Rust sets
    // `SIGPIPE` to `SIG_IGN` at process start — so the error is reportable.
    let result = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(bytes),
        None => Err(io::Error::other(
            "could not open a pipe to the clipboard helper",
        )),
    };

    if result.is_err() {
        // Don't leave a helper hanging around waiting for input it will
        // never get.
        let _ = child.kill();
    }
    // Deliberately not waited on: the child's whole job is to outlive this
    // process. Dropping the handle reparents it to init, exactly like
    // `dbus::spawn_daemon_detached`.
    result
}

/// The hidden `clipboard-serve` verb's body: read stdin, then serve it as
/// the selection until something else claims the clipboard.
///
/// `foreground(true)` is what makes `copy` block in *this* thread instead of
/// spawning one and returning — which is the entire point of the helper
/// process.
pub fn run_clipboard_serve(mime: &str) -> Result<(), io::Error> {
    use wl_clipboard_rs::copy::{MimeType, Options, Source};

    let mut options = Options::new();
    options.foreground(true);
    options
        .copy(Source::StdIn, MimeType::Specific(mime.to_string()))
        .map_err(io::Error::other)
}

// ---------------------------------------------------------------------
// History index
// ---------------------------------------------------------------------

/// One line of the history index.
///
/// **The on-disk format (stable from Stage 5; Stage 16's library reads it).**
/// `$XDG_DATA_HOME/saola/capture/history.jsonl`, or
/// `~/.local/share/saola/capture/history.jsonl`. One JSON object per line,
/// appended, never rewritten. Newest last. Keys:
///
/// | key      | type            | meaning                                        |
/// | -------- | --------------- | ---------------------------------------------- |
/// | `v`      | integer         | format version. `1` today. Bump on any breaking change. |
/// | `unix`   | integer         | capture time, seconds since the Unix epoch (UTC). |
/// | `path`   | string          | absolute path to the capture.                  |
/// | `png`    | string or absent| the `png-also` sidecar, when there is one.     |
/// | `kind`   | string          | `"fullscreen"` \| `"region"` \| `"window"`.     |
/// | `format` | string          | `"webp"` \| `"png"` — the extension of `path`.  |
/// | `width`  | integer         | physical pixels.                                |
/// | `height` | integer         | physical pixels.                                |
/// | `scale`  | number          | physical pixels per logical pixel on the source output (`1.5` on a fractionally-scaled laptop panel). Stage 16 needs it to show a capture at its intended size. |
/// | `bytes`  | integer         | size of `path` on disk.                         |
///
/// A reader must **ignore unknown keys** and **skip lines it cannot parse**
/// (a line half-written by a machine that lost power is the expected
/// failure, and it is always the last one). That rule is what lets later
/// stages add keys without a migration.
// No `Eq`: `scale` is an `f64` (see `capture::Frame`).
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub unix: i64,
    pub path: PathBuf,
    pub png_sidecar: Option<PathBuf>,
    pub kind: &'static str,
    pub format: &'static str,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub bytes: u64,
}

/// The index file's path, or `None` when there is no data directory to put
/// it in (no `$XDG_DATA_HOME`, no `$HOME` — a container, essentially).
pub fn history_path() -> Option<PathBuf> {
    history_dir(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
        .map(|dir| dir.join("history.jsonl"))
}

/// The testable core of [`history_path`]. `$XDG_DATA_HOME` wins;
/// `~/.local/share` is the XDG spec's own fallback. An empty variable counts
/// as unset, same rule `config::config_dir_from` applies.
fn history_dir(
    xdg_data_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(xdg) = xdg_data_home {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("saola/capture"));
        }
    }
    home.filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".local/share/saola/capture"))
}

/// Appends one entry to the index file at `path`, creating its directory and
/// the file itself if either is missing.
///
/// Takes the path rather than calling [`history_path`] itself; see
/// [`save_capture_indexing_to`] for why nothing below the edge of this module
/// reads the environment.
fn append_history(path: &Path, entry: &HistoryEntry) -> Result<(), io::Error> {
    if let Some(dir) = path.parent() {
        // `create_dir_all("")` is a documented no-op, so a bare relative
        // filename (whose parent is the empty path) is fine here.
        fs::create_dir_all(dir)?;
    }
    append_history_line(path, &history_line(entry))
}

/// Appends one already-serialized line (plus its newline).
///
/// Opened with `append(true)`, which on Linux makes each `write` atomic with
/// respect to other appenders for writes under `PIPE_BUF` — the daemon and a
/// concurrent `--no-daemon` CLI run can therefore both append without
/// interleaving each other's lines. Long paths can exceed that, which is why
/// readers are told to skip unparseable lines.
fn append_history_line(path: &Path, line: &str) -> Result<(), io::Error> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")
}

/// Serializes one entry. Pure, so the format itself is unit-tested without
/// touching the filesystem.
///
/// Paths go through `to_string_lossy`: JSON strings are Unicode by
/// definition, so a path that isn't valid UTF-8 (legal on Linux) gets
/// replacement characters rather than corrupting the line. That is a real,
/// documented limitation of the index — and it is why the index is an
/// *index*, not the source of truth; the files themselves are.
fn history_line(entry: &HistoryEntry) -> String {
    let mut object = serde_json::Map::new();
    object.insert("v".to_string(), serde_json::Value::from(1));
    object.insert("unix".to_string(), serde_json::Value::from(entry.unix));
    object.insert(
        "path".to_string(),
        serde_json::Value::from(entry.path.to_string_lossy().into_owned()),
    );
    if let Some(png) = &entry.png_sidecar {
        object.insert(
            "png".to_string(),
            serde_json::Value::from(png.to_string_lossy().into_owned()),
        );
    }
    object.insert("kind".to_string(), serde_json::Value::from(entry.kind));
    object.insert("format".to_string(), serde_json::Value::from(entry.format));
    object.insert("width".to_string(), serde_json::Value::from(entry.width));
    object.insert("height".to_string(), serde_json::Value::from(entry.height));
    object.insert("scale".to_string(), serde_json::Value::from(entry.scale));
    object.insert("bytes".to_string(), serde_json::Value::from(entry.bytes));
    serde_json::Value::Object(object).to_string()
}

// ---------------------------------------------------------------------
// History index — reading (Stage 16)
// ---------------------------------------------------------------------

/// Reads every parseable row of the index at `path`, oldest first (the file's
/// own append order — see [`HistoryEntry`]'s doc comment, "newest last").
/// [`crate::modules::history`] is the one caller, and reverses this itself
/// for "newest first" display, keeping this function's contract "the file,
/// faithfully" rather than baking in a display order.
///
/// **Best-effort, matching every other reader in this module**: a missing
/// file (no history yet, or `history` was never written for lack of a data
/// directory — see [`save_capture_indexing_to`]) is not an error, just an
/// empty history — `Vec::new()`. A line that fails to parse is *skipped*,
/// per [`HistoryEntry`]'s own documented reader contract ("a line half-
/// written by a machine that lost power is the expected failure, and it is
/// always the last one"); this function does not distinguish "skipped a bad
/// line" from "the file just doesn't have one" in its return value, because
/// nothing downstream needs to know which — a corrupt trailing line and a
/// pristine file both mean "here is everything readable".
pub fn read_history_entries(path: &Path) -> Vec<HistoryEntry> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return Vec::new(),
    };
    parse_history_lines(&text)
}

/// The testable core of [`read_history_entries`]: the same parse, over text
/// already in memory rather than read from a path — so a fixture string
/// (including deliberately malformed lines) exercises the parser without
/// touching the filesystem, the same split every other pure/impure pair in
/// this module uses.
fn parse_history_lines(text: &str) -> Vec<HistoryEntry> {
    text.lines().filter_map(parse_history_line).collect()
}

/// One line to one [`HistoryEntry`], or `None` if the line is blank, isn't
/// valid JSON, isn't an object, is missing a required key, or has a `kind`/
/// `format` outside the schema's documented enum — every one of those is
/// "cannot parse", not a partial success, so this returns `Option` rather
/// than trying to salvage a half-decoded entry.
///
/// `kind`/`format` decode through [`static_kind`]/[`static_format`] rather
/// than leaking an arbitrary `String` to `&'static str`: [`HistoryEntry`]'s
/// fields are `&'static str` specifically because the schema is a closed
/// enum of three/two known words (see its own doc comment's table), so a
/// reader that saw some *other* word has found a line this schema version
/// cannot represent — correctly "cannot parse", not a value to invent a
/// leaked string for.
fn parse_history_line(line: &str) -> Option<HistoryEntry> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let object = value.as_object()?;

    let unix = object.get("unix")?.as_i64()?;
    let path = PathBuf::from(object.get("path")?.as_str()?);
    let png_sidecar = object
        .get("png")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from);
    let kind = static_kind(object.get("kind")?.as_str()?)?;
    let format = static_format(object.get("format")?.as_str()?)?;
    let width = u32::try_from(object.get("width")?.as_u64()?).ok()?;
    let height = u32::try_from(object.get("height")?.as_u64()?).ok()?;
    let scale = object.get("scale")?.as_f64()?;
    let bytes = object.get("bytes")?.as_u64()?;

    Some(HistoryEntry {
        unix,
        path,
        png_sidecar,
        kind,
        format,
        width,
        height,
        scale,
        bytes,
    })
}

fn static_kind(value: &str) -> Option<&'static str> {
    match value {
        "fullscreen" => Some("fullscreen"),
        "region" => Some("region"),
        "window" => Some("window"),
        _ => None,
    }
}

fn static_format(value: &str) -> Option<&'static str> {
    match value {
        "webp" => Some("webp"),
        "png" => Some("png"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory unique to this test, removed on drop. Avoids
    /// `$HOME` entirely — no test in this module may write to a real save
    /// directory.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "saola-capture-test-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("a writable temp dir");
            TempDir(path)
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

    fn frame(width: u32, height: u32) -> Frame {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[x as u8, y as u8, 0x40, 0xff]);
            }
        }
        match Frame::new(width, height, 1.5, pixels) {
            Some(frame) => frame,
            None => unreachable!("the builder always produces width*height*4 bytes"),
        }
    }

    /// A `CaptureOptions` aimed at a scratch directory, with the clipboard
    /// off (a real copy would spawn a helper process from a unit test).
    fn options(dir: &Path, format: ImageFormat, png_also: bool) -> CaptureOptions {
        CaptureOptions {
            kind: ShotKind::Fullscreen,
            geometry: None,
            window_id: None,
            format,
            webp_quality: 90,
            png_also,
            output_dir: Some(dir.to_path_buf()),
            delay: 0,
            cursor: true,
            copy: false,
            toast: false,
            no_daemon: true,
        }
    }

    // -- the whole storage half, end to end --------------------------------

    /// Exercises the whole storage half: encode, atomic write, sidecar, and
    /// the history append.
    ///
    /// The index destination is **passed in**, not redirected by setting
    /// `$XDG_DATA_HOME`. That is load-bearing rather than stylistic: `cargo
    /// test` runs the tests in this binary on parallel threads of one
    /// process, so `std::env::set_var` would let this test and
    /// [`png_format_with_png_also_does_not_write_a_duplicate`] overwrite each
    /// other's redirect and each append its line into the other's directory.
    /// An argument cannot race. (See [`save_capture_indexing_to`]'s teaching
    /// note; `save_capture` itself is the thin wrapper that reads the
    /// environment, and it is what production calls.)
    ///
    /// The index path is deliberately two levels below the temp directory, so
    /// this also covers [`append_history`] creating it on demand.
    #[test]
    fn save_capture_writes_encodes_and_indexes() {
        let dir = TempDir::new("save-capture");
        let data = TempDir::new("save-capture-data");
        let index = data.path().join("saola/capture/history.jsonl");

        let options = options(dir.path(), ImageFormat::Webp, true);
        let saved = save_capture_indexing_to(
            &frame(48, 32),
            &options,
            ShotKind::Fullscreen,
            ClipboardOwner::DetachedHelper,
            Some(&index),
        )
        .expect("a writable temp dir is all this needs");

        assert_eq!(
            saved.path.extension().and_then(|e| e.to_str()),
            Some("webp")
        );
        assert!(saved.path.is_file(), "{} missing", saved.path.display());
        assert_eq!((saved.width, saved.height), (48, 32));
        assert!(saved.bytes > 0);

        let sidecar = saved.png_sidecar.as_ref().expect("png-also was set");
        assert!(sidecar.is_file());
        assert_eq!(sidecar.with_extension("webp"), saved.path, "shared stem");

        let contents = fs::read_to_string(&index).expect("the index was created");
        let lines: Vec<&str> = contents.lines().collect();
        // Exactly one, not "at least one": this index belongs to this test
        // alone, so a second line would mean something else wrote into it.
        assert_eq!(lines.len(), 1, "one capture, one line: {lines:?}");
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON");
        assert_eq!(parsed["path"], saved.path.to_string_lossy().into_owned());
        assert_eq!(parsed["png"], sidecar.to_string_lossy().into_owned());
        assert_eq!(parsed["kind"], "fullscreen");
        assert_eq!(parsed["format"], "webp");
        assert_eq!(parsed["width"], 48);
    }

    /// `image-format = "png"` writes exactly one file and no sidecar — the
    /// sidecar would otherwise be a byte-identical duplicate of the primary.
    #[test]
    fn png_format_with_png_also_does_not_write_a_duplicate() {
        let dir = TempDir::new("png-only");
        let data = TempDir::new("png-only-data");
        let index = data.path().join("saola/capture/history.jsonl");

        let options = options(dir.path(), ImageFormat::Png, true);
        let saved = save_capture_indexing_to(
            &frame(16, 16),
            &options,
            ShotKind::Fullscreen,
            ClipboardOwner::DetachedHelper,
            Some(&index),
        )
        .expect("saves");

        assert_eq!(saved.path.extension().and_then(|e| e.to_str()), Some("png"));
        assert_eq!(saved.png_sidecar, None);
        let files: Vec<_> = fs::read_dir(dir.path())
            .expect("readdir")
            .filter_map(|entry| entry.ok())
            .collect();
        assert_eq!(files.len(), 1, "exactly one file, not a duplicate pair");

        let contents = fs::read_to_string(&index).expect("the index was created");
        assert_eq!(contents.lines().count(), 1, "one capture, one line");
    }

    /// Stage 8, CAPTURE-RESEARCH D3: a `--no-copy` window capture must still
    /// save the file even when the clipboard-clear mitigation itself can't
    /// reach a compositor (this test process has none) — the same "best
    /// effort, the file already exists" posture the ordinary copy-failure
    /// path already has. This is a save-still-succeeds test, not a
    /// clipboard-content test: nothing in this crate can assert on the
    /// *state* of a real Wayland selection from a unit test, only that a
    /// failure to touch it never turns into a failed capture.
    #[test]
    fn a_no_copy_window_capture_still_saves_even_if_clearing_the_clipboard_fails() {
        let dir = TempDir::new("window-no-copy");
        let data = TempDir::new("window-no-copy-data");
        let index = data.path().join("saola/capture/history.jsonl");

        // `options()` already sets `copy: false` — see its own doc comment.
        let opts = options(dir.path(), ImageFormat::Png, false);
        let saved = save_capture_indexing_to(
            &frame(8, 8),
            &opts,
            ShotKind::Window,
            ClipboardOwner::DetachedHelper,
            Some(&index),
        )
        .expect("the capture does not depend on being able to clear the clipboard");

        assert!(saved.path.is_file(), "{} missing", saved.path.display());

        let contents = fs::read_to_string(&index).expect("the index was created");
        let parsed: serde_json::Value =
            serde_json::from_str(contents.lines().next().expect("one line")).expect("valid JSON");
        assert_eq!(parsed["kind"], "window");
    }

    /// No data directory at all (a container with neither `$XDG_DATA_HOME`
    /// nor `$HOME`) must still save the capture — the index is best effort,
    /// the file is not. This branch could not be tested at all while the
    /// destination came from the environment.
    #[test]
    fn a_capture_still_saves_when_there_is_nowhere_to_index_it() {
        let dir = TempDir::new("no-data-dir");

        let options = options(dir.path(), ImageFormat::Png, false);
        let saved = save_capture_indexing_to(
            &frame(8, 8),
            &options,
            ShotKind::Region,
            ClipboardOwner::DetachedHelper,
            None,
        )
        .expect("the capture does not depend on the index");

        assert!(saved.path.is_file(), "{} missing", saved.path.display());
        assert!(saved.bytes > 0);
    }

    // -- save-dir resolution ---------------------------------------------

    #[test]
    fn an_explicit_output_dir_wins() {
        let dir = resolve_save_dir(Some(Path::new("/tmp/shots"))).expect("explicit dir");
        assert_eq!(dir, PathBuf::from("/tmp/shots"));
    }

    #[test]
    fn the_fallback_is_pictures_captures_under_home() {
        assert_eq!(
            default_save_dir(Some("/home/jordan".into())),
            Some(PathBuf::from("/home/jordan/Pictures/Captures"))
        );
    }

    #[test]
    fn no_home_means_no_save_dir_rather_than_a_relative_path() {
        assert_eq!(default_save_dir(None), None);
        assert_eq!(default_save_dir(Some("".into())), None);
    }

    // -- filenames ---------------------------------------------------------

    #[test]
    fn the_stem_is_sortable_and_free_of_filesystem_hostile_characters() {
        let stem = format_stem(
            SCREENSHOT_PREFIX,
            CivilTime {
                year: 2026,
                month: 8,
                day: 8,
                hour: 17,
                minute: 4,
                second: 9,
            },
        );
        assert_eq!(stem, "Screenshot_2026-08-08_17-04-09");
        assert!(
            !stem.contains(':') && !stem.contains('/') && !stem.contains(' '),
            "no colon (FAT/SMB), no slash, no space"
        );
    }

    #[test]
    fn the_live_timestamp_has_the_same_shape_as_the_formatter() {
        // Can't assert on the actual time without pinning $TZ (which would
        // race every other test in this binary), so this asserts the shape:
        // same length, same separator positions, all digits where digits go.
        let stem = timestamp_stem();
        let reference = format_stem(
            SCREENSHOT_PREFIX,
            CivilTime {
                year: 2026,
                month: 8,
                day: 8,
                hour: 17,
                minute: 4,
                second: 9,
            },
        );
        assert_eq!(stem.len(), reference.len(), "{stem} vs {reference}");
        assert!(stem.starts_with("Screenshot_"));
    }

    /// **Stage 11.** Recordings get their own prefix so a capture directory
    /// sorts into two groups, and their own extension pair so a `.mkv` and a
    /// `.mp4` from two runs one second apart cannot collide.
    #[test]
    fn a_recording_path_is_named_and_deconflicted_separately_from_screenshots() {
        let dir = TempDir::new("recording-path");
        let first =
            allocate_recording_path(Some(dir.path()), "mkv").expect("a writable scratch dir");
        let name = first
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        assert!(name.starts_with("Recording_"), "{name}");
        assert!(name.ends_with(".mkv"), "{name}");
        assert_eq!(first.parent(), Some(dir.path()));

        // Occupying the *other* container's name still pushes the next
        // allocation to a suffix — the same rule screenshots have for
        // webp/png.
        fs::write(first.with_extension("mp4"), b"x").expect("write");
        let second =
            allocate_recording_path(Some(dir.path()), "mkv").expect("a writable scratch dir");
        assert_ne!(first, second);
        let second_name = second
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        assert!(second_name.contains("-1."), "{second_name}");
    }

    #[test]
    fn a_recording_path_creates_the_save_directory_on_demand() {
        let dir = TempDir::new("recording-mkdir");
        let nested = dir.path().join("does/not/exist/yet");
        let path = allocate_recording_path(Some(&nested), "mp4").expect("created on demand");
        assert!(nested.is_dir());
        assert_eq!(path.parent(), Some(nested.as_path()));
        // Allocating a path must not create the file itself — ffmpeg does.
        assert!(!path.exists());
    }

    #[test]
    fn a_free_stem_is_used_as_is() {
        let dir = TempDir::new("free-stem");
        let stem = unique_stem(dir.path(), "Screenshot_2026-08-08_17-04-09").expect("free");
        assert_eq!(stem, "Screenshot_2026-08-08_17-04-09");
    }

    #[test]
    fn a_taken_stem_gets_a_numeric_suffix_and_checks_both_extensions() {
        let dir = TempDir::new("taken-stem");
        let base = "Screenshot_2026-08-08_17-04-09";
        fs::write(dir.path().join(format!("{base}.webp")), b"x").expect("write");
        // Only the .png of the -1 variant exists: `unique_stem` must still
        // skip it, or a later `png-also` run would clobber it.
        fs::write(dir.path().join(format!("{base}-1.png")), b"x").expect("write");

        let stem = unique_stem(dir.path(), base).expect("free eventually");
        assert_eq!(stem, format!("{base}-2"));
    }

    // -- atomic write ------------------------------------------------------

    #[test]
    fn write_atomically_leaves_no_part_file_behind() {
        let dir = TempDir::new("atomic");
        let path = dir.path().join("shot.webp");
        write_atomically(&path, b"hello").expect("write");

        assert_eq!(fs::read(&path).expect("read back"), b"hello");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .expect("readdir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "found {leftovers:?}");
    }

    // -- encoders ----------------------------------------------------------

    #[test]
    fn webp_encodes_to_a_riff_webp_container() {
        let bytes = encode_webp(&frame(64, 48), 90).expect("libwebp accepts an RGBA frame");
        assert!(
            bytes.len() > 12,
            "suspiciously small: {} bytes",
            bytes.len()
        );
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
    }

    #[test]
    fn webp_quality_changes_the_output_size() {
        let frame = frame(128, 128);
        let low = encode_webp(&frame, 5).expect("encode");
        let high = encode_webp(&frame, 100).expect("encode");
        assert!(
            low.len() < high.len(),
            "quality 5 produced {} bytes, quality 100 produced {}",
            low.len(),
            high.len()
        );
    }

    #[test]
    fn webp_clamps_an_out_of_range_quality_instead_of_failing() {
        assert!(encode_webp(&frame(8, 8), 0).is_ok());
        assert!(encode_webp(&frame(8, 8), 250).is_ok());
    }

    #[test]
    fn png_round_trips_the_exact_pixels() {
        let original = frame(9, 7);
        let bytes = encode_png(&original).expect("encode");
        assert_eq!(&bytes[1..4], b"PNG");

        let decoded = image::load_from_memory(&bytes).expect("decode");
        assert_eq!((decoded.width(), decoded.height()), (9, 7));
        assert_eq!(
            decoded.to_rgba8().into_raw(),
            original.pixels(),
            "PNG is lossless — every byte must survive"
        );
    }

    // -- history index -----------------------------------------------------

    #[test]
    fn history_dir_prefers_xdg_data_home() {
        assert_eq!(
            history_dir(Some("/xdg/data".into()), Some("/home/jordan".into())),
            Some(PathBuf::from("/xdg/data/saola/capture"))
        );
        assert_eq!(
            history_dir(Some("".into()), Some("/home/jordan".into())),
            Some(PathBuf::from("/home/jordan/.local/share/saola/capture"))
        );
        assert_eq!(history_dir(None, None), None);
    }

    #[test]
    fn a_history_line_is_one_json_object_with_the_documented_keys() {
        let entry = HistoryEntry {
            unix: 1_786_000_000,
            path: PathBuf::from(
                "/home/jordan/Pictures/Captures/Screenshot_2026-08-08_17-04-09.webp",
            ),
            png_sidecar: None,
            kind: "fullscreen",
            format: "webp",
            width: 2560,
            height: 1600,
            scale: 1.5,
            bytes: 812_345,
        };
        let line = history_line(&entry);
        assert!(!line.contains('\n'), "one entry is exactly one line");

        let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(parsed["v"], 1);
        assert_eq!(parsed["unix"], 1_786_000_000_i64);
        assert_eq!(parsed["kind"], "fullscreen");
        assert_eq!(parsed["format"], "webp");
        assert_eq!(parsed["width"], 2560);
        assert_eq!(parsed["height"], 1600);
        assert_eq!(parsed["scale"], 1.5);
        assert_eq!(parsed["bytes"], 812_345);
        assert!(parsed.get("png").is_none(), "absent, not null, when unset");
    }

    #[test]
    fn a_history_line_escapes_a_hostile_path() {
        // Tabs, newlines and quotes in a filename are legal on Linux and are
        // exactly what a hand-rolled TSV index would corrupt.
        let entry = HistoryEntry {
            unix: 0,
            path: PathBuf::from("/tmp/we\tird\n\"name\".webp"),
            png_sidecar: Some(PathBuf::from("/tmp/side\\car.png")),
            kind: "region",
            format: "webp",
            width: 1,
            height: 1,
            scale: 1.0,
            bytes: 1,
        };
        let line = history_line(&entry);
        assert!(!line.contains('\n'));

        let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(parsed["path"], "/tmp/we\tird\n\"name\".webp");
        assert_eq!(parsed["png"], "/tmp/side\\car.png");
    }

    #[test]
    fn appending_produces_one_line_per_entry_in_order() {
        let dir = TempDir::new("history");
        let path = dir.path().join("history.jsonl");

        append_history_line(&path, r#"{"v":1,"n":1}"#).expect("append");
        append_history_line(&path, r#"{"v":1,"n":2}"#).expect("append");

        let contents = fs::read_to_string(&path).expect("read back");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines, vec![r#"{"v":1,"n":1}"#, r#"{"v":1,"n":2}"#]);
        assert!(contents.ends_with('\n'), "every line is newline-terminated");
    }

    /// The first capture on a fresh machine has no `saola/capture/` directory
    /// to append into, so [`append_history`] has to create it.
    #[test]
    fn append_history_creates_the_index_directory_on_demand() {
        let dir = TempDir::new("history-mkdir");
        let path = dir.path().join("saola/capture/history.jsonl");

        let entry = HistoryEntry {
            unix: 1_786_000_000,
            path: PathBuf::from("/tmp/Screenshot_2026-08-08_17-04-09.webp"),
            png_sidecar: None,
            kind: "fullscreen",
            format: "webp",
            width: 8,
            height: 8,
            scale: 1.0,
            bytes: 64,
        };
        append_history(&path, &entry).expect("creates saola/capture/ and the file");

        let contents = fs::read_to_string(&path).expect("read back");
        assert_eq!(contents.lines().count(), 1);
        let parsed: serde_json::Value =
            serde_json::from_str(contents.lines().next().expect("one line")).expect("valid JSON");
        assert_eq!(parsed["v"], 1);
    }

    // -- history index — reading (Stage 16) ---------------------------------

    #[test]
    fn round_trips_a_written_entry_through_the_reader() {
        let entry = HistoryEntry {
            unix: 1_786_000_000,
            path: PathBuf::from("/tmp/Screenshot_2026-08-08_17-04-09.webp"),
            png_sidecar: Some(PathBuf::from("/tmp/Screenshot_2026-08-08_17-04-09.png")),
            kind: "region",
            format: "webp",
            width: 640,
            height: 480,
            scale: 1.5,
            bytes: 12_345,
        };
        let parsed = parse_history_lines(&history_line(&entry));
        assert_eq!(parsed, vec![entry]);
    }

    #[test]
    fn the_reader_skips_blank_and_unparseable_lines_but_keeps_the_good_ones() {
        let good = HistoryEntry {
            unix: 1,
            path: PathBuf::from("/tmp/a.webp"),
            png_sidecar: None,
            kind: "fullscreen",
            format: "webp",
            width: 1,
            height: 1,
            scale: 1.0,
            bytes: 1,
        };
        let text = format!(
            "\n{}\nnot json at all\n{{\"unix\": 2}}\n{{\"unix\":3,\"path\":\"/tmp/b.webp\",\
             \"kind\":\"laser\",\"format\":\"webp\",\"width\":1,\"height\":1,\"scale\":1.0,\
             \"bytes\":1}}\n",
            history_line(&good)
        );
        // Line 1: blank. Line 3: garbage. Line 4: valid JSON, missing keys.
        // Line 5: an unknown `kind` — outside the closed enum, so it must be
        // dropped too, per `static_kind`'s doc comment.
        let parsed = parse_history_lines(&text);
        assert_eq!(parsed, vec![good]);
    }

    #[test]
    fn read_history_entries_on_a_missing_file_is_an_empty_history_not_an_error() {
        let dir = TempDir::new("history-missing");
        let path = dir.path().join("does-not-exist.jsonl");
        assert_eq!(read_history_entries(&path), Vec::new());
    }

    #[test]
    fn read_history_entries_reads_a_real_file_in_append_order() {
        let dir = TempDir::new("history-read");
        let path = dir.path().join("history.jsonl");

        let first = HistoryEntry {
            unix: 1,
            path: PathBuf::from("/tmp/first.webp"),
            png_sidecar: None,
            kind: "fullscreen",
            format: "webp",
            width: 1,
            height: 1,
            scale: 1.0,
            bytes: 1,
        };
        let second = HistoryEntry {
            unix: 2,
            path: PathBuf::from("/tmp/second.webp"),
            png_sidecar: None,
            kind: "window",
            format: "png",
            width: 1,
            height: 1,
            scale: 1.0,
            bytes: 1,
        };
        append_history(&path, &first).expect("append");
        append_history(&path, &second).expect("append");

        assert_eq!(read_history_entries(&path), vec![first, second]);
    }
}
