# Stage 15 handoff — annotation editor II: text, steps, blur/pixelate

Forward-facing context for **Stage 16** (history library, color picker, GIF
export — per PLAN.md's task brief for this handoff) and for whoever
eventually live-tests the editor's GUI half (§8 — this stage could not, same
as Stage 14 before it).

Touched: `src/modules/editor.rs` (+~1000 lines incl. ~35 new tests),
`Cargo.toml` (`cosmic-text = "0.15"`, one new survey essay), `CLAUDE.md`.
No other source file changed. **Nothing committed**, as every prior stage.

Gates at hand-off: `cargo build`, `cargo clippy --all-targets -- -D
warnings`, `cargo fmt --check` all clean. `cargo test`: **445 passed**
(Stage 14: 418 → +27 real new tests, not +35 — some of the +35 replaced
existing call sites whose signatures changed, see §6), 1 ignored (a manual
`#[ignore]` perf test — see §4).

---

## 0. What is and isn't wired

**Is:** `window edit <path>`'s toolbar now has ten tools: Select, Crop,
Arrow, Rectangle, Ellipse, Freehand (Stage 14) plus **Text, Step, Blur,
Pixelate** (this stage). Text places an empty selected caption on click and
shows a content field + three size stops while it's selected; Step places
an auto-numbered terracotta/ivory badge on click; Blur/Pixelate drag a
rectangle and apply their kernel to the canvas the instant the drag ends.
The footer grew an export panel (WebP/PNG format picker, a free-text
quality field) and a fourth action button, **Save & Copy**.

**Is not:** resize-after-placement for Text/Step (same Stage-14-inherited
scope line — select/move/delete only); a *color* for Step badges (always
fixed terracotta/ivory, ignores the color picker — see §1); wrapping text
onto multiple lines at a width limit (both painters give text unbounded
width — see §1); undo granularity finer than "one Snapshot per commit" for
text content edits (typing a caption doesn't push an undo entry per
keystroke, matching `set_color`/`set_width`'s existing posture — see §1);
GUI live-testing of any kind (§8, same unavoidable reason as Stage 14).

---

## 1. Text and Step — the two `Shape` variants (mechanical, as promised)

Stage 14's handoff predicted exactly this shape for a new tool: "one more
`Shape` variant, one more arm in `shape_bounds`/`shape_hit`/
`translate_shape`/`paint_shape` (raster)/`draw_shape` (interactive)". That
held for both:

```rust
Shape::Text { position: Point, content: String, size: f32 }  // position = top-left
Shape::Step { position: Point, number: u32 }                  // position = disc center
```

**Interaction**: both commit on a plain **press**, not a drag —
`EditorModel::pressed`'s `Tool::Text`/`Tool::Step` arms call
`commit_new_annotation` directly (which already pushes undo and selects the
new annotation — unchanged from Stage 14) and reset `interaction` to `Idle`
so a stray release at the same point doesn't do anything. This is a genuine
divergence from every Stage 14 tool (all drag-based) and from Blur/Pixelate
(also drag-based, §2) — Text/Step are the only "single click places a
complete thing" tools in the file.

**Text content/size editing lives in the toolbar, not inline on the
canvas**: `EditorModel::selected_text() -> Option<(&str, f32)>` is what
`toolbar_view` checks to decide whether to show the content `text_input`
(bound to `Message::TextContentChanged` → `set_selected_text_content`) and
the `TextSizeStop` segmented control (bound to `Message::TextSizeSelected`
→ `set_text_size`). Since a click both creates *and* selects a Text
annotation, this row appears immediately after placing one — no separate
"now edit it" step. Re-selecting an existing caption with the Select tool
later shows the same row, so editing a typo is "select it, retype it," not
"delete and re-place." **Neither edit pushes an undo entry** — same
"picker restyle, not a drawing action" posture `set_color`/`set_width`
already had; only the *placement* itself is undoable as a whole.

**`TextSizeStop::{Small,Medium,Large}` resolves to three real tokens**
(`TextSizeScale::from_theme`): `theme.typography.size.body`/
`.section_heading`/`.screen_title`. `EditorModel` itself never sees a
`TextSizeStop` — it only ever stores the resolved `f32`
(`current_text_size`), mirroring `AnnotationColor`/`ColorPalette`'s
existing "pure model, resolve at the GUI edge" split exactly.
`TextSizeScale::nearest(size) -> TextSizeStop` exists purely to decide which
segmented-control button to highlight for a selected caption's current
size — display-only, never feeds back into the model.

**Step numbers never renumber on delete.** `EditorModel::next_step: u32`
is monotonic, like `next_id`; deleting badge #2 out of #1/#2/#3 and placing
a new one gives #4, not a renumbered #2 or #3. Deliberate — "explicit over
clever," and a renumbering feature nobody asked for. **Step badges ignore
the color picker entirely**: `paint_step_badge`/`draw_shape`'s Step arm both
reach for `palette.terracotta`/`palette.ivory` directly, never
`annotation.color` — this is a fixed two-tone badge, the same "solid icon"
category CLAUDE.md's design language already carves out for record/stop/
play, not a customizable shape.

**Bounding boxes are estimates, on purpose.** Neither `shape_bounds` arm has
real glyph metrics available (that needs the font system, which lives in
the GUI-wrapper half — see §3, not the pure-geometry half `shape_bounds`
lives in). Text's estimate is `content.chars().count() * size * 0.6` wide
by `size * 1.3` tall; Step's is a fixed `STEP_BADGE_RADIUS`-sized square.
Both feed into `shape_hit`'s existing "click the bounding box, not the
exact ink" leniency (already true of Rectangle/Ellipse in Stage 14), so the
imprecision is absorbed rather than causing a real bug — but a very long
caption's *actual* rendered width (which depends on the real font, kerning,
etc.) can differ noticeably from the estimate, meaning the click-to-select
hitbox for a long caption may not exactly match where the glyphs visually
end. Not observed live (§8) — flagged as the kind of thing a human
looking at a long caption should specifically check.

