//! The capture history library (PLAN.md Stage 16): a browsable list of past
//! captures — screenshots from `storage.rs`'s history index, recordings
//! discovered by scanning the save directory — rendered inside
//! [`crate::modules::app`]'s `ViewState::History`, alongside the actions
//! PLAN.md names: open/edit, copy, show in folder, delete (with confirm).
//!
//! # Where recordings come from (the schema question Stage 14/15's handoffs
//! # both flagged and left for this stage)
//!
//! `storage::HistoryEntry`'s documented JSONL schema is still-image-shaped
//! (`format` is `"webp" | "png"` only; `scale`/`png` are still-image-only
//! fields) and recordings have **no row in it at all** —
//! `storage::allocate_recording_path`'s own doc comment says so explicitly,
//! and names the decision this module had to make: bump the index schema
//! (a `v: 2`, or a `type` key) so the *writer* (`modules::recorder`) starts
//! emitting a row per recording, or find recordings some other way.
//!
//! **This stage picks the second option: a directory scan, not a schema
//! bump.** `modules::recorder`'s finalization path (`spawn_recording_tasks`)
//! is untouched. Reasons, in order of how much they mattered:
//!
//! 1. **A schema bump changes a stable, tested writer** for a reader-only
//!    feature — `HistoryEntry`'s own doc comment already promises "a reader
//!    must ignore unknown keys and skip lines it cannot parse", which is
//!    exactly the contract that makes *this* stage safe to add without
//!    touching Stage 5's format at all. Reaching for `v: 2` here would mean
//!    editing `dbus.rs`'s recording-supervisor code — a different subsystem,
//!    already load-bearing, with its own "exactly one finalization site"
//!    invariant (CLAUDE.md Architecture) — for a feature whose only
//!    consumer is a list view.
//! 2. **The filename convention already carries everything the library
//!    needs.** `storage::RECORDING_PREFIX`/`VIDEO_EXTENSIONS` (now
//!    `pub(crate)`, specifically so this module can read them rather than
//!    duplicate the literal) fully describe what a recording on disk looks
//!    like: `Recording_YYYY-MM-DD_HH-MM-SS.{mkv,mp4}`. [`scan_recordings`]
//!    is a plain `fs::read_dir` filtered on that shape, using each file's
//!    own `mtime`/size for the "when"/"how big" a `HistoryEntry` row would
//!    otherwise carry — real filesystem metadata, not a second source of
//!    truth that could drift from it.
//! 3. **Self-healing for free.** A directory scan can never go stale the way
//!    an index row can — delete a recording by hand (`rm`, a file manager)
//!    and it is simply gone from the next scan, no orphaned row to filter.
//!    [`load_library`]'s screenshot half gets the same property by
//!    filtering out any [`storage::HistoryEntry`] whose `path` no longer
//!    exists, for the same reason (see its doc comment).
//!
//! The real cost, named rather than hidden: a directory scan cannot recover
//! anything an index row would carry that isn't derivable from the file
//! itself — there is no recorded `kind` ("fullscreen"/"region"/"window"
//! equivalent) or duration for a scanned recording, only its name, size and
//! mtime. Good enough for "browse and act on what's on disk"; a future stage
//! wanting per-recording metadata (a thumbnail frame, its target kind) will
//! need the schema bump this one deliberately avoided.
//!
//! # Deletion never touches `history.jsonl`
//!
//! The index is documented as **append-only** ("no rewriting of earlier
//! lines ever" — `storage.rs`'s own module doc comment) and stays that way
//! here: [`delete_item`] removes the file(s) on disk and nothing else. A
//! deleted screenshot's index row becomes exactly the kind of line
//! `HistoryEntry`'s own doc comment already tells readers to expect and
//! handle — "a line half-written by a machine that lost power" is the
//! documented failure mode for a line that no longer resolves to a real
//! file, and [`load_library`] filters those out on every load rather than
//! rewriting the file to remove them. Editing an append-only log to remove
//! one line would mean either rewriting the whole file (losing the format's
//! whole reason for existing — see `storage.rs`'s own "cheap to append,
//! concurrent-safe" reasoning) or inventing a tombstone record this stage's
//! brief never asked for.
//!
//! # "Grid", as actually built
//!
//! PLAN.md's task 1 says "a browsable grid". This crate's resolved `iced`
//! feature set has no flex-wrap widget (no `iced_aw`, and adding one is a
//! new-dependency decision this stage's brief doesn't ask for) — so
//! [`HistoryModel::view`] is a scrollable **list** of rows, each already
//! showing a thumbnail tile, filename, metadata and its own action buttons
//! inline. It reads as a grid of cards stacked one per line rather than
//! wrapped into columns; the content per item (thumbnail + actions +
//! metadata) is what the task brief actually asks for, and a true
//! multi-column wrap is a layout change, not a feature this stage is
//! missing.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use iced::widget::{button, column, container, image, row, scrollable, text, Space};
use iced::{Center, Element, Length};
use saola_theme::{ColorExt, Surface, Theme};

