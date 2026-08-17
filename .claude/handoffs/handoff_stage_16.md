# Stage 16 handoff — history library, real PickColor, GIF/WebP export

Forward-facing context for **Stage 17** (CI, packaging, autostart, README)
and for whoever eventually live-tests PickColor's interactive half or the
History/export GUI paths this stage could not safely drive by hand.

Touched: `src/dbus.rs`, `src/main.rs`, `src/modules/app.rs`,
`src/modules/mod.rs`, `src/modules/toast.rs`, `src/storage.rs`,
`src/encode/mod.rs`, `AGENTS.md`. New: `src/modules/history.rs`,
`src/modules/picker.rs`, `src/encode/export.rs`. **Nothing committed**, as
every prior stage.

Gates at hand-off: `cargo build`, `cargo clippy --all-targets -- -D
warnings`, `cargo fmt --check` all clean. `cargo test`: **478 passed**
(Stage 15: 445 → +33 real new tests), 1 ignored (Stage 15's own manual perf
test, unaffected by this stage).

---

## 0. What is and isn't wired

**Is:** `io.saola.Capture1`'s `PickColor` is real end to end — the CLI verb,
the daemon's own D-Bus method, the swatch toast, and the clipboard copy. The
app window's Main tab has a new "More" row with **History** and **Pick
Color** buttons; History is real in-app navigation (`ViewState::History`) to
a scrollable list of every saved screenshot and recording, with Open/Edit,
Copy (screenshots only), Show in folder, Delete (two-step confirm), and
GIF/animated-WebP export (recordings only). No stub methods remain anywhere
on `io.saola.Capture1`.