---

## 2. Blur and Pixelate — `Tool`s, deliberately **not** `Shape` variants

This is the one place Stage 15 broke from the "one more Shape variant"
pattern, and it was a deliberate reading of PLAN.md's own task 3 wording
("Blur/pixelate compose destructively into the export... but stay editable
in-session via the undo stack"), not an oversight. Full reasoning is in
`EditorModel::apply_redaction`'s doc comment; the short version:

- **The drag commits immediately on release** — no Crop-style pending/Apply
  step. `Interaction::Redacting { kind: RedactKind, anchor, current }` is a
  new interaction variant with its own live preview (`draw_region_dimming`,
  see below), but `EditorModel::released`'s `Redacting` arm calls
  `apply_redaction` directly the moment `MIN_CROP_SIZE` is cleared — same
  gate Crop uses to discard a stray click, reused rather than duplicated.
- **`apply_redaction` mutates `self.canvas`'s own pixels in place**, not a
  translucent shape composited on top at save time. It clones the current
  pixel buffer, runs the kernel (`blur_region`/`pixelate_region`) on the
  affected rectangle, builds a new `Frame` from the result, pushes one undo
  `Snapshot` (of the *pre*-redaction canvas — same `push_undo` every other
  mutator uses), and replaces `self.canvas`. **This is what makes the
  redaction promise (CLAUDE.md Boundaries: "blur/pixelate must be
  irreversible in exported files") literally true rather than aspirational
  as of this stage**: after the mutation, the original pixels in that
  rectangle exist nowhere except in that one in-memory `Snapshot` — never
  written to disk, gone once the undo stack is pushed past or the process
  exits. If it *had* been a `Shape` composited on top instead, the original
  pixels would still be sitting untouched in `self.canvas`, recoverable by
  deleting the annotation — exactly the "trivially reversible from the
  exported file" failure mode the task brief's own teaching-note
  requirement calls out.
- **`EditorModel::released` now returns `bool`**, not `()` — `true` iff the
  canvas *pixels themselves* changed (a committed redaction), mirroring the
  signal `apply_crop`'s own `bool` return already gives its caller.
  `EditorState::update`'s `Message::Released` arm uses it to decide whether
  to call `refresh_display_handle()` — previously that only happened for
  Undo/Redo/ApplyCrop; now a committed Blur/Pixelate triggers it too, since
  those are the only two *other* ways `self.canvas`'s bytes can change.
  Every other `Interaction` arm still returns `false` (verified directly —
  `released_returns_false_for_every_non_redaction_interaction`).
- **Live preview reuses Crop's dimming visual, generalized**:
  `draw_crop_dimming` was renamed `draw_region_dimming` (CLAUDE.md's own
  Stage 14 bullet updated to note the rename) and is now called for both
  `Interaction::Cropping` and `Interaction::Redacting` — same "dim outside
  the rectangle, dashed accent outline" language for any "drag defines the
  area this action affects" gesture. No new visual language was invented.

**The kernels themselves** (`src/modules/editor.rs`, in a new "Redaction
kernels" section, pure `&mut [u8]` functions, unit-tested with synthetic
buffers exactly like the Stage 14 `paint_*` functions):

- **`pixelate_region(buf, width, height, rect, block_size)`**: tiles `rect`
  into `block_size²` blocks, each replaced by the average color of the
  pixels it covers. `O(rect area)` — every pixel read once, written once,
  regardless of `block_size`. `PIXELATE_BLOCK_PX = 18`.
- **`blur_region(buf, width, height, rect, radius)`**: a separable box blur
  (`box_blur_axis`, horizontal pass then vertical pass) sourced from a
  **padded copy** of the region (expanded by `radius` on every side,
  clamped to the canvas) so edge pixels blend with real neighboring
  content rather than a synthetic clamp at the drag rectangle's own
  boundary; only pixels inside `rect` are written back. `BLUR_RADIUS_PX =
  14`. **The sliding window is the load-bearing detail**: each output pixel
  updates a running sum by subtracting the sample that just left the window
  and adding the one that just entered, so the whole pass is `O(pixels)`
  **independent of `radius`** — a larger blur radius costs more padding
  (a constant border), not more per-pixel work. This is what makes §4's
  performance numbers hold regardless of how strong a blur someone might
  want later.
- Both are edge-clamped, `saturating_add`-guarded against `u32` overflow in
  their bounds math (the `PixelRect` inputs are always canvas-bounded in
  practice via `redact_pixel_rect`, but the kernels themselves don't assume
  that — see `pixelate_region_does_not_panic_on_a_rect_extending_past_the_
  image`/`blur_region_does_not_panic_near_every_image_edge`), and neither
  panics on `block_size`/`radius` of `0` (clamped to `1` internally).
- `redact_pixel_rect(rect: Rectangle, width, height) -> Option<PixelRect>`
  is the redaction-kernel counterpart to `apply_crop`'s own pixel-rect
  construction — clamps a (possibly sub-pixel, possibly partly off-canvas)
  drag rectangle into the canvas, `None` if it doesn't overlap at all.

---

## 3. `cosmic-text` — real glyph rendering, zero net new crates

The one genuine capability gap Stage 14 flagged and Stage 15 had to fill:
placing legible text into an RGBA8 buffer at Save/Copy time needs a font
shaping + rasterization library, and Stage 14's raster half had no font
concept at all. **`cosmic-text = "0.15"`** was already resolved in
`Cargo.lock` at exactly this version (pulled transitively via `cryoglyph`,
`iced_wgpu`'s own glyph-atlas dependency — iced's on-screen text rendering
already runs on it), so declaring it directly here with its own default
features (`std`, `swash`, `fontconfig` — identical to what `cryoglyph`
already requests) added **one edge in the dependency graph, zero new
`[[package]]` entries** — confirmed by inspecting the `Cargo.lock` diff
directly, which is exactly two lines. Full essay in `Cargo.toml`.

**`raster_text(buf, width, height, request: TextRasterRequest)`** is the
one function that touches `cosmic-text`. `TextRasterRequest` bundles
`content`/`family`/`size_px`/`color`/`position`/`align` — a plain data
struct, not behavior — purely so the function stays under clippy's
`too_many_arguments` lint (nine scalar args would have tripped it; every
other `paint_*` function already sits at or near that same 7-arg ceiling
with buffer+geometry+color+width alone). It:

1. Guards against empty/whitespace content and non-finite/non-positive
   sizes up front (a no-op, never a panic — `Buffer::new`'s underlying
   `assert_ne!(line_height, 0.0)` is the one real panic path in
   `cosmic-text`'s own code on this route, and the size guard makes it
   unreachable from here).
2. Shapes via a **process-lifetime `FontSystem`/`SwashCache` pair**
   (`font_resources()`, a `std::sync::OnceLock<Mutex<FontResources>>`) —
   `FontSystem::new()` scans the system's installed fonts via `fontconfig`,
   real work worth paying once per process rather than once per Save/Copy;
   `SwashCache` additionally memoizes rasterized glyph bitmaps by `(font,
   size, glyph id)`. Lock poisoning is recovered via
   `unwrap_or_else(PoisonError::into_inner)`, never `.unwrap()` — no-panic
   rule.
3. Measures the shaped text's extent (`buffer.layout_runs()`, summing
   `line_w`/`line_top`/`line_height`) to compute an offset for
   `TextRasterAlign::{TopLeft, Center}` — `TopLeft` for the Text tool
   (matches `Shape::Text::position`'s own top-left convention), `Center`
   for a Step badge's numeral (so it sits centered in the disc regardless
   of digit count).
4. Blends via `buffer.draw(cache, base_color, |x, y, w, h, pixel| ...)` —
   `cosmic-text`'s own per-pixel callback, where `pixel`'s alpha channel
   **is the glyph's antialiasing coverage** for a standard (non-color)
   glyph (`SwashCache::with_pixels`'s `Content::Mask` branch, verified by
   reading its source directly) — exactly the shape `blend_pixel` already
   expects, so no adapter layer was needed. The annotation's own color alpha
   is folded in (`coverage * color.a / 255`), so a hypothetical future
   translucent color would translucently blend text too.

**No text wrapping, on either painter.** `raster_text` calls
`buffer.set_size(None, None)` (unbounded); the interactive painter's
`canvas::Text` uses the default `max_width: f32::INFINITY`. A very long
caption will run off the edge of the canvas rather than wrap — consistent
between the two painters (good), but neither has a length limit or an
ellipsis. Not built; PLAN.md's task list didn't ask for it and neither
painter needed it to satisfy "one more Shape variant, one more pair of
painters."

**Font family**: `theme.typography.family_ui` ("IBM Plex Sans" in the
built-in theme), resolved once at `EditorState::load` into
`text_font_family: String` (raster path) and `saola_theme::convert::
ui_font(theme)` into `EditorCanvas::text_font: iced::Font` (interactive
path) — two different representations because `cosmic-text`'s `Family::
Name(&str)` and `iced::Font` are unrelated types with no conversion between
them; both are resolved from the same theme field, so they can't drift.
**If `fontconfig` can't resolve "IBM Plex Sans" on a given machine**
(not installed), `cosmic-text`'s own family-fallback substitution picks
whatever sans-serif *is* available — no crash, no empty text, just a
different-looking font than intended. This was reasoned through, not
observed live (Jordan's saola-theme design almost certainly has IBM Plex
Sans installed already, since `saola-panel`/`saola-lockscreen` both depend
on rendering it correctly) — see §8's new checklist item.

**Testing posture, and why it's deliberately shallow on exactness**: only
one test (`raster_text_does_not_panic_on_empty_or_degenerate_input`)
exercises `raster_text` directly, and it asserts *only* that nothing panics
across six edge cases (empty content, whitespace-only, zero size, NaN size,
an unresolvable font family, an extreme off-canvas position) — never that
specific pixels get painted. **This is intentional, not a coverage gap**:
real glyph shapes depend on which fonts are actually installed on whatever
machine runs `cargo test`, which this suite has no way to control or assume
— asserting exact pixel output would make the test suite's pass/fail
depend on the *test runner's* font configuration, not on this code being
correct. Every other new test (kernels, Text/Step model behavior, `parse_
quality`) is exact-value-asserted the normal way; only the font-shaping
boundary itself gets the "doesn't crash" treatment.

---

## 4. Kernel performance at 4K (this handoff's required section)

Measured with a new `#[ignore]` test
(`redaction_kernels_are_fast_at_4k` — run manually via `cargo test
--release -- --ignored redaction_kernels_are_fast_at_4k --nocapture`;
ignored by default so normal `cargo test` runs don't pay for it or become
machine-speed-flaky), on a 3840×2160 canvas, both in `--release` and in the
plain `cargo test` **dev** build (the profile `cargo run`/the app window
process actually use — see below for why that distinction matters):

| Scenario | Release | Dev |
|---|---|---|
| Pixelate, 600×400 region (a realistic redaction: a face, a plate, a phone number) | 377 µs | 10.2 ms |
| Pixelate, whole 3840×2160 canvas (worst case — nothing stops a user dragging Blur across the whole screenshot) | 11.2 ms | 369 ms |
| Blur, 600×400 region | 7.8 ms | 121 ms |
| Blur, whole 3840×2160 canvas (worst case) | **361 ms** | **4.39 s** |

**Pixelate is fast at any realistic scale, in both profiles.** Blur's
*typical* cost (a real redaction region, not the whole frame) is small
enough not to matter (121 ms dev, 7.8 ms release). **Blur's worst case is
the one number worth carrying forward**: 4.39 seconds in a dev build is a
real, user-visible stall.

**Why the dev number matters as much as the release one**: `Cargo.toml`'s
own `[profile.dev.package."*"] opt-level = 3` override (Stage 1's own
"4-second screenshot" essay) optimizes *dependencies* but deliberately
leaves this crate's *own* code — including `blur_region` — at opt-level 0,
because the keybind-triggered debug binary is what Jordan actually runs day
to day. `blur_region` is exactly the kind of hot, tight numeric loop that
essay already identified as suffering worst under opt-level 0 (its own
measured case was libwebp/PNG encoding, 2.8 s → 0.37 s from the override);
`blur_region` is *this crate's own code*, so the override doesn't help it
at all.

**This runs synchronously, on the UI thread — a real, unfixed finding, not
a hypothetical.** Unlike Save/Copy's `compose` call (wrapped in
`run_blocking`/`tokio::task::spawn_blocking`, off iced's executor thread),
`EditorModel::apply_redaction` is called directly from
`EditorModel::released`, which is called directly from `EditorState::
update`'s `Message::Released` arm — i.e., on iced's own update/render call
stack. A worst-case whole-canvas Blur drag would visibly freeze the app
window for **up to ~4.4 seconds in the dev build Jordan runs day to day**
(361 ms even in `--release`). This is not a correctness bug (no panic, no
data loss, no wrong output — the redaction completes correctly, just
slowly) and PLAN.md's task list didn't ask for async redaction, but it is
a real, quantified UX gap this stage's own performance measurement
surfaced and did **not** fix.

**Why it wasn't fixed here**: every `EditorModel` mutator (`pressed`,
`released`, `set_color`, `undo`, ...) is a synchronous, pure, `&mut self`
method — that uniformity is *why* the whole model is unit-testable the way
CLAUDE.md's testing rule wants ("pure data + functions, unit-tested"), and
it's the same reason Crop's own `apply_crop` (also potentially expensive
for a huge crop, though its cost is a linear memcpy, not a blur kernel) is
synchronous too. Making just redaction's kernel call async without
breaking that uniform contract needs real design — likely something in the
shape of "commit a placeholder state synchronously, kick off the kernel via
a `Task::perform`/`run_blocking`, apply the result when it lands, with a
busy/pending UI state in between" — not a small patch, and risks
introducing exactly the kind of undo-stack-timing subtlety
(`EncoderSink`'s "exactly one finalization site" lesson from Stage 11,
applied to a much smaller surface) that's easy to get subtly wrong under
time pressure. **Recorded as open work for whoever next touches this
path** — Stage 16 doesn't obviously need to be the one (it's history
library/color picker/GIF export, not editor performance), but this is the
kind of gap that should be picked up deliberately rather than rediscovered
by a user noticing a stall.