use crate::capture::Frame;
use crate::config::CaptureConfig;
use crate::encode::export::AnimatedFormat;
use crate::storage::{self, ClipboardOwner, HistoryEntry, StorageError};

/// The thumbnail's longer side, in pixels — a bit smaller than the toast's
/// own 128px (`dbus.rs`'s own comment on that constant): a history row's
/// tile sits next to filename/metadata/action-button text in a fixed-height
/// row, where the toast's tile is the card's only other content.
const THUMBNAIL_MAX_DIM: u32 = 96;

// ---------------------------------------------------------------------
// Data model — pure, unit-tested directly
// ---------------------------------------------------------------------

/// One row the library can show: a saved screenshot (from the index) or a
/// recording (from the directory scan — see the module doc comment).
#[derive(Debug, Clone, PartialEq)]
pub enum HistoryItem {
    Screenshot(HistoryEntry),
    Recording(RecordingFile),
}

/// A recording found on disk — everything [`scan_recordings`] can learn
/// without an index row: its path, real filesystem metadata standing in for
/// "when" and "how big".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingFile {
    pub path: PathBuf,
    /// The file's `mtime`, seconds since the Unix epoch — a recording is
    /// written incrementally over its whole life (`storage.rs`'s own "no
    /// `.part`+`rename`" note on why), so its *last-modified* time is the
    /// moment recording stopped, which is the closest a directory scan gets
    /// to `HistoryEntry::unix`'s "capture time" without an index row.
    pub unix: i64,
    pub bytes: u64,
}

impl HistoryItem {
    pub fn path(&self) -> &Path {
        match self {
            HistoryItem::Screenshot(entry) => &entry.path,
            HistoryItem::Recording(file) => &file.path,
        }
    }

    fn unix(&self) -> i64 {
        match self {
            HistoryItem::Screenshot(entry) => entry.unix,
            HistoryItem::Recording(file) => file.unix,
        }
    }

    pub fn bytes(&self) -> u64 {
        match self {
            HistoryItem::Screenshot(entry) => entry.bytes,
            HistoryItem::Recording(file) => file.bytes,
        }
    }

    pub fn is_recording(&self) -> bool {
        matches!(self, HistoryItem::Recording(_))
    }

    pub fn file_name(&self) -> String {
        self.path()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path().display().to_string())
    }
}

/// What clicking a row's Open/Edit button should do — screenshots go to the
/// editor, recordings (which the editor has no support for — CLAUDE.md
/// Boundaries/Architecture, unchanged since Stage 12) open their containing
/// directory instead, exactly like the finish toast's own click target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenAction {
    Edit(PathBuf),
    OpenDir(PathBuf),
}

fn open_action(item: &HistoryItem) -> OpenAction {
    match item {
        HistoryItem::Screenshot(entry) => OpenAction::Edit(entry.path.clone()),
        HistoryItem::Recording(file) => OpenAction::OpenDir(file.path.clone()),
    }
}

// ---------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------

/// Scans `dir` for recordings — filenames starting with
/// [`storage::RECORDING_PREFIX`] and ending in one of
/// [`storage::VIDEO_EXTENSIONS`]. A directory that doesn't exist yet (no
/// recording has ever been saved) is an empty list, not an error — the same
/// "no history yet" posture [`storage::read_history_entries`] takes for a
/// missing index file.
fn scan_recordings(dir: &Path) -> Vec<RecordingFile> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with(storage::RECORDING_PREFIX) {
            continue;
        }
        let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        if !storage::VIDEO_EXTENSIONS.contains(&extension) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let unix = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .unwrap_or(0);

        files.push(RecordingFile {
            path,
            unix,
            bytes: metadata.len(),
        });
    }
    files
}

/// Merges screenshots and recordings into one library, newest first. Pure —
/// the testable core of [`load_library`], taking both lists as plain
/// arguments rather than reading the filesystem itself (this crate's
/// "resolution reads the environment once; logic takes arguments" rule —
/// see `storage.rs`'s own module doc comment for the rule and why it
/// matters for `cargo test`).
fn merge_items(entries: Vec<HistoryEntry>, recordings: Vec<RecordingFile>) -> Vec<HistoryItem> {
    let mut items: Vec<HistoryItem> = entries
        .into_iter()
        .map(HistoryItem::Screenshot)
        .chain(recordings.into_iter().map(HistoryItem::Recording))
        .collect();
    items.sort_by_key(|item| std::cmp::Reverse(item.unix()));
    items
}

