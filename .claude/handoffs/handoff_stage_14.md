# Stage 14 handoff — annotation editor I: canvas, crop, shapes

Forward-facing context for **Stage 15** (text/step-number/blur-pixelate
tools, extending this stage's tool-state model) and for whoever eventually
live-tests the editor's GUI half (see §7 — this stage could not).

New file: `src/modules/editor.rs` (~2,700 lines incl. ~40 tests). Touched:
`src/modules/app.rs` (the editor stub → real editor wiring, window sizing),
`src/modules/mod.rs` (module registration + doc comment), `src/storage.rs`
(two small `pub` exposures), `CLAUDE.md`. **No new dependency** — `iced`'s
`canvas` feature was already enabled in `Cargo.toml` with a comment
anticipating exactly this stage. **Nothing committed**, as every prior stage.

Gates at hand-off: `cargo build`, `cargo clippy --all-targets -- -D
warnings`, `cargo fmt --check` clean. `cargo test`: **418 passed** (Stage 13:
373 → +45: ~40 in `editor.rs`, 2 new in `app.rs`, minus 1 removed
`load_image` test that moved to `editor::load_canvas`'s own coverage).

---

## 0. What is and isn't wired

**Is:** `window edit <path>` boots the real editor — crop (drag + confirm),
arrow, rectangle, ellipse, freehand; select/move/delete of placed
annotations; undo/redo; terracotta/ink/ivory color picker + three
stroke-width presets; Save (overwrite), Save As (a text field, writes to
whatever path is typed), Copy (always PNG, to the clipboard). Toast click and
`OpenWindow` both already spawned `window edit <path>` before this stage —
neither needed a code change to "land in a real editor now"; they just do,
because the process they spawn no longer boots a stub.

**Is not:** resizing a placed annotation (PLAN.md's task list says
"select/move/delete", not resize — see §2); any keyboard shortcut (Delete
key, Enter, Escape) — every action is a button; text, step numbers,
blur/pixelate (Stage 15's own task list); a history-index row for an edited
file (editor saves are not new captures — see §4); GUI live-testing of any
kind (see §7 — this is the one thing every future stage extending this file
should plan to eventually do, since nobody has yet).

---

## 1. Canvas architecture — two painters, one model

Every annotation is stored once as vector data (`Shape`, `enum { Arrow,
Rectangle, Ellipse, Freehand }`, all in **image-space** — the base image's
own pixel coordinates, origin top-left, independent of window size or the
on-screen scale) and painted **twice** by two painters that share no code:

1. **Interactive** (`EditorCanvas`, a `canvas::Program<editor::Message>`):
   GPU vector geometry via `canvas::Path`/`Stroke`/`Frame`, redrawn every
   frame a drag is live. Cheap regardless of the base image's resolution —
   it never touches a pixel buffer, only a handful of `Path`s.
2. **Raster** (`compose` + the `paint_*` free functions): a hand-rolled
   software rasterizer that paints each `Annotation` directly into the base
   image's RGBA8 bytes. Runs **once, at Save/Copy time**, never during
   interaction. This is the tested half — every primitive (`paint_segment_capsule`,
   `paint_rect_outline`, `paint_ellipse_outline`, `paint_arrow`,
   `fill_triangle`, `blend_pixel`) is a free function over `&mut [u8]` with
   no `Theme`, no widget, no clock; ~20 of the ~40 tests exercise this half
   with tiny synthetic buffers, asserting exact pixel bytes.

**Why two painters, not one:** iced 0.14 has no supported way to extract
owned RGBA bytes from a live `canvas` render mid-frame (no stable
"screenshot this widget" API this crate can lean on), and even if there were,
a GPU round trip is the wrong tool for "encode these exact bytes to WebP" —
that just needs the bytes. The two painters must stay *visually* consistent
(same geometry math, same arrowhead proportions — `draw_arrowhead` and
`paint_arrow` compute the head the same way, just in two coordinate spaces)
but never need to be pixel-identical, because only the raster one is ever
compared against expected bytes.

**View-space ↔ image-space**: `FitTransform` (private, `struct { scale,
offset_x, offset_y }`) implements the same `ContentFit::Contain`-style
letterbox math the `image` widget uses underneath it, computed fresh from
`canvas::Program::draw`'s own `bounds: Rectangle` on every call — so it
tracks window resizes for free with no cached state to invalidate. Unit
tested (`fit_transform_letterboxes_the_shorter_axis`, a round-trip test).
**The pointer path never needs overlay's "ButtonPressed carries no
coordinates" workaround**: `canvas::Program::update` hands you
`cursor.position_in(bounds)` on every event, including presses/releases —
that's a genuine, real difference from `main.rs`'s raw
`iced::event::listen_with` stream that `modules::overlay` has to live with
(see CLAUDE.md's new Stage 14 gotchas bullet). `EditorCanvas::resolve_point`
falls back to a `last_pointer: Option<Point>` (image-space, sourced from
`EditorModel::pointer`) only for the edge case of a release delivered after
the OS has already moved the cursor outside the canvas widget's bounds —
untested live (see §7), reasoned through but not proven.

---

## 2. The tool-state model — what Stage 15 extends

`EditorModel` (pure: no `Theme`, no iced widget type beyond
`Point`/`Rectangle`, which are plain geometry) is the whole interaction
surface:

```rust
pub struct EditorModel {
    canvas: Arc<Frame>,             // base pixels — see §3
    annotations: Vec<Annotation>,
    next_id: AnnotationId,          // u64, monotonic
    tool: Tool,
    current_color: AnnotationColor, // Terracotta | Ink | Ivory
    current_width: StrokeWidth,     // Thin | Medium | Thick
    selected: Option<AnnotationId>,
    interaction: Interaction,       // ephemeral, never snapshotted — see below
    pending_crop: Option<Rectangle>,
    pointer: Option<Point>,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
}
```

Three entry points carry the whole pointer contract, all taking an
already-resolved **image-space** `Point`: `pressed`, `pointer_moved`,
`released`. `EditorCanvas` is the only thing that knows view-space exists.

**`Interaction`** (private) is the in-flight, not-yet-committed pointer
state — `DrawingLine { tool: LineTool, anchor, current }` (Arrow/Rectangle/
Ellipse share this: a two-point drag, converted to the right `Shape` at
release via `line_tool_shape`), `DrawingFreehand { points }`, `Moving { id,
grab, origin, preview }` (Select tool — `origin` unchanged for the undo
comparison, `preview` recomputed fresh from `origin` on every motion, never
accumulated, so rounding can't drift), `Cropping { anchor, current }`.
**Never part of a `Snapshot`** — only a committed result pushes undo.

**Undo/redo is whole-document snapshots**, not per-field diffs:
```rust
struct Snapshot { canvas: Arc<Frame>, annotations: Vec<Annotation> }
```
`Arc<Frame>` clone is O(1) (a refcount bump) except across a real `apply_crop`
(a new `Frame`, a new `Arc`), so pushing undo on every draw/move/delete costs
one small `Vec<Annotation>` clone, not a multi-megabyte pixel copy. `push_undo`
is called explicitly, right before the mutation, in every arm that decided
something real happened — critically, **a plain click-to-select does not
push an undo entry** (`Moving`'s release compares `preview != origin` first;
`a_plain_click_to_select_does_not_push_an_undo_entry` guards this), and
neither does an aborted tiny drag (`MIN_SHAPE_SIZE`/`MIN_CROP_SIZE` gate
`released`'s commit arms before `push_undo` is ever called).

**Deliberately out of scope, and why (read before extending):**
- **No resize-after-placement.** PLAN.md's task list names "select/move/
  delete" for placed annotations — resize is not in it. Move alone needed no
  handle system at all (just `origin`/`preview`), so `modules::overlay`'s
  whole `Handle`/`resize`/`hit_test` machinery was never pulled in — a
  deliberate choice, not an oversight; see `editor.rs`'s `rect_from_corners`
  doc comment for the explicit "this file reuses none of `overlay::Rect`'s
  handle-flip resize logic" note. **If Stage 15 needs resize**, the shape is:
  add `Interaction::Resizing { id, handle: ... }` variants, hit-test the
  corners of `shape_bounds(&annotation.shape)`, and either build a small
  local 4-corner handle set or *reconsider* importing `overlay::{Rect,
  Handle, resize}` at that point (they're `pub`, and the reasoning against
  reusing them in Stage 14 — "this file didn't need them" — stops applying
  once resize is a real requirement).
- **No keyboard shortcuts.** Every action (Undo, Redo, Delete, Apply Crop,
  Cancel Crop, Save, Save As, Copy) is a button. This app's daemon-side
  modules have `main.rs`'s `iced::event::listen_with` for global keyboard
  capture, but that pattern is wired around `SurfaceRole` and the daemon's
  own event loop — a plain `iced::application` (the window process) has no
  established equivalent in this codebase yet. Buttons are unambiguous,
  discoverable without documentation, and their wiring is a one-line
  `on_press_maybe` a reviewer can eyeball without a compositor — which
  mattered given §7. If Stage 15 wants Escape-cancels-crop or Delete-key,
  that needs a `Subscription` on `iced::keyboard::on_key_press` (a real
  iced 0.14 API, not used anywhere in this file — check its exact signature
  before assuming it matches `main.rs`'s layer-shell-side pattern).
- **`Shape` is a closed `enum`, deliberately.** Adding a Stage 15 tool
  (text, a numbered step badge, a blur/pixelate region) is: one more `Shape`
  variant, one more arm in `shape_bounds`/`shape_hit`/`translate_shape`/
  `paint_shape` (raster)/`draw_shape` (interactive) — every one of those is
  an exhaustive `match`, so the compiler names every site that needs the new
  arm. There is no `Box<dyn Annotation>`-style open trait, on purpose
  (CLAUDE.md's "prefer explicit code over generic/dynamic abstraction").
  **One caveat for text specifically**: every `paint_*` function here
  operates in *raw pixel* space with no font/glyph dependency at all; a text
  annotation's raster half will need a font rasterizer (or reuse
  `iced_graphics`/`cosmic-text`'s own — check what's already in the dep tree
  via `iced`'s `advanced` feature before adding a new text-shaping crate).
- **Ellipse hit-testing uses the bounding box, not the curve.** `shape_hit`
  for `Rectangle`/`Ellipse` is `rect.expand(tolerance).contains(point)` —
  clicking inside an ellipse's "corners" (outside the curve, inside the box)
  still selects it. A deliberate leniency (real vector editors do this too),
  not a bug; tightening it to the actual curve is a `shape_hit` change only,
  isolated from everything else.

---

## 3. `capture::Frame` reuse (a design decision worth knowing about)

`EditorModel.canvas: Arc<Frame>` reuses `crate::capture::Frame` **directly**
rather than inventing a parallel "editor canvas" pixel-buffer type. This
means `apply_crop` is `self.canvas.crop(pixel_rect)` — the *exact* same
already-tested clamping/buffer-slicing code `capture::screencopy` uses for a
live screenshot crop, not a reimplementation. `Frame::scale` is always `1.0`
here (a saved capture's pixels already *are* the physical-pixel image the
user gets back; nothing in this file ever calls
`capture::logical_to_pixel_rect`, the only consumer of `scale`). The
trade-off: `EditorModel` now has a `crate::capture` dependency it technically
didn't strictly need (a bespoke `struct Canvas { width, height, pixels }`
would have been equally simple) — judged worth it for the crop-math reuse
and for `storage::encode_frame`/`write_atomically` already being written in
terms of `Frame`, so the save path needs no adapter type at all.

**One precision detail that matters if you touch `apply_crop`**: the
translation applied to every annotation after a crop uses the **rounded
integer `PixelRect`** (`pixel_rect.x as f32`, not the original sub-pixel
`Rectangle` the drag produced) — using the float rect would leave annotations
a fraction of a pixel off from where the image was actually cropped from,
since `Frame::crop` itself only ever sees the rounded rect.

---

## 4. Save/Save As/Copy — deliberately *not* `storage::save_capture`

`save_to_path`/`copy_composed` are small, standalone functions, not a call
into `storage::save_capture`/`save_capture_indexing_to`. That pipeline
resolves a save directory, invents a timestamped filename, and appends a
history-index row — all correct for "a brand-new capture just happened,"
none of it applicable to "the user is editing a file that already exists and
wants it written back or copied." Two small, real exposures were added to
`storage.rs` instead of duplicating its encoder-selection logic:

- `pub fn encode_frame(frame: &Frame, format: ImageFormat, webp_quality: u8)
  -> Result<Vec<u8>, StorageError>` — the same `match` `save_capture_indexing_to`
  already had inline, pulled out so a second caller doesn't duplicate it.
- `write_atomically` changed from private `fn` to `pub fn` — same `.part` +
  `rename` crash-safety guarantee, now available to a caller writing to an
  arbitrary explicit path instead of one `storage.rs` itself resolved.

**No history-index row is written for an editor save.** `HistoryEntry`'s
documented schema (`storage.rs`) fixes `format` to `"webp" | "png"` and
carries still-image-only fields written against Stage 16's library reader;
deciding what an *edited* file's history entry should look like (a new row?
an update to the original's row? a schema version bump?) is a decision that
belongs to whichever stage builds the library, the same reasoning Stage 11
used to justify recordings having no history row at all. Flagged here so
Stage 16 doesn't discover a silent gap — search history.jsonl after an
editor Save and you will find nothing new.

**Format inference** (`format_for_path`): `.png`/`.webp` extensions (case-
insensitive) map directly; anything else (no extension, `.jpg`, a bare
filename typed into Save As) falls back to `capture.toml`'s `image-format`
default, passed in from `App::boot`'s already-loaded `CaptureConfig`.

**Copy always encodes PNG** regardless of the file's own format, and hands
it to a **detached** `clipboard-serve` helper (`ClipboardOwner::
DetachedHelper`), not `ThisProcess` — reasoned explicitly in the module
comment: the app window process is exactly the kind of thing a user closes
right after copying ("copy the annotated shot, paste it somewhere, done"),
so the clipboard's owner must outlive *this* process, the same logic
`storage.rs` already applies to `--no-daemon` CLI shots.

---

## 5. `storage.rs`/`app.rs` diffs, precisely

- `storage.rs`: `pub fn encode_frame` (new, ~15 lines), `write_atomically`
  visibility `fn` → `pub fn` (body unchanged). Nothing else touched; no
  encoder logic duplicated.
- `app.rs`:
  - `ViewState::Editor.editor` field type changed from `Result<image::Handle,
    String>` to `Result<Box<editor::EditorState>, String>` — **boxed**
    because `clippy::large_enum_variant` fired once `EditorState` grew a
    whole `EditorModel` (undo/redo stacks) inside it; see CLAUDE.md's new
    Stage 14 gotchas bullet.
  - `App::boot` calls `editor::EditorState::load(path, &theme,
    config.image_format, config.webp_quality).map(Box::new)`.
  - `App::title` now reads `EditorState::path()` (the *current* path — a
    successful Save As updates it) rather than the outer `ViewState::Editor`
    field frozen at boot, so a Save As rename is reflected in the window
    header. This was a real bug caught during this stage's own review, not
    speculative — the outer `path` field is still used for the `Err` (failed
    to load) case, where there is no `EditorState` to ask.
  - `Message::Editor(editor::Message)` added, delegating to
    `EditorState::update(..).map(Message::Editor)` — the same nested-`Message`
    shape every module in this crate already uses.
  - `run()` picks window size/resizability by `WindowMode` now:
    `WindowMode::Main` keeps the old fixed `popover_width`/`window_height`,
    non-resizable; `WindowMode::Edit(_)` uses a new `editor_window_size`
    (token-derived, `popover_width * 3.0` wide) and **is resizable** — a
    screenshot-sized canvas needs real room, unlike the Main tab's five
    control rows. The outer `container(content)` in `App::view` changed from
    `.width(Length::Fixed(popover_width))` to `.width(Length::Fill)` for
    exactly this reason — the old `Fixed` would have silently clipped the
    editor's body to the Main tab's narrow footprint.
  - `load_image`/`editor_view` deleted (moved into `editor::load_canvas` and
    `EditorState::view`/`editor_error_view` respectively). `editor_error_view`
    is the one case still in `app.rs`'s own hands: a file that failed to
    decode at all.

---

## 6. Design-token gaps found (no tag bump, same posture as Stages 6/7)

- **No color token for a windowed canvas's crop dimming.** `scrims.capture`
  is specified for the full-output capture overlay (`modules::overlay`)
  specifically; reaching for it here would be using a token for a surface it
  doesn't describe. `draw_crop_dimming` uses a plain `Color { a: 0.55,
  ..BLACK }` instead — flagged, not silently hardcoded-and-uncommented.
- **No stroke-width scale.** `StrokeWidth::pixels` (3.0/6.0/10.0, named
  `Thin`/`Medium`/`Thick`) is a drawing-tool parameter, the same category
  `modules::overlay::HANDLE_RADIUS` already established as "not a design
  token" — a design system has no opinion on a pen's own width presets.

---

## 7. What was NOT verified, and how a human should check it

**Everything in §1's raster half (`compose`, every `paint_*` function,
`FitTransform`'s math) is unit-tested with exact pixel-byte assertions** —
high confidence there. **Everything GUI (`EditorCanvas`'s actual on-screen
rendering, the toolbar layout, resizing behavior, Save As's text field,
whether a drag genuinely feels responsive) has never been rendered once.**
This stage's own brief explicitly forbids driving a GUI here (the app window
is a plain `iced::application` toplevel, not a layer-shell surface the
nested-niri recipe can safely exercise, and no synthetic input was injected
against any real session). What a human (Jordan) should do, roughly in order
of "most likely to have broken something":

1. `cargo run -- shot --fullscreen` (or reuse any existing capture), then
   `cargo run -- window edit <path>` on it. Confirm the window opens at the
   new, larger, resizable size and the image is visible via `ContentFit::
   Contain`.
2. Draw one of each shape (Arrow, Rectangle, Ellipse, Freehand) — confirm
   each renders roughly where dragged, in the selected color, at the
   selected width. Confirm a tiny click-without-drag on each drawing tool
   places nothing (this **is** tested at the model level; confirming the
   canvas painter doesn't visually show a stray dot is not).
3. Select tool: click a shape (confirm the dashed selection highlight
   appears), drag it (confirm it follows the cursor live via the `preview`
   substitution — this is the one thing `EditorCanvas::effective_shape`
   exists for and has never been watched happen), click empty space
   (confirm deselect), press Delete (confirm removal).
4. Crop: drag a rectangle, confirm the dimmed-outside preview tracks the
   drag, press Apply Crop, confirm the canvas visibly shrinks and any
   annotation that was partially inside now renders correctly *offset* (not
   just "still there somewhere").
5. Undo/Redo through a few of the above — confirm the canvas visually
   reverts (not just the annotation list; a crop's undo should visibly grow
   the canvas back).
6. Save (confirm the file on disk changed — `file <path>` timestamp, or
   diff), Save As to a new path (confirm **both** files exist and the window
   title updates to the new name), Copy (confirm a paste target receives the
   composed image, not just the base).
7. Resize the window — confirm the canvas/toolbar reflow rather than
   clipping (this is the `Length::Fill` change in §5, never watched happen).
8. The one edge case reasoned through but not observed: drag a shape or a
   crop rectangle **past the edge of the canvas widget, into the toolbar or
   footer area, and release there.** `EditorCanvas::resolve_point`'s
   `last_pointer` fallback should make this degrade gracefully (the drag
   freezes at the canvas edge, the release still commits using that frozen
   position) rather than silently dropping the interaction — confirm that's
   actually what it feels like.

No destructive/input-injection risk in any of the above (this is a normal
niri toplevel like any other app window — nothing here maps a layer-shell
surface or asks for keyboard exclusivity), so none of it needs the
nested-niri recipe or Jordan's special presence the way the region overlay
does. It just needs someone to look at it, which no stage has done yet.