---

## 5. Export panel (PLAN.md task 2)

New `EditorState` fields: `export_format: ImageFormat` (starts at the
loaded `capture.toml`'s `image-format` default, then a plain independent
user choice) and `export_quality_text: String` (kept as raw text —
`self.webp_quality.to_string()` initially — the same "free text field,
parsed at use" shape `save_as_path` already had, so a momentarily-invalid
edit mid-typing doesn't fight the user).

- **Format** (`Message::ExportFormatSelected`, a WebP/PNG segmented
  control) governs **Save/Save As only**. `format_for_path` — unchanged
  logic, just fed a different default — still lets an explicit `.png`/
  `.webp` extension typed into Save As win outright over the panel's
  picker; the panel's picker is only consulted when the target path's own
  extension doesn't disambiguate (no extension, or something like `.jpg`).
- **Quality** (`Message::ExportQualityChanged`, a free-text field) is
  parsed by a new pure function, `parse_quality(text, default) -> u8`:
  `1..=100`, clamped (not truncated — parses as `u32` first specifically
  so `"99999"` clamps to `100` instead of wrapping via a raw `as u8`), any
  unparseable text falls back to the loaded `capture.toml`'s `webp-quality`
  — same "bad knob → warned default" posture `config.rs`'s own hand-walked
  TOML parsing already established for the identical knob's file form.
  Unit-tested directly (`parse_quality_clamps_and_falls_back_on_garbage`).
- **Copy is still hardcoded PNG, unaffected by the format picker.**
  `copy_composed`'s doc comment now says so explicitly. This was a
  deliberate decision, not an oversight — changing it would break
  `storage.rs`'s "the clipboard always gets PNG" rule (every paste target
  understands PNG; not every one understands WebP), which this stage's own
  task brief gave no reason to revisit.