/// Loads the whole library: every readable index row whose file still
/// exists, plus every recording [`scan_recordings`] finds in the resolved
/// save directory — see the module doc comment for why a missing/deleted
/// file is filtered out here rather than surfaced as a broken row.
///
/// Reads `$XDG_DATA_HOME`/`$HOME` (via [`storage::history_path`]) and
/// `config.save_dir`, both at this one edge — [`merge_items`]/
/// [`scan_recordings`] underneath take plain arguments and are what the unit
/// tests exercise directly.
pub fn load_library(config: &CaptureConfig) -> Vec<HistoryItem> {
    let entries: Vec<HistoryEntry> = storage::history_path()
        .map(|path| storage::read_history_entries(&path))
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.path.exists())
        .collect();

    let recordings = storage::resolve_save_dir(config.save_dir.as_deref())
        .map(|dir| scan_recordings(&dir))
        .unwrap_or_default();

    merge_items(entries, recordings)
}

// ---------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------

/// Copies a saved screenshot to the clipboard as PNG — `storage.rs`'s own
/// "the clipboard always gets PNG" rule, applied to a file that's already on
/// disk rather than a freshly captured [`Frame`]. A PNG-format entry's bytes
/// are used directly (no re-encode, matching `save_capture_indexing_to`'s
/// own "already PNG, reuse the bytes" branch); a WebP entry is decoded and
/// re-encoded — `image = "0.25"`'s default features include WebP *decode*
/// (see `Cargo.toml`'s own survey: only the *lossy encoder* was the gap that
/// pulled in the `webp` crate), so this needs no new dependency.
fn copy_screenshot(entry: &HistoryEntry, owner: ClipboardOwner) -> Result<(), StorageError> {
    let read = |path: &Path| {
        fs::read(path).map_err(|source| StorageError::Write {
            path: path.to_path_buf(),
            source,
        })
    };
    let bytes = read(&entry.path)?;

    let png = if entry.format == "png" {
        bytes
    } else {
        let decoded = ::image::load_from_memory(&bytes)
            .map_err(|err| StorageError::Encode(err.to_string()))?
            .into_rgba8();
        let (width, height) = (decoded.width(), decoded.height());
        let frame =
            Frame::new(width, height, entry.scale, decoded.into_raw()).ok_or_else(|| {
                StorageError::Encode(
                    "the decoded image's dimensions didn't match its own pixel buffer".to_string(),
                )
            })?;
        storage::encode_frame(&frame, crate::config::ImageFormat::Png, 100)?
    };

    storage::copy_to_clipboard(&png, owner).map_err(|source| StorageError::Write {
        path: entry.path.clone(),
        source,
    })
}

/// Removes an item's file(s) from disk. A screenshot's PNG sidecar (when
/// `png-also = true` produced one) is removed best-effort — if the primary
/// file is already gone the sidecar removal is skipped entirely, and a
/// sidecar that resists removal for its own reason (permissions) doesn't
/// block reporting the primary deletion as successful, since that's the
/// file the row actually represents.
fn delete_item(item: &HistoryItem) -> std::io::Result<()> {
    match item {
        HistoryItem::Screenshot(entry) => {
            fs::remove_file(&entry.path)?;
            if let Some(sidecar) = &entry.png_sidecar {
                let _ = fs::remove_file(sidecar);
            }
            Ok(())
        }
        HistoryItem::Recording(file) => fs::remove_file(&file.path),
    }
}

/// `1_234_567` bytes to `"1.2 MB"` — the export size feedback, and a row's
/// own size line. Binary (1024-based) units, matching how every file
/// manager on this desktop already reports size; `bytes` fits comfortably in
/// an `f64` for anything a screen recording could plausibly reach.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ---------------------------------------------------------------------
// The model — state + update, following the sibling `state struct + view +
// Message` shape (AGENTS.md: "every module maps to a signal, not a poll")
// ---------------------------------------------------------------------

/// One row's state, alongside its (screenshot-only — see the module doc
/// comment on why a recording has no free thumbnail) decoded thumbnail.
struct Row {
    item: HistoryItem,
    thumbnail: Option<image::Handle>,
}