**Is not:** a true multi-column "grid" (iced 0.14 here has no flex-wrap
widget — see `modules::history`'s own doc comment); a history-index row for
recordings (deliberately a directory scan instead — see §2); a `window
history` CLI subcommand or a way to reach History without the app window
(not asked for, and adding one is cheap later if wanted); PickColor's
interactive grab actually exercised live (forbidden by this stage's own
constraints — see §5); any GUI interaction (clicking History, Pick Color, a
row action, watching an export run) live-tested (same unavoidable reason as
every prior UI-heavy stage — see §6).

---

## 1. PickColor — real, and a real correction to the old stub doc comment

`src/modules/picker.rs` is the new module. Two halves:

- **`ShellScreenshotProxy`** (`#[zbus::proxy]`, private): a client for
  niri's `org.gnome.Shell.Screenshot`. `pick_color()` is declared
  `-> zbus::Result<HashMap<String, OwnedValue>>` — **not** `(f64,f64,f64)`.
- **`pub async fn pick_color(connection) -> Result<(f64,f64,f64), PickColorError>`**
  calls it and hands the reply to `extract_rgb`, which pulls the `"color"`
  key out and decodes it as `(f64,f64,f64)` via `zvariant`'s tuple
  `TryFrom<OwnedValue>` impl.
- **`pub fn rgb_to_hex(r,g,b) -> String`** — moved here from `main.rs`
  (unchanged logic: clamp, `* 255.0`, round, `#RRGGBB`). `main.rs::
  run_pick_color` now calls `modules::picker::rgb_to_hex` instead of a local
  copy.

**The real quirk, found by reading (not assuming) the wire signature**:
`dbus.rs`'s Stage-3-vintage doc comment on the `pick_color` stub claimed
niri's `PickColor` returns a bare `(ddd)`. It does not.
`docs/research/2026-08-08-probes/input-tests/NOTES.md` (Test 4, already in
the repo) recorded the real reply:
`a{sv} 1 "color" (ddd) 0.215686 0.215686 0.215686` — a one-entry dict, the
triple under a `"color"` key. **Live-reconfirmed 2026-08-17, read-only**:

```
$ busctl --user introspect org.gnome.Shell.Screenshot /org/gnome/Shell/Screenshot
.PickColor    method    -    a{sv}    -
```

`ShellScreenshotProxy::pick_color`'s declared return type matches this
exactly. Decoding it as `(ddd)` directly would have failed **every real
call** with a D-Bus signature mismatch — this would not have been caught by
`cargo test` (no live bus in CI) and would only have surfaced the first time
someone actually ran `pick-color` against a real niri.

**This crate's own `PickColor() -> (ddd)`, unaffected** — also
live-reconfirmed, against an isolated test daemon:

```
$ busctl --user introspect io.saola.Capture1 /io/saola/Capture1
.PickColor    method    -    ddd    -
```

`dbus.rs::CaptureService::pick_color`'s body: open a session connection,
call `modules::picker::pick_color`, on success compute the hex
(`picker::rgb_to_hex`), **copy it to the clipboard**
(`storage::copy_text_to_clipboard`, `ClipboardOwner::ThisProcess` — the
daemon outlives the copy, same reasoning `capture_and_save` already uses),
**offer a swatch toast** (`DaemonEvent::ColorPicked { hex, rgb }`,
`try_send`, best-effort like every other bridge in this file), then return
`(r,g,b)`. Clipboard/toast failures are logged and do **not** fail the call
— the caller is still owed the three doubles.

`main.rs`'s `not_yet_implemented` helper is **deleted** — `PickColor` was
the last stub, and a helper with no call sites is dead code under
`-D warnings`.

---

## 2. `modules::toast::ToastKind::Swatch` — the swatch card

New variant: `Swatch { hex: String, rgb: (f64,f64,f64) }`.
`push_swatch(hex, rgb, theme, now)` is the new `ToastStack` method, same
timing/stack rule as `push`/`push_notice`/`push_recording`.

- **Title/body**: `("Color picked", hex)`.
- **Icon tile**: painted the *picked color itself* (`iced::Color::from_rgb`
  from the raw `rgb` tuple, full alpha regardless of the card's own fade —
  see the variant's doc comment for why a desaturated-mid-fade swatch would
  misreport the color). This is the one `ToastKind` whose tile needs no
  `src/icons.rs`/thumbnail workaround at all.
- **Body font**: `saola_theme::convert::mono_font` (PLAN.md task 2's own
  wording: "swatch toast + hex (mono font)") — every other card still uses
  `body_font`; `card_view` picks per-`ToastKind` now.
- **Click**: no-op (`Action::None`) — the clipboard copy already happened at
  pick time, before the toast is even pushed, so there is nothing for a
  click to *do*. Matches `Notice`'s existing posture exactly.

---

## 3. `storage.rs` additions

- **`pub fn copy_text_to_clipboard(text, owner)`** — the text-clipboard
  sibling of `copy_to_clipboard` (which stays PNG-only, unchanged). Backing
  functions generalized rather than duplicated:
  `serve_clipboard_in_process`/`spawn_clipboard_helper` became
  `serve_bytes_in_process(bytes, mime)`/`spawn_clipboard_helper(bytes, mime)`
  (both now take an explicit MIME instead of hardcoding
  `CLIPBOARD_MIME`), plus a new `serve_text_in_process` using
  `wl_clipboard_rs::copy::MimeType::Text`. `run_clipboard_serve` (the hidden
  `clipboard-serve` verb) is **unchanged** — it already took an arbitrary
  `mime: &str`. New constant: `TEXT_MIME = "text/plain;charset=utf-8"` (used
  by the detached-helper path only; the in-process path uses `MimeType::Text`
  directly).
- **`pub fn read_history_entries(path) -> Vec<HistoryEntry>`** — the reader
  half of the JSONL format `storage.rs` already documented but never parsed
  back. Missing file → empty `Vec` (not an error, same posture as everything
  else in this module); unparseable/blank lines are skipped; `kind`/`format`
  decode through `static_kind`/`static_format`, which only accept the
  documented closed-enum values (`"fullscreen"|"region"|"window"`,
  `"webp"|"png"`) — anything else makes the whole line unparseable, since
  `HistoryEntry`'s fields are `&'static str` by design (see that struct's
  doc comment) and there's no honest way to leak an arbitrary string into
  one.
- **`RECORDING_PREFIX`/`VIDEO_EXTENSIONS`** are now `pub(crate)` (were
  private) — `modules::history`'s directory scan reuses them rather than
  re-deriving the filename convention.
- **`pub fn encode_frame`/`pub fn write_atomically`**: unchanged, reused by
  `modules::history::copy_screenshot` for the WebP→PNG re-encode path
  (`encode_frame`) — no `write_atomically` call needed there since Copy
  never writes a file.

**Live-verified, real save + real read-back** (isolated test daemon,
2026-08-17): a real `shot --fullscreen` produced
`{"bytes":289770,"format":"webp","height":1600,"kind":"fullscreen",...}` in
`history.jsonl`; `read_history_entries` round-trips it correctly (also unit
tested against a real temp file, `read_history_entries_reads_a_real_file_in_
append_order`).

---

## 4. `modules::history` — the library, and the schema decision

`src/modules/history.rs` (new). Read its own module doc comment first — it
carries the full reasoning below in more detail, load-bearing for anyone
touching this later.

**The decision Stage 14/15's handoffs both flagged and left open**:
recordings have no `HistoryEntry` row (`storage::allocate_recording_path`'s
own doc comment says so). This stage answers it with a **directory scan**,
not a JSONL schema bump (`v: 2`/a `type` key):

- `fn scan_recordings(dir) -> Vec<RecordingFile>` — `fs::read_dir` filtered
  on `storage::RECORDING_PREFIX`/`VIDEO_EXTENSIONS`; each `RecordingFile`
  carries `path`/`unix` (the file's own `mtime`)/`bytes` (real file size).
  Missing directory → empty `Vec`, not an error.
- **Why a scan and not a schema bump**: it doesn't touch `modules::
  recorder`'s finalization path (a different, already load-bearing
  subsystem with its own "exactly one finalization site" invariant); the
  filename convention already carries everything the library needs; and a
  scan self-heals (delete a recording by hand, it's just gone from the next
  scan — no orphaned row to filter, unlike an index entry).
- **The real cost, named**: a scanned recording has no recorded `kind`
  (fullscreen/region/window) and no duration, only name/size/mtime. A
  future stage wanting either needs the schema bump this one deliberately
  avoided.

**Data model** (pure, fully unit-tested): `HistoryItem::{Screenshot(HistoryEntry),
Recording(RecordingFile)}`, `fn merge_items(entries, recordings) ->
Vec<HistoryItem>` (sorts newest-first by `unix`), `pub fn
load_library(config: &CaptureConfig) -> Vec<HistoryItem>` (the one
env-touching entry point — resolves `storage::history_path()` and
`storage::resolve_save_dir(config.save_dir)`, filters out any
`HistoryEntry` whose file no longer exists, calls `merge_items`).

**Deletion never touches `history.jsonl`** — the index stays append-only
exactly as `storage.rs` already documents ("no rewriting of earlier lines
ever"). `delete_item` removes the file(s) from disk only; a deleted
screenshot's row becomes exactly the kind of unreadable/dangling line
`HistoryEntry`'s own doc comment already tells readers to tolerate, and
`load_library`'s missing-file filter (not a JSONL rewrite) is what makes it
disappear from the library on the next load.

**Actions** (`HistoryModel::update`, returns `Action` — same "child model
hands the parent a value" shape `modules::toast::Action` already
established):

- `Action::Edit(path)` — screenshots only, `App::apply_history_action`
  calls `crate::spawn_editor` (now `pub(crate)`, was private — this and
  `open_containing_dir` are the two visibility widenings `main.rs` needed).
- `Action::OpenDir(path)` — recordings' Open/Edit *and* every item's "Show
  in folder"; calls `crate::open_containing_dir` (also now `pub(crate)`).
- `Action::Export { source, format }` — the one action that needs
  background work; `App::apply_history_action` spawns it via
  `Task::perform(run_export(...), ...)`, which wraps
  `encode::export::export_with_size` in a **fourth** private
  `run_blocking` copy (app.rs's own — see `dbus::run_blocking`'s and
  `editor::run_blocking`'s doc comments for why each module gets its own
  rather than sharing one). Result comes back as
  `history::Message::ExportFinished(path, result)`, matched by **path, not
  index** — every cross-`Task` message in this model is keyed by path
  specifically so a concurrent delete can't invalidate a stale index.

**Copy** (`copy_screenshot`): PNG entries copy their bytes directly; WebP
entries decode via `::image::load_from_memory` (note the leading `::` — the
same `iced::widget::image` name-shadowing gotcha AGENTS.md already documents
for `modules::app`/`modules::editor`, hit again here) and re-encode PNG via
`storage::encode_frame`. No new dependency — `image = "0.25"`'s
default-formats already include WebP decode (only the *lossy encoder* was
the historical gap, which is why `webp` is in the tree at all).

**Thumbnails**: built once, synchronously, at `HistoryModel::load` time
(`build_thumbnail`, screenshots only — a recording's first frame isn't free
to decode without spawning ffmpeg, same reasoning `ToastKind::Recording`
already documents for its own tile). **Not wrapped in `run_blocking`/
`Task::perform`** — this runs on `App::update`'s own call stack when the
History button is pressed. Flagged explicitly (in the module's own doc
comment and in `App::update`'s `HistoryRequested` arm) as the same class of
recorded-not-fixed cost Stage 15's Blur/Pixelate worst case is: a very large
history could make pressing "History" visibly stall the window while every
screenshot decodes in sequence. Not measured (no large history exists to
measure against); reasoned through, not benchmarked.

**View**: a scrollable list, not a wrapped grid (iced 0.14 here has no
flex-wrap widget, and adding `iced_aw` is a new-dependency decision this
stage's brief doesn't ask for — see the module doc comment's "Grid, as
actually built" section). Each row: thumbnail tile (or an ivory placeholder
for recordings), filename (mono font) + kind/size meta line, action buttons.
Delete is a two-step inline confirm — pressing "Delete" swaps that row's
action buttons for "Delete this capture? This can't be undone." + Confirm/
Cancel (wording carries severity, no red, per Design language). A persistent
hint line above the list (shown only when at least one recording is present)
carries PLAN.md task 3's required size-warning teaching note.

---

## 5. `encode::export` — GIF / animated WebP

`src/encode/export.rs` (new). A **batch** job, not an `EncoderSink` — see
its own doc comment's "not an EncoderSink" section for the full reasoning
(no stream to keep alive, `Command::output()` is sufficient with no
stderr-drain thread since this module never writes to ffmpeg's stdin at
all).

- **`AnimatedFormat::{Gif, AnimatedWebp}`** — `.extension()`/`.label()`.
- **`pub fn export(source, format) -> Result<PathBuf, ExportError>`** /
  **`pub fn export_with_size(...) -> Result<(PathBuf, u64), ExportError>`**
  (the one `modules::history` actually calls — avoids a second
  `fs::metadata` call at the caller).
- **GIF**: the standard ffmpeg two-pass recipe — pass 1 `palettegen` writes
  a scratch palette PNG (`.{stem}-palette.png`, dot-prefixed, removed on
  every exit path); pass 2 `paletteuse` (dither `sierra2_4a`) reads both
  inputs and writes the final GIF, `-loop 0`.
- **Animated WebP**: one pass, ffmpeg's `libwebp_anim` muxer — **not** the
  vendored `webp` crate (still-image-only; animation was never in its
  scope). `-lossless 0 -q:v 75 -an` (no audio track).
- Both resampled to a fixed **12 fps** (`EXPORT_FPS`) — a recording's own
  damage-driven cadence can run well past 60 fps in bursts and neither
  format has interframe prediction, so encoding at source rate would be
  enormous for no visible benefit.
- **`unique_export_path`**: `source.with_extension(...)`, `-1`/`-2`/…
  suffixed on collision (a repeat export, or GIF+WebP of the same source
  landing on the same stem) — its own small `ExportError::NoFreeFilename`
  variant, since `EncodeError` had no equivalent (it's always writing a
  freshly-allocated, guaranteed-free path).
- **`ensure_ffmpeg_available()` is called up front**, same "missing-ffmpeg
  detected before any work starts" posture `ffmpeg_cli.rs` already has.

**Not live-tested against a real recording** — none existed in the isolated
test session (a real recording needs `record start`/`stop`, which this
stage's own scope didn't include re-verifying). Argument-builder functions
(`gif_palettegen_args`/`gif_paletteuse_args`/`animated_webp_args`) are unit
tested directly (exact flags, `-loop 0`, `-an`, both inputs/outputs present)
and `unique_export_path`'s collision logic is tested against real temp
directories; the actual `ffmpeg` spawn (`run_ffmpeg`) is deliberately
untested, matching `ffmpeg_cli.rs`'s own precedent of not unit-testing a
real spawn. **A human with a real recording on disk should run both export
buttons once** and confirm the GIF/WebP actually play and look reasonable —
see §6.

---

## 6. `modules::app` integration

- **`ViewState` gains `History(Box<history::HistoryModel>)`** — boxed for
  the same `clippy::large_enum_variant` reason `Editor`'s payload already
  is. The module doc comment now documents this as a **deliberate, narrow
  exception** to "no in-app navigation between views": history has no
  separate-document lifetime the way an edit target does, so toggling it in
  the same running process (rather than spawning a new one, the way `window
  edit <path>` does) is the right call, not a quiet violation of the
  existing rule.
- **New `Message` variants**: `HistoryRequested`/`HistoryClosed` (plain
  state toggles, no daemon call — History reads local files directly and
  needs no connection at all, unlike Capture/Record/Pick Color), `History
  (history::Message)` (delegates, mirrors `Message::Editor`'s shape except
  the child returns an `Action` to interpret rather than a `Task` to just
  map), `PickColorRequested`/`PickColorFinished(Result<(f64,f64,f64),
  String>)`.
- **`start_pick_color`**: same hide-then-ask-the-daemon shape as
  `start_capture`, reusing `busy`/`finish`. Hiding matters more here than
  for a screenshot — the window sitting over the very pixel someone's
  trying to click would be actively unhelpful.
- **Main tab**: a new "More" section label + row (`History`, `Pick Color`,
  both `button::rest` — §11's "exactly one terracotta element" stays
  Capture/Start Recording's). `History` is always enabled; `Pick Color` gates
  on `!busy && connection.is_some()`, same as Capture.
- **History screen**: `history_back_row` ("← Back", `button::rest`) above
  `HistoryModel::view(theme).map(Message::History)`.

**Live-verified against the real session, 2026-08-17** (see §7 for the full
transcript): the app window booted, rendered, and `grim` confirmed the new
"MORE" row with both buttons styled correctly, no crash, no layout
breakage. **Not verified**: actually pressing either button, or any row
action inside History — no synthetic input was used at all this stage (see
§7's reasoning).

---

## 7. Live verification actually performed, and what was deliberately not done

All against a fully isolated test daemon
(`XDG_DATA_HOME`/`--config-dir`/`save-dir` in a scratch dir under this
session's scratchpad, `busctl --user list | grep io.saola.Capture1` checked
empty first, teardown-verified after — both PIDs gone, bus name released):

1. `busctl --user introspect org.gnome.Shell.Screenshot
   /org/gnome/Shell/Screenshot` — confirmed `PickColor`'s real `a{sv}`
   signature (read-only, no grab triggered).
2. `busctl --user introspect io.saola.Capture1 /io/saola/Capture1` —
   confirmed this crate's own `PickColor() -> ddd` is unchanged, and that
   every other method/signal/property is present and unaffected.
3. `shot --fullscreen --no-copy` against the isolated daemon — a real
   capture, saved, and its `history.jsonl` row round-tripped through
   `read_history_entries` correctly (both by direct file inspection and by
   the unit test suite).
4. `saola-capture window` booted against that same daemon; `niri msg
   windows` confirmed it opened as a real floating toplevel
   (`Title: "Saola Capture"`, correct tile size); a `grim` capture cropped
   to the window (via ImageMagick, no synthetic input) visually confirmed
   the Main tab renders correctly **including the new "MORE" row with
   History and Pick Color buttons**, properly styled.
5. Clean teardown confirmed after each step (`ps aux`, `busctl --user
   list`).

**Deliberately NOT done, and why**:

- **PickColor's actual interactive grab was never invoked.** This stage's
  own constraints are explicit: niri's real pointer grab has no cancel path
  a script can safely drive, and there was no human present to click or
  Escape. Verified instead by (1) unit tests on `extract_rgb`/`rgb_to_hex`
  against synthetic `a{sv}` replies and (2) the read-only introspections
  above, which is what actually caught the real `a{sv}`-vs-`(ddd)` bug —
  arguably a stronger check than a single live click would have been, since
  it verifies the *shape* rather than one lucky value.
- **No synthetic input was injected anywhere this stage** — not into the
  app window (History/Pick Color buttons, row actions, the delete confirm),
  not into niri. Everything above is boot-and-observe only. This is a
  narrower posture than Stage 7's own "sanctioned ydotool against a nested
  niri, overlay-only" — that sanction is scoped to the *overlay* surface
  specifically (per this session's own memory note) and doesn't cover a
  plain toplevel window's ordinary buttons, and this stage's own
  instructions gave no separate authorization to extend it. A future stage
  (or Jordan, present) should click through: History → each row's Open/
  Copy/Show-in-folder/Delete-confirm-then-cancel-then-confirm, and Pick
  Color → click a pixel → confirm the swatch toast shows the right color in
  mono font and the hex is genuinely on the clipboard (`wl-paste` or a
  paste target).
- **No GIF/WebP export was run against a real file** — no recording existed
  in the scratch session (would need `record start`/`stop` first, out of
  this stage's own scope to re-exercise). A human with a real
  `Recording_*.mkv` on disk should press both export buttons from the
  History screen and confirm the resulting `.gif`/`.webp` actually play and
  look reasonable (not just that the button reported success).
- **Copy (screenshot → clipboard) was not clicked live** — unit-tested
  logic only (the PNG-reuse and WebP-decode-reencode paths), never
  round-tripped through a real Wayland paste. A human should Copy a WebP
  screenshot from History and paste it somewhere to confirm.

---

## 8. API/signature diffs (for anyone grepping call sites)

- `storage::copy_to_clipboard` unchanged in signature; its two private
  backing functions were renamed/generalized (`serve_clipboard_in_process`
  → `serve_bytes_in_process(bytes, mime)`; `spawn_clipboard_helper(png)` →
  `spawn_clipboard_helper(bytes, mime)`).
- `storage::RECORDING_PREFIX`/`VIDEO_EXTENSIONS`: `const` → `pub(crate)
  const`.
- `dbus.rs::CaptureService::pick_color`: stub body → real (see §1). Its
  `not_yet_implemented` helper function is **deleted**.
- `dbus::DaemonEvent` gained `ColorPicked { hex: String, rgb: (f64,f64,f64) }`.
- `main.rs::Message` gained `ColorPicked { hex, rgb }`; `dbus_worker_stream`
  forwards it; `Daemon::update` calls `toasts.push_swatch`.
- `main.rs::spawn_editor`/`open_containing_dir`: `fn` → `pub(crate) fn`
  (used by `modules::app` now).
- `main.rs::rgb_to_hex`: **deleted** — moved to
  `modules::picker::rgb_to_hex`, `run_pick_color` calls that instead.
- `modules::toast::ToastKind` gained `Swatch { hex: String, rgb: (f64,f64,f64) }`;
  `ToastStack` gained `push_swatch`.
- `modules::app::ViewState` gained `History(Box<history::HistoryModel>)`;
  `Message` gained `HistoryRequested`, `HistoryClosed`,
  `History(history::Message)`, `PickColorRequested`,
  `PickColorFinished(Result<(f64,f64,f64), String>)`.
- `modules::mod.rs` gained `pub mod history;` and `pub mod picker;`.
- `encode/mod.rs` gained `pub mod export;`.

---

## 9. Notes for Stage 17 (CI, packaging, autostart, README)

- **Zero new dependencies this stage** — `Cargo.lock` is unchanged apart
  from this crate's own version. Nothing new for the PKGBUILD/CI's
  dependency list beyond what Stage 11 already added (ffmpeg).
- **README's CLI examples section** should mention `pick-color`'s real
  behavior now (blocks until click, prints/copies/toasts) and the app
  window's History screen — there is no dedicated CLI verb for History, it
  is app-window-only, which the README should say explicitly so nobody goes
  looking for a `saola-capture history` subcommand that doesn't exist.
- **The end-to-end Jordan-run sequence Stage 19 is supposed to write** (per
  PLAN.md's own Stage 19 text: "Print screenshot, region + window shots, a
  30 s recording..., an annotate-and-export round trip, a GIF export, and
  `--no-daemon` scripted capture") should fold in: opening History from the
  app window, deleting a capture (confirming the two-step wording), copying
  a WebP screenshot and pasting it somewhere, and running `pick-color`
  for real (click a pixel, confirm the swatch toast and clipboard). This
  stage could not perform any of those itself — see §7.
- **The History-load-time thumbnail-decode cost** (§4, "recorded rather than
  fixed") is the same category of open item Stage 15's Blur/Pixelate
  worst-case redaction stall is — neither blocks Stage 17's own task list,
  but both are worth a mention if Stage 18's audit goes looking for
  synchronous-work-on-the-UI-thread findings.