- **"Copy vs save vs both"** turned out not to need a mode/state-machine at
  all: Save, Copy, and a new **Save & Copy** button
  (`Message::SaveAndCopyPressed` → `start_save_and_copy`) are three
  independent buttons, the third just running save-then-copy in one
  `Task`. Save-then-copy, not the reverse — if the save fails there's
  nothing worth copying, so the copy is skipped and the save's own error is
  what reaches the user (`?` short-circuits inside the `run_blocking`
  closure). `Message::SaveAndCopyFinished(Result<PathBuf, String>)` is a
  new, separate message from `SaveFinished`/`CopyFinished` so its feedback
  text ("Saved & copied: …") is distinguishable from a plain Save's.

---

## 6. API/signature diffs, precisely (for anyone grepping call sites)

- `compose(base, annotations, palette)` → `compose(base, annotations,
  palette, font_family: &str)`.
- `paint_shape(buf, width, height, shape, color, stroke_width)` →
  `paint_shape(buf, width, height, annotation: &Annotation, palette:
  ColorPalette, font_family: &str)` — takes the whole `Annotation` now,
  not a pre-resolved `color`, because `Step`'s badge needs the *whole*
  palette (terracotta **and** ivory) regardless of `annotation.color`.
- `save_to_path`/`copy_composed` both gained a `font_family: &str`
  parameter, threaded from `EditorState::text_font_family`.