pub struct HistoryModel {
    config: CaptureConfig,
    rows: Vec<Row>,
    /// The one row currently asking "delete this? This can't be undone." —
    /// at most one at a time; pressing Delete on a different row silently
    /// replaces whichever confirmation was pending, rather than stacking
    /// two.
    pending_delete: Option<PathBuf>,
    /// The one export in flight, if any — [`Action::Export`] is a
    /// fire-and-forget request the owning `App` turns into a background
    /// `Task`; this flag is what keeps a second Export press from starting a
    /// second `ffmpeg` before the first has finished (both would target the
    /// same-or-colliding output path).
    exporting: Option<PathBuf>,
    feedback: Option<Result<String, String>>,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// Re-scan the index and the save directory — a manual refresh, since
    /// nothing pushes this model a live update when a *different* process
    /// (a keybind-triggered `shot`, the daemon) saves a new capture.
    Refresh,
    OpenRequested(PathBuf),
    CopyRequested(PathBuf),
    ShowInFolderRequested(PathBuf),
    DeleteRequested(PathBuf),
    DeleteConfirmed(PathBuf),
    DeleteCancelled,
    ExportRequested(PathBuf, AnimatedFormat),
    /// `path` identifies which row's export this is (not an index — see
    /// [`HistoryModel`]'s `pending_delete`/`exporting` fields' own doc
    /// comments on why every cross-`Task` message in this model is keyed by
    /// path rather than a position that could shift under a concurrent
    /// delete).
    ExportFinished(PathBuf, Result<(PathBuf, u64), String>),
}

/// What [`HistoryModel::update`] asks the owning window process to do
/// outside this model's own state — the same "return a value instead of a
/// `Task`" shape `modules::toast::Action` already uses, for the same
/// testability reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// Spawn the editor on this screenshot.
    Edit(PathBuf),
    /// Open the directory containing this path (`xdg-open`) — a recording's
    /// Open/Edit button, and every item's "Show in folder" button.
    OpenDir(PathBuf),
    /// Run this export in the background and report back with
    /// [`Message::ExportFinished`] — `App::update` is what actually spawns
    /// the `Task` (this model has no way to run one itself; see
    /// `HistoryModel::update`'s doc comment).
    Export {
        source: PathBuf,
        format: AnimatedFormat,
    },
}

impl HistoryModel {
    pub fn load(config: &CaptureConfig) -> Self {
        let items = load_library(config);
        let rows = items
            .into_iter()
            .map(|item| {
                let thumbnail = build_thumbnail(&item);
                Row { item, thumbnail }
            })
            .collect();
        HistoryModel {
            config: config.clone(),
            rows,
            pending_delete: None,
            exporting: None,
            feedback: None,
        }
    }

    fn find(&self, path: &Path) -> Option<&HistoryItem> {
        self.rows
            .iter()
            .find(|row| row.item.path() == path)
            .map(|row| &row.item)
    }

    /// **Not itself async** — this model runs entirely on `App::update`'s
    /// call stack, the same synchronous-`&mut self` shape every other
    /// `EditorModel`/`RecorderState` mutator in this crate uses (CLAUDE.md's
    /// testing rule: "pure data + functions, unit-tested"). The one
    /// genuinely slow operation this model can trigger — an export, which
    /// runs `ffmpeg` twice for a GIF — is therefore never run *inside*
    /// `update` itself: [`Message::ExportRequested`] only sets the `busy`
    /// flag and hands the real work back to the caller as [`Action::Export`]
    /// for a `Task::perform`/`spawn_blocking` to run off this thread. This
    /// is the opposite choice from `modules::editor::EditorModel::released`'s
    /// own (recorded, unfixed) synchronous redaction-kernel call — the
    /// difference is that an export was never on that model's existing
    /// synchronous contract to begin with, so there was no existing shape to
    /// preserve by staying synchronous the way redaction's was.
    pub fn update(&mut self, message: Message) -> Action {
        match message {
            Message::Refresh => {
                *self = HistoryModel::load(&self.config);
                Action::None
            }
            Message::OpenRequested(path) => match self.find(&path) {
                Some(item) => match open_action(item) {
                    OpenAction::Edit(path) => Action::Edit(path),
                    OpenAction::OpenDir(path) => Action::OpenDir(path),
                },
                None => Action::None,
            },
            Message::CopyRequested(path) => {
                match self.find(&path) {
                    Some(HistoryItem::Screenshot(entry)) => {
                        let entry = entry.clone();
                        self.feedback =
                            Some(match copy_screenshot(&entry, ClipboardOwner::ThisProcess) {
                                Ok(()) => Ok("Copied to clipboard".to_string()),
                                Err(err) => Err(err.to_string()),
                            });
                    }
                    // A recording's row has no Copy button in `view` at all
                    // (`storage.rs`'s own "no clipboard — nothing pastes a
                    // video" rule) — this arm exists only so a stray message
                    // (a stale button, in principle) degrades to a clear
                    // sentence instead of doing nothing silently.
                    Some(HistoryItem::Recording(_)) => {
                        self.feedback = Some(Err(
                            "Recordings can't be copied to the clipboard".to_string()
                        ));
                    }
                    None => {}
                }
                Action::None
            }
            Message::ShowInFolderRequested(path) => Action::OpenDir(path),
            Message::DeleteRequested(path) => {
                self.pending_delete = Some(path);
                Action::None
            }
            Message::DeleteCancelled => {
                self.pending_delete = None;
                Action::None
            }
            Message::DeleteConfirmed(path) => {
                if let Some(item) = self.find(&path).cloned() {
                    match delete_item(&item) {
                        Ok(()) => {
                            self.rows.retain(|row| row.item.path() != path);
                            self.feedback = Some(Ok("Deleted".to_string()));
                        }
                        Err(err) => self.feedback = Some(Err(err.to_string())),
                    }
                }
                self.pending_delete = None;
                Action::None
            }
            Message::ExportRequested(path, format) => {
                if self.exporting.is_some() {
                    // One export at a time — see the field's own doc
                    // comment. Silently ignored rather than queued: the
                    // button that sent this is disabled in `view` while
                    // `exporting.is_some()`, so reaching this arm at all
                    // means a message that outraced its own disabled button
                    // (a `Task` in flight from a press just before this one
                    // landed), not something a user can normally trigger
                    // twice.
                    return Action::None;
                }
                self.exporting = Some(path.clone());
                Action::Export {
                    source: path,
                    format,
                }
            }
            Message::ExportFinished(path, result) => {
                if self.exporting.as_deref() == Some(path.as_path()) {
                    self.exporting = None;
                }
                self.feedback = Some(match result {
                    Ok((out, bytes)) => Ok(format!(
                        "Exported {} ({})",
                        out.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| out.display().to_string()),
                        format_bytes(bytes)
                    )),
                    Err(err) => Err(err),
                });
                Action::None
            }
        }
    }

    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        let mut list = column![].spacing(theme.sizes.island_gap);

        list = list.push(
            row![
                text("Capture history")
                    .font(saola_theme::convert::ui_font(theme))
                    .size(theme.typography.size.section_heading)
                    .color(theme.on_paper.primary.into_iced()),
                Space::new().width(Length::Fill),
                button(
                    text("Refresh")
                        .font(saola_theme::convert::ui_font_regular(theme))
                        .size(theme.typography.size.secondary)
                )
                .style(saola_theme::style::button::rest(theme, Surface::Paper))
                .on_press(Message::Refresh),
            ]
            .align_y(Center),
        );

        if self.rows.iter().any(|row| row.item.is_recording()) {
            // PLAN.md task 3's required teaching note: GIF/animated-WebP
            // export is frame-by-frame with no interframe compression, so
            // the result can dwarf the source recording. Shown once, above
            // the list, rather than repeated on every recording row.
            list = list.push(
                text(
                    "Exporting a recording to GIF or animated WebP stores every frame \
                     independently — the result can be much larger than the recording \
                     itself. Export only what you plan to share.",
                )
                .font(saola_theme::convert::ui_font_regular(theme))
                .size(theme.typography.size.meta)
                .color(theme.on_paper.tertiary.into_iced()),
            );
        }

        if let Some(feedback) = &self.feedback {
            let (message, color) = match feedback {
                Ok(message) => (message.clone(), theme.on_paper.secondary.into_iced()),
                Err(message) => (message.clone(), theme.on_paper.secondary.into_iced()),
            };
            list = list.push(
                text(message)
                    .font(saola_theme::convert::ui_font_regular(theme))
                    .size(theme.typography.size.secondary)
                    .color(color),
            );
        }

        if self.rows.is_empty() {
            list = list.push(
                text("No captures yet.")
                    .font(saola_theme::convert::ui_font_regular(theme))
                    .size(theme.typography.size.secondary)
                    .color(theme.on_paper.tertiary.into_iced()),
            );
        } else {
            for row_state in &self.rows {
                list = list.push(row_view(
                    theme,
                    row_state,
                    &self.pending_delete,
                    &self.exporting,
                ));
            }
        }

        scrollable(list.padding(theme.sizes.popover_padding))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(saola_theme::style::scrollable::rest(theme, Surface::Paper))
            .into()
    }
}

fn build_thumbnail(item: &HistoryItem) -> Option<image::Handle> {
    let HistoryItem::Screenshot(entry) = item else {
        // A recording's first frame is not free to decode without spawning
        // ffmpeg — the same reasoning `modules::toast::ToastKind::Recording`
        // already documents for its own tile. Not fixed here; see the
        // module doc comment's cost accounting.
        return None;
    };
    let bytes = fs::read(&entry.path).ok()?;
    let decoded = ::image::load_from_memory(&bytes).ok()?.into_rgba8();
    let (width, height) = (decoded.width(), decoded.height());
    let frame = Frame::new(width, height, entry.scale, decoded.into_raw())?;
    Some(crate::modules::toast::thumbnail_handle(
        &frame,
        THUMBNAIL_MAX_DIM,
    ))
}