- `draw_shape(frame, fit, shape, color, view_stroke_width)` →
  `draw_shape(frame, fit, shape, color, view_stroke_width, palette:
  ColorPalette, text_font: iced::Font)` — same reasoning as `paint_shape`,
  interactive side. Every call site (the per-annotation loop, both preview
  calls for `DrawingLine`/`DrawingFreehand`) updated; previews never
  produce a Text/Step shape (those tools have no drag-preview interaction —
  §1), so the extra params are unused dead weight on those two call sites,
  not a correctness concern.
- `EditorModel::released(point: Point) -> ()` → `-> bool` — see §2.
- `draw_crop_dimming` renamed `draw_region_dimming` (same signature) — now
  called for both `Interaction::Cropping` and `Interaction::Redacting`.
- `EditorCanvas` gained one field, `text_font: iced::Font`.
- `EditorState` gained four fields: `text_font_family: String`,
  `text_sizes: TextSizeScale`, `export_format: ImageFormat`,
  `export_quality_text: String`. Lost one: `default_format: ImageFormat`
  was removed (it was only ever read once, at construction, to seed
  `export_format` — keeping it around unread tripped `dead_code`).
- `Message` gained six variants: `TextContentChanged(String)`,
  `TextSizeSelected(TextSizeStop)`, `ExportFormatSelected(ImageFormat)`,
  `ExportQualityChanged(String)`, `SaveAndCopyPressed`,
  `SaveAndCopyFinished(Result<PathBuf, String>)`.