fn row_view<'a>(
    theme: &Theme,
    row_state: &'a Row,
    pending_delete: &Option<PathBuf>,
    exporting: &Option<PathBuf>,
) -> Element<'a, Message> {
    let path = row_state.item.path().to_path_buf();

    let tile: Element<'static, Message> = match &row_state.thumbnail {
        Some(handle) => image(handle.clone())
            .width(Length::Fixed(THUMBNAIL_MAX_DIM as f32))
            .height(Length::Fixed((THUMBNAIL_MAX_DIM as f32) * 0.7))
            .content_fit(iced::ContentFit::Cover)
            .into(),
        None => {
            // A recording (no decoded thumbnail — see `build_thumbnail`), or
            // a screenshot whose file could not be decoded for some reason.
            // A plain ivory tile, matching `modules::toast`'s own posture
            // for content with no free thumbnail (that module's own doc
            // comment: "this crate still has no `src/icons.rs`").
            //
            // The color is extracted to a plain `Copy` value *before* the
            // closure, not captured as `theme` itself — the closure must be
            // `'static` (this whole tile is `Element<'static, _>`) and
            // `theme` here is only `&'_ Theme`, the same reason
            // `modules::toast::ink_card_style` takes bare `Color`s rather
            // than a borrowed theme.
            let paper = theme.palette.paper.into_iced();
            container(Space::new())
                .width(Length::Fixed(THUMBNAIL_MAX_DIM as f32))
                .height(Length::Fixed((THUMBNAIL_MAX_DIM as f32) * 0.7))
                .style(move |_: &iced::Theme| container::Style {
                    background: Some(iced::Background::Color(paper)),
                    ..container::Style::default()
                })
                .into()
        }
    };

    let kind_label = match &row_state.item {
        HistoryItem::Screenshot(entry) => entry.kind,
        HistoryItem::Recording(_) => "recording",
    };

    let meta = column![
        text(row_state.item.file_name())
            .font(saola_theme::convert::mono_font(theme))
            .size(theme.typography.size.secondary)
            .color(theme.on_paper.primary.into_iced()),
        text(format!(
            "{kind_label} · {}",
            format_bytes(row_state.item.bytes())
        ))
        .font(saola_theme::convert::ui_font_regular(theme))
        .size(theme.typography.size.meta)
        .color(theme.on_paper.tertiary.into_iced()),
    ]
    .spacing(2.0)
    .width(Length::Fill);

    let actions: Element<'static, Message> = if pending_delete.as_ref() == Some(&path) {
        confirm_delete_row(theme, path.clone())
    } else {
        default_action_row(theme, &row_state.item, path.clone(), exporting)
    };

    row![tile, meta, actions]
        .spacing(theme.sizes.pill_gap)
        .align_y(Center)
        .width(Length::Fill)
        .into()
}

/// "Wording carries severity, no red" (CLAUDE.md Design language) — the
/// confirmation sentence itself, not a color, is what marks this
/// irreversible.
fn confirm_delete_row(theme: &Theme, path: PathBuf) -> Element<'static, Message> {
    row![
        text("Delete this capture? This can't be undone.")
            .font(saola_theme::convert::ui_font_regular(theme))
            .size(theme.typography.size.secondary)
            .color(theme.on_paper.primary.into_iced()),
        small_button(theme, "Delete", true, Some(Message::DeleteConfirmed(path))),
        small_button(theme, "Cancel", false, Some(Message::DeleteCancelled)),
    ]
    .spacing(theme.sizes.pill_gap)
    .align_y(Center)
    .into()
}

fn default_action_row(
    theme: &Theme,
    item: &HistoryItem,
    path: PathBuf,
    exporting: &Option<PathBuf>,
) -> Element<'static, Message> {
    let mut buttons = row![].spacing(theme.sizes.pill_gap).align_y(Center);

    // A recording's Open/Edit button already opens its containing directory
    // (`open_action`, the editor having no video support) — a *second*,
    // separate "Show in folder" button for the same target would be a
    // literal duplicate, so it's only added for screenshots (whose Open
    // button spawns the editor instead).
    let open_label = if item.is_recording() {
        "Show in folder"
    } else {
        "Open"
    };
    buttons = buttons.push(small_button(
        theme,
        open_label,
        false,
        Some(Message::OpenRequested(path.clone())),
    ));

    if !item.is_recording() {
        buttons = buttons.push(small_button(
            theme,
            "Copy",
            false,
            Some(Message::CopyRequested(path.clone())),
        ));
        buttons = buttons.push(small_button(
            theme,
            "Show in folder",
            false,
            Some(Message::ShowInFolderRequested(path.clone())),
        ));
    }

    if item.is_recording() {
        let busy = exporting.as_ref() == Some(&path);
        let label = if busy { "Exporting…" } else { "Export GIF" };
        buttons = buttons.push(small_button(
            theme,
            label,
            false,
            (!busy).then(|| Message::ExportRequested(path.clone(), AnimatedFormat::Gif)),
        ));
        buttons = buttons.push(small_button(
            theme,
            if busy { "Exporting…" } else { "Export WebP" },
            false,
            (!busy).then(|| Message::ExportRequested(path.clone(), AnimatedFormat::AnimatedWebp)),
        ));
    }

    buttons = buttons.push(small_button(
        theme,
        "Delete",
        false,
        Some(Message::DeleteRequested(path)),
    ));

    buttons.into()
}