- `Tool::ALL` grew from 6 to 10 entries.

---

## 7. Design-token gaps found (no tag bump, same posture as Stages 6/7/14)

- No size/ratio tokens for the Step badge: `STEP_BADGE_RADIUS` (14.0),
  `STEP_BADGE_FONT_RATIO` (1.05) — same "drawing-tool parameter, not a
  design-system value" category `StrokeWidth::pixels`/`HANDLE_RADIUS`
  already established.
- No width token for the export panel's quality `text_input`:
  `QUALITY_FIELD_WIDTH` (120.0) — a one-off layout parameter, not a
  reusable scale value.
- **Not a gap, worth flagging as the positive counter-example**: the Text
  tool's three size stops are *not* a new literal scale —`TextSizeScale`
  resolves to three existing `theme.typography.size` entries (`body`,
  `section_heading`, `screen_title`). Whoever adds the next size-driven
  control to this file should default to "is there an existing token
  scale I can pick three stops from" before reaching for new named
  literals — this stage found one place where the answer was yes.

---

## 8. What was NOT verified, and how a human should check it

Everything in §1–2's pure-logic half (Text/Step placement, selection,
editing, undo; the redaction kernels' exact pixel math) is unit-tested with
either exact assertions (kernels, model state) or explicit not-panicking
assertions where exactness isn't knowable in a test environment
(`raster_text` — §3). **Nothing about how any of this actually *looks* on
screen has been rendered once.** Same structural reason as Stage 14: this
crate's live-testing recipe has no safe way to drive a plain
`iced::application` window's pointer/keyboard without Jordan present, and
this stage's own brief didn't ask for it either. What a human should check,
roughly in order of "most likely to reveal something Stage 14's own
checklist didn't cover":

1. **Font rendering, the new risk this stage adds**: open the editor, place
   a Text caption, and look at it. Does it actually render in IBM Plex
   Sans, or visibly some other sans-serif? (If `fontconfig` can't resolve
   the family name on this machine, `cosmic-text` silently substitutes —
   see §3 — and the only way to notice is looking.) Same question for a
   Step badge's numeral.
2. Place several Step badges, confirm they read 1, 2, 3, ... in placement
   order, confirm deleting one and placing a new one does **not**
   renumber the survivors (this is model-tested — §1 — but the *visual*
   confirmation that the right disc has the right number on screen has
   never happened).
3. Drag out a Blur region over something with real detail (text, an icon)
   and confirm it visibly blurs — and specifically confirm the **live drag
   preview** (the dimmed rectangle) looks distinct enough from Crop's own
   preview that a user isn't confused about which tool is active (both use
   `draw_region_dimming` now — same visual language on purpose, but worth
   a sanity look).
4. Drag Pixelate over the same kind of area, confirm the mosaic effect is
   visible and roughly the size `PIXELATE_BLOCK_PX` (18px) implies.
5. **The performance finding from §4, made real**: select the Blur tool and
   drag a rectangle across the *entire* canvas on a large (4K-ish)
   screenshot. Does the app window visibly freeze for a few seconds before
   the result appears? If Jordan is running a dev build (the normal case),
   this should be observable and roughly match the ~4.4 s dev-build number
   above. Confirming this live (rather than trusting the synthetic-buffer
   timing test) would upgrade §4 from "measured on synthetic data,
   reasoned through architecturally" to "watched happen."