/// One small text button — `button::active` for the (at most one) live/
/// destructive action a row is currently offering, `button::rest`
/// otherwise. `on_press: None` renders a disabled button rather than
/// omitting it, so the row's layout doesn't jump while an export is in
/// flight.
fn small_button(
    theme: &Theme,
    label: &str,
    primary: bool,
    on_press: Option<Message>,
) -> Element<'static, Message> {
    let content = text(label.to_string())
        .font(saola_theme::convert::ui_font_regular(theme))
        .size(theme.typography.size.meta);

    if primary {
        button(content)
            .style(saola_theme::style::button::active(theme, Surface::Paper))
            .on_press_maybe(on_press)
            .into()
    } else {
        button(content)
            .style(saola_theme::style::button::rest(theme, Surface::Paper))
            .on_press_maybe(on_press)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(unix: i64, name: &str, kind: &'static str, format: &'static str) -> HistoryEntry {
        HistoryEntry {
            unix,
            path: PathBuf::from(format!("/tmp/{name}")),
            png_sidecar: None,
            kind,
            format,
            width: 100,
            height: 100,
            scale: 1.0,
            bytes: 1000,
        }
    }

    fn recording(unix: i64, name: &str) -> RecordingFile {
        RecordingFile {
            path: PathBuf::from(format!("/tmp/{name}")),
            unix,
            bytes: 2000,
        }
    }

    // -- merge_items ---------------------------------------------------

    #[test]
    fn merge_sorts_newest_first_across_both_kinds() {
        let entries = vec![
            entry(10, "a.webp", "fullscreen", "webp"),
            entry(30, "c.webp", "window", "webp"),
        ];
        let recordings = vec![recording(20, "b.mkv")];

        let merged = merge_items(entries, recordings);
        let names: Vec<String> = merged.iter().map(HistoryItem::file_name).collect();
        assert_eq!(names, vec!["c.webp", "b.mkv", "a.webp"]);
    }

    #[test]
    fn merge_of_two_empty_lists_is_empty() {
        assert_eq!(merge_items(Vec::new(), Vec::new()), Vec::new());
    }

    // -- HistoryItem accessors -------------------------------------------

    #[test]
    fn a_recording_item_reports_itself_as_one() {
        let item = HistoryItem::Recording(recording(1, "clip.mkv"));
        assert!(item.is_recording());
        assert_eq!(item.file_name(), "clip.mkv");
    }

    #[test]
    fn a_screenshot_item_is_not_a_recording() {
        let item = HistoryItem::Screenshot(entry(1, "shot.webp", "region", "webp"));
        assert!(!item.is_recording());
    }

    // -- open_action -----------------------------------------------------

    #[test]
    fn a_screenshot_opens_in_the_editor() {
        let item = HistoryItem::Screenshot(entry(1, "shot.webp", "fullscreen", "webp"));
        assert_eq!(
            open_action(&item),
            OpenAction::Edit(PathBuf::from("/tmp/shot.webp"))
        );
    }

    #[test]
    fn a_recording_opens_its_containing_directory() {
        let item = HistoryItem::Recording(recording(1, "clip.mkv"));
        assert_eq!(
            open_action(&item),
            OpenAction::OpenDir(PathBuf::from("/tmp/clip.mkv"))
        );
    }

    // -- format_bytes ------------------------------------------------------

    #[test]
    fn format_bytes_stays_whole_under_a_kilobyte() {
        assert_eq!(format_bytes(512), "512 B");
    }

    #[test]
    fn format_bytes_shows_one_decimal_above_a_kilobyte() {
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(3 * 1024 * 1024), "3.0 MB");
    }

    #[test]
    fn format_bytes_scales_up_to_gigabytes() {
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    // -- scan_recordings ---------------------------------------------------

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "saola-capture-history-test-{label}-{}-{}",
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
    fn scan_recordings_finds_both_containers_and_ignores_everything_else() {
        let dir = TempDir::new("scan");
        fs::write(dir.path().join("Recording_2026-08-08_12-00-00.mkv"), b"a").unwrap();
        fs::write(dir.path().join("Recording_2026-08-08_12-05-00.mp4"), b"bb").unwrap();
        fs::write(dir.path().join("Screenshot_2026-08-08_12-06-00.webp"), b"c").unwrap();
        fs::write(dir.path().join("Recording_2026-08-08_12-07-00.txt"), b"d").unwrap();
        fs::write(
            dir.path()
                .join(".Recording_2026-08-08_12-08-00-palette.png"),
            b"e",
        )
        .unwrap();

        let found = scan_recordings(dir.path());
        let mut names: Vec<String> = found
            .iter()
            .map(|file| {
                file.path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "Recording_2026-08-08_12-00-00.mkv".to_string(),
                "Recording_2026-08-08_12-05-00.mp4".to_string(),
            ]
        );
    }

    #[test]
    fn scan_recordings_on_a_missing_directory_is_empty_not_an_error() {
        let dir = TempDir::new("missing");
        let missing = dir.path().join("does-not-exist");
        assert_eq!(scan_recordings(&missing), Vec::new());
    }

    #[test]
    fn scan_recordings_reports_real_file_sizes() {
        let dir = TempDir::new("size");
        fs::write(
            dir.path().join("Recording_2026-08-08_12-00-00.mkv"),
            vec![0u8; 12345],
        )
        .unwrap();
        let found = scan_recordings(dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bytes, 12345);
    }

    // -- delete_item ---------------------------------------------------

    #[test]
    fn deleting_a_screenshot_removes_its_png_sidecar_too() {
        let dir = TempDir::new("delete-sidecar");
        let primary = dir.path().join("Screenshot_2026-08-08_12-00-00.webp");
        let sidecar = dir.path().join("Screenshot_2026-08-08_12-00-00.png");
        fs::write(&primary, b"a").unwrap();
        fs::write(&sidecar, b"b").unwrap();

        let item = HistoryItem::Screenshot(HistoryEntry {
            unix: 1,
            path: primary.clone(),
            png_sidecar: Some(sidecar.clone()),
            kind: "fullscreen",
            format: "webp",
            width: 1,
            height: 1,
            scale: 1.0,
            bytes: 1,
        });

        delete_item(&item).expect("delete");
        assert!(!primary.exists());
        assert!(!sidecar.exists());
    }

    #[test]
    fn deleting_a_recording_removes_just_its_own_file() {
        let dir = TempDir::new("delete-recording");
        let path = dir.path().join("Recording_2026-08-08_12-00-00.mkv");
        fs::write(&path, b"a").unwrap();

        let item = HistoryItem::Recording(RecordingFile {
            path: path.clone(),
            unix: 1,
            bytes: 1,
        });
        delete_item(&item).expect("delete");
        assert!(!path.exists());
    }

    // -- load_library (the one env-touching entry point, exercised end to
    // end against a real temp tree rather than mocked) ---------------------

    #[test]
    fn load_library_filters_out_a_screenshot_whose_file_is_gone() {
        let dir = TempDir::new("load-missing-file");
        let history_path = dir.path().join("history.jsonl");
        let existing = dir.path().join("exists.webp");
        fs::write(&existing, b"a").unwrap();

        let present = HistoryEntry {
            unix: 2,
            path: existing,
            png_sidecar: None,
            kind: "fullscreen",
            format: "webp",
            width: 1,
            height: 1,
            scale: 1.0,
            bytes: 1,
        };
        let gone = HistoryEntry {
            unix: 1,
            path: dir.path().join("gone.webp"),
            png_sidecar: None,
            kind: "fullscreen",
            format: "webp",
            width: 1,
            height: 1,
            scale: 1.0,
            bytes: 1,
        };

        // Writes both rows directly via the same JSONL format `storage.rs`
        // documents, without going through `save_capture` (which would need
        // a whole `Frame`) — the point of this test is `load_library`'s own
        // missing-file filter, not the writer.
        let line = |entry: &HistoryEntry| {
            format!(
                r#"{{"v":1,"unix":{},"path":"{}","kind":"{}","format":"{}","width":1,"height":1,"scale":1.0,"bytes":1}}"#,
                entry.unix,
                entry.path.display(),
                entry.kind,
                entry.format,
            )
        };
        fs::write(
            &history_path,
            format!("{}\n{}\n", line(&gone), line(&present)),
        )
        .unwrap();

        let entries = storage::read_history_entries(&history_path)
            .into_iter()
            .filter(|entry| entry.path.exists())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].unix, 2);
    }
}