6. Undo a committed Blur/Pixelate and confirm the canvas visibly reverts to
   the sharp original — not just that `can_undo()` is true.
7. Type a long caption (long enough to run past the canvas's right edge)
   and confirm it behaves as documented in §3 (runs off, doesn't wrap,
   doesn't crash/truncate oddly) rather than something unexpected — no
   painter in this file has ever been watched handle an overflow like this.
8. Try the export panel: pick PNG instead of the default, Save, confirm the
   written file is actually a PNG (not just that the button didn't error);
   try Save & Copy and confirm both effects happened (a changed file on
   disk *and* a working paste).

No destructive/input-injection risk in any of the above — same posture as
Stage 14's own list — this is a normal niri toplevel, nothing here maps a
layer-shell surface or needs keyboard exclusivity.

---

## 9. Notes for Stage 16 (history library, color picker, GIF export)

- **`cosmic-text` is now in the tree** and process-lifetime-cached
  (`font_resources()`, private to `modules::editor`). If Stage 16's history
  library or GIF export ever needs to render text (a caption overlay on a
  thumbnail, a filename label, whatever), there's a real font-shaping
  capability already paid for — check whether reusing `raster_text`'s
  shape (or exposing a shared version of it) makes more sense than a
  second font pipeline, rather than assuming this crate still has none.
- **A "color picker" in Stage 16's own brief is presumably about the
  `pick-color` CLI verb/D-Bus method** (still the one real stub — `PickColor`,
  Stage 16 per every prior stage's Status paragraph), not this editor's
  three-color `AnnotationColor` palette — don't conflate the two; they're
  unrelated pieces of scope that happen to share the word "color."
- **The history-index gap Stage 14 flagged is still open and still not this
  stage's problem**: an editor Save writes no `history.jsonl` row (Stage
  14's own reasoning: `HistoryEntry`'s schema is still/webp-image-shaped,
  and deciding what an *edited* file's row should look like is a library
  design decision). Stage 16 is very likely the stage that finally has to
  answer this — search `history.jsonl` after any editor Save in this stage
  too and you'll still find nothing new, exactly as Stage 14 documented.
- **The §4 redaction-performance gap is real, quantified, and unclaimed.**
  It doesn't obviously belong to Stage 16's task list (history/color-picker/
  GIF-export), but it's the kind of thing worth a one-line mention in
  whatever comes after if nobody's picked it up by then — a multi-second
  UI freeze on a worst-case Blur drag is the sort of finding that's easy to
  lose track of once a few more stages have landed on top of it.
