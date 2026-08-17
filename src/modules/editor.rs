//! The annotation editor: crop, arrow, rectangle, ellipse, freehand — PLAN.md
//! Stage 14, replacing Stage 9's read-only editor stub
//! (`modules::app::editor_view` before this stage; see that module's doc
//! comment for the "editor stub" section this file makes real).
//!
//! # Where this lives (teaching note)
//!
//! This is **not** a daemon surface — it has no `SurfaceRole`, maps no
//! layer-shell surface, grabs no keyboard. It is the body of the app
//! window's `ViewState::Editor` (`modules::app`), a plain `iced::application`
//! window running in its own process. `modules::app::App::view` renders
//! [`EditorState::view`] and maps its `Message` into `app::Message::Editor`,
//! exactly the way `main.rs` nests `modules::overlay::Message` into its own
//! `Message::Overlay` — "every module maps to a signal... nested `Message`
//! enum" (CLAUDE.md).
//!
//! # Two rendering paths, on purpose (the canvas architecture)
//!
//! An annotation is stored once, as vector data ([`Shape`]), and painted
//! **twice** by two independent painters that must agree visually but never
//! share code:
//!
//! 1. **Interactive** ([`EditorCanvas`], an `iced::widget::canvas::Program`):
//!    GPU vector geometry (`canvas::Path`/`Stroke`), redrawn every frame a
//!    drag is live. Fast regardless of the base image's resolution, because
//!    it never touches a pixel buffer — it draws maybe a dozen strokes.
//! 2. **Raster** ([`compose`] and everything under "Raster compositing"
//!    below): a hand-rolled software rasterizer that paints each
//!    [`Annotation`] directly into the base image's RGBA8 bytes, run
//!    **once, at save time**. This is the path CLAUDE.md's testing rule
//!    ("pure data + functions, unit-tested") is written for — every
//!    primitive (a thick line, a rectangle ring, an ellipse ring, a
//!    triangular arrowhead) is a free function over `&mut [u8]` with no
//!    `Theme`, no iced widget, no clock, exhaustively unit-tested with small
//!    synthetic buffers below.
//!
//! Why not one path? iced 0.14 has no supported way to rasterize a `canvas`
//! widget to an owned RGBA buffer outside the live GPU frame it was drawn
//! into (extracting pixels from a `wgpu` surface mid-render is not a stable,
//! sandboxable operation this crate can lean on) — and even if it were, a
//! GPU round trip is the wrong tool for "encode this exact byte buffer to
//! WebP," which just needs the bytes. Splitting the two means the tested
//! surface (raster) never depends on a live renderer, and the interactive
//! surface (vector) never has to be pixel-exact — only visually consistent,
//! which a human verifies (see the Stage 14 handoff's GUI section).
//!
//! # The tool-state model (Stage 15 extends this)
//!
//! [`EditorModel`] is pure data plus pure functions — no `Theme`, no iced
//! type beyond `iced::Point`/`Rectangle` (themselves plain `f32` structs with
//! no rendering dependency) — mirroring `modules::overlay`'s split of "pure
//! geometry core, unit-tested" from "GUI wrapper, deferred to human-verify."
//! [`EditorModel::pressed`]/[`pointer_moved`](EditorModel::pointer_moved)/
//! [`released`](EditorModel::released) take an already-resolved **image-space**
//! point (the base image's own pixel coordinates — origin at its top-left,
//! independent of window size or the on-screen scale factor) and are the
//! entire interaction surface; [`EditorCanvas`] (GUI-only, below) is the only
//! thing that knows about *view*-space (on-screen widget pixels) at all, via
//! [`FitTransform`].
//!
//! **Deliberately out of scope for Stage 14** (see the module's own
//! `Tool`/`Shape` doc comments and the Stage 14 handoff for the fuller
//! reasoning Stage 15 should read before extending this):
//! - **No resize-after-placement.** PLAN.md's task list asks for
//!   "select/move/delete of placed annotations" — resize is not named, so a
//!   selected annotation can be moved and deleted but not dragged bigger or
//!   smaller. Move alone needed no handle system at all.
//! - **No keyboard shortcuts** (Delete key, Enter to confirm a crop, Escape
//!   to cancel). Every action is a button. A plain `iced::application` has no
//!   established global-keyboard-listener pattern in this codebase the way
//!   the daemon's `iced::event::listen_with` does (that pattern is
//!   `main.rs`-only, wired around `SurfaceRole`s), and GUI interaction can't
//!   be live-tested in this stage's environment anyway (see Boundaries in
//!   the task brief) — buttons are unambiguous, discoverable, and their
//!   wiring is trivial to eyeball-review without a compositor.
//! - **No text, step-numbers, or blur/pixelate tools** — literally Stage 15's
//!   task list. [`Tool`]/[`Shape`]/[`AnnotationColor`] are written to make
//!   adding a variant mechanical: one more `Shape` case, one more `paint_*`
//!   function, one more arm in the handful of exhaustive `match`es below.
//!
//! # Raster-composition performance (handoff detail, and binding for Stage 15)
//!
//! - [`compose`] is `O(image pixels)` for the base copy plus
//!   `O(annotation stroke area)` per annotation — never `O(image pixels ×
//!   annotation count)`, because every `paint_*` function iterates only its
//!   own shape's bounding box (expanded by the stroke radius), not the whole
//!   canvas. A handful of arrows on a 4K screenshot touches a few thousand
//!   pixels, not eight million.
//! - **[`compose`] runs exactly twice per editor session in the common
//!   case**: once for Save, once for Copy (each re-clones the base pixel
//!   buffer and re-paints every annotation from scratch — there is no
//!   incremental/cached composite). Both run inside [`run_blocking`]
//!   (`tokio::task::spawn_blocking`, the same guarded pattern
//!   `dbus::run_blocking` already uses), off iced's own executor thread, so a
//!   large image's encode+write cannot stall the UI.
//! - **The interactive path is the one that must stay cheap on every
//!   frame**, and it does: [`EditorCanvas::draw`] never touches the base
//!   pixel buffer at all (the `image` widget underneath draws it once per
//!   `view()`, from a cached [`iced::widget::image::Handle`] —
//!   [`EditorState::display_handle`] — that is only ever rebuilt when the
//!   canvas *pixels themselves* change, i.e. load, crop, undo/redo across a
//!   crop; never on every `CursorMoved`). A drag that fires fifty
//!   `PointerMoved` messages a second costs fifty small `Vec`/`Rectangle`
//!   updates and fifty cheap vector redraws, not fifty pixel-buffer clones.
//! - **Freehand point growth is bounded**, not by a cap but by spacing:
//!   [`MIN_FREEHAND_SPACING`] only appends a point when the pointer has moved
//!   at least that far in image-space since the last one, so a stroke across
//!   a 2000px-wide image tops out in the low hundreds of points, not
//!   thousands.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{button, canvas, column, container, image, row, scrollable, text, text_input};
use iced::{mouse, Center, Element, Length, Padding, Point, Rectangle, Size, Task};
use saola_theme::{ColorExt, Surface, Theme};

use crate::capture::{Frame, PixelRect};
use crate::config::ImageFormat;
// The one piece of `modules::app`'s segmented control this module shares
// rather than duplicates: the geometry must match between the two twins or
// the same control would look different on the two surfaces. (The ~25 lines
// of `segmented_row` itself stay duplicated for the reason that function's
// own doc comment gives — it hardcodes its owner's `Message` type.)
use crate::modules::app::SEGMENT_INSET;
use crate::storage::{self, ClipboardOwner, StorageError};

// ---------------------------------------------------------------------
// Interaction constants — geometry/behaviour, not design tokens. Same
// posture as `modules::overlay`'s own constants block: a design system has
// no opinion on how many image-pixels a drag needs before it's a "real"
// shape, or how close a click has to land to select one.
// ---------------------------------------------------------------------

/// Below this drag distance (image-space px), a completed arrow/rectangle/
/// ellipse drag is discarded instead of becoming a zero-size annotation
/// nobody meant to place. Mirrors `modules::overlay::MIN_SELECTION`'s
/// reasoning exactly.
const MIN_SHAPE_SIZE: f32 = 3.0;

/// Same idea as [`MIN_SHAPE_SIZE`], for the crop rectangle.
const MIN_CROP_SIZE: f32 = 4.0;

/// A freehand stroke only appends a point once the pointer has moved at
/// least this far (image-space px) from the last recorded point — see the
/// module doc comment's performance section for why this matters.
const MIN_FREEHAND_SPACING: f32 = 3.0;

/// How close (image-space px) a click has to land to a placed annotation's
/// outline (or, for a filled test, inside its bounding box) to select it
/// with the Select tool.
const SELECT_TOLERANCE: f32 = 6.0;

/// The minimum on-screen stroke radius (image-space px, before the view
/// scale is applied) a shape is ever painted at — guards against a
/// zero-width stroke silently painting nothing.
const MIN_STROKE_RADIUS: f32 = 0.5;

/// A numbered-step badge's disc radius, image-space px. Not a
/// `saola-theme` token — the same "a drawing tool's own parameter, not an
/// interface control's size" posture as [`StrokeWidth::pixels`] and
/// `modules::overlay::HANDLE_RADIUS` (see CLAUDE.md's design-language
/// gaps section, which this stage extends rather than re-litigates).
const STEP_BADGE_RADIUS: f32 = 14.0;

/// The step badge's numeral size, as a multiple of [`STEP_BADGE_RADIUS`] —
/// picked so a two-digit number ("12") still fits comfortably inside the
/// disc at the default radius above.
const STEP_BADGE_FONT_RATIO: f32 = 1.05;

/// The Blur tool's box-blur kernel radius, image-space px — same
/// not-a-token posture as [`STEP_BADGE_RADIUS`]. See [`blur_region`]'s doc
/// comment for why the *radius* doesn't change this kernel's asymptotic
/// cost (it's an `O(1)`-per-pixel sliding window, not `O(radius)`).
const BLUR_RADIUS_PX: u32 = 14;

/// The Pixelate tool's mosaic block size, image-space px.
const PIXELATE_BLOCK_PX: u32 = 18;

/// The export panel's quality field width — a layout parameter for one
/// small numeric `text_input`, not a `saola-theme` size token (the same
/// "not everything with a number is a design-system value" posture as
/// [`STEP_BADGE_RADIUS`] above).
const QUALITY_FIELD_WIDTH: f32 = 120.0;

// ---------------------------------------------------------------------
// Pure geometry helpers — no `Theme`, no widget, unit-tested below.
// ---------------------------------------------------------------------

/// Maps between **image-space** (the base image's own pixel coordinates,
/// origin top-left) and **view-space** (the editor canvas widget's own
/// on-screen pixels) under `ContentFit::Contain`-style letterboxing: the
/// image is scaled uniformly to fit the view and centered, exactly like the
/// `iced::widget::image` drawn underneath it (see the module doc comment) —
/// the two must agree, or the interactive overlay drifts off the picture it
/// is annotating.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FitTransform {
    scale: f32,
    offset_x: f32,
    offset_y: f32,
}

impl FitTransform {
    fn new(image_size: (f32, f32), view_size: (f32, f32)) -> Self {
        let (image_w, image_h) = image_size;
        let (view_w, view_h) = view_size;
        if image_w <= 0.0 || image_h <= 0.0 || view_w <= 0.0 || view_h <= 0.0 {
            return FitTransform {
                scale: 1.0,
                offset_x: 0.0,
                offset_y: 0.0,
            };
        }
        let scale = (view_w / image_w).min(view_h / image_h);
        FitTransform {
            scale,
            offset_x: (view_w - image_w * scale) / 2.0,
            offset_y: (view_h - image_h * scale) / 2.0,
        }
    }

    fn to_view(self, point: Point) -> Point {
        Point::new(
            point.x * self.scale + self.offset_x,
            point.y * self.scale + self.offset_y,
        )
    }

    fn to_image(self, point: Point) -> Point {
        if self.scale <= 0.0 {
            return Point::ORIGIN;
        }
        Point::new(
            (point.x - self.offset_x) / self.scale,
            (point.y - self.offset_y) / self.scale,
        )
    }
}

/// Clamps `point` into `[0, width] x [0, height]`, treating non-finite input
/// as the origin rather than propagating it — the same NaN-safety
/// `modules::overlay::clamp_f32`/`clamp_point` guard for, duplicated locally
/// (a few lines) rather than exposed cross-module: the two overlays operate
/// in unrelated coordinate spaces and CLAUDE.md's own precedent (`clamp_f32`
/// reimplemented per-module) treats this as acceptable, cheap duplication
/// rather than a coupling worth introducing.
fn clamp_to_canvas(point: Point, width: f32, height: f32) -> Point {
    let clamp = |value: f32, max: f32| {
        if !value.is_finite() {
            0.0
        } else {
            value.clamp(0.0, max.max(0.0))
        }
    };
    Point::new(clamp(point.x, width), clamp(point.y, height))
}

/// The rectangle spanned by two corners, in either order — same contract as
/// `modules::overlay::Rect::from_corners`, over `iced::Rectangle` instead of
/// that module's own `Rect` (this file has no need for `Rect`'s handle-flip
/// resize machinery — see the module doc comment's "deliberately out of
/// scope" section — so it reuses iced's own rectangle type directly rather
/// than importing `overlay::Rect` for a shape it only ever constructs and
/// reads).
fn rect_from_corners(a: Point, b: Point) -> Rectangle {
    Rectangle {
        x: a.x.min(b.x),
        y: a.y.min(b.y),
        width: (b.x - a.x).abs(),
        height: (b.y - a.y).abs(),
    }
}

fn translate_point(point: Point, dx: f32, dy: f32) -> Point {
    Point::new(point.x + dx, point.y + dy)
}

fn translate_rect(rect: Rectangle, dx: f32, dy: f32) -> Rectangle {
    Rectangle {
        x: rect.x + dx,
        y: rect.y + dy,
        ..rect
    }
}

fn distance(a: Point, b: Point) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// Shortest distance from `p` to the segment `a`-`b` (not the infinite
/// line) — the building block for arrow/freehand hit-testing and for their
/// thick-line rasterization (a "capsule": every point within `radius` of the
/// segment).
fn distance_to_segment(p: Point, a: Point, b: Point) -> f32 {
    let abx = b.x - a.x;
    let aby = b.y - a.y;
    let len_sq = abx * abx + aby * aby;
    if len_sq <= f32::EPSILON {
        return distance(p, a);
    }
    let t = (((p.x - a.x) * abx + (p.y - a.y) * aby) / len_sq).clamp(0.0, 1.0);
    let proj = Point::new(a.x + t * abx, a.y + t * aby);
    distance(p, proj)
}

fn rects_intersect(a: Rectangle, b: Rectangle) -> bool {
    a.x < b.x + b.width && a.x + a.width > b.x && a.y < b.y + b.height && a.y + a.height > b.y
}

// ---------------------------------------------------------------------
// The annotation model
// ---------------------------------------------------------------------

pub type AnnotationId = u64;

/// The ten tools this file supports. `Select` is the odd one out — every
/// drawing variant *places* something on press/drag; `Select` instead picks
/// up, moves and (via the Delete button) removes an existing one. See the
/// module doc comment for why resize isn't a capability of this tool.
///
/// **Stage 15 additions** (`Text`, `Step`, `Blur`, `Pixelate`), and how each
/// one's interaction shape differs from Stage 14's four drawing tools:
/// - `Text`/`Step` commit on a plain **press**, not a drag — see
///   [`EditorModel::pressed`]'s own arms. A click places an annotation
///   immediately (empty content for Text, the next auto-incrementing number
///   for Step) and selects it, so the toolbar's content/size controls (only
///   shown while a `Shape::Text` is selected) appear right away.
/// - `Blur`/`Pixelate` drag a rectangle exactly like `Crop`, but — unlike
///   `Crop` — commit **immediately on release**, with no separate Apply
///   button: see [`EditorModel::apply_redaction`]'s doc comment for why
///   that asymmetry is deliberate, not an oversight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Crop,
    Arrow,
    Rectangle,
    Ellipse,
    Freehand,
    Text,
    Step,
    Blur,
    Pixelate,
}

impl Tool {
    pub const ALL: [Tool; 10] = [
        Tool::Select,
        Tool::Crop,
        Tool::Arrow,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Freehand,
        Tool::Text,
        Tool::Step,
        Tool::Blur,
        Tool::Pixelate,
    ];

    fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Crop => "Crop",
            Tool::Arrow => "Arrow",
            Tool::Rectangle => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Freehand => "Freehand",
            Tool::Text => "Text",
            Tool::Step => "Step",
            Tool::Blur => "Blur",
            Tool::Pixelate => "Pixelate",
        }
    }
}

/// Which region kernel a [`Tool::Blur`]/[`Tool::Pixelate`] drag applies —
/// see [`EditorModel::apply_redaction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedactKind {
    Blur,
    Pixelate,
}

/// Arrow/Rectangle/Ellipse share one "drag from an anchor to the current
/// point" interaction ([`Interaction::DrawingLine`]) — this is which of the
/// three the drag will become at release. `Freehand` and `Crop` accumulate
/// differently (a polyline; a to-be-confirmed rectangle) and are not part of
/// this set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineTool {
    Arrow,
    Rectangle,
    Ellipse,
}

fn line_tool_shape(tool: LineTool, anchor: Point, current: Point) -> Shape {
    match tool {
        LineTool::Arrow => Shape::Arrow {
            start: anchor,
            end: current,
        },
        LineTool::Rectangle => Shape::Rectangle {
            rect: rect_from_corners(anchor, current),
        },
        LineTool::Ellipse => Shape::Ellipse {
            rect: rect_from_corners(anchor, current),
        },
    }
}

/// The restrained three-color set the style guide's own "terracotta
/// default; ink/ivory alternates" line (CLAUDE.md, this stage's task 1)
/// asks for — resolved against a [`ColorPalette`] rather than a `Theme`
/// directly, so [`compose`] and every `paint_*` function underneath it stay
/// theme-free and unit-testable with synthetic colors (see [`ColorPalette`]'s
/// own doc comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationColor {
    Terracotta,
    Ink,
    Ivory,
}

impl AnnotationColor {
    pub const ALL: [AnnotationColor; 3] = [
        AnnotationColor::Terracotta,
        AnnotationColor::Ink,
        AnnotationColor::Ivory,
    ];

    fn label(self) -> &'static str {
        match self {
            AnnotationColor::Terracotta => "Terracotta",
            AnnotationColor::Ink => "Ink",
            AnnotationColor::Ivory => "Ivory",
        }
    }

    fn resolve(self, palette: ColorPalette) -> saola_theme::tokens::Color {
        match self {
            AnnotationColor::Terracotta => palette.terracotta,
            AnnotationColor::Ink => palette.ink,
            AnnotationColor::Ivory => palette.ivory,
        }
    }
}

/// The three real token colors a [`Shape`] can be painted in, resolved once
/// from `saola_theme::Theme::saola()` at the GUI edge ([`EditorState::new`])
/// and threaded through as plain `Copy` data from there on. Every pure
/// function below this line — [`compose`], every `paint_*` — takes a
/// `ColorPalette`, never a `Theme`, which is what lets their tests build a
/// synthetic palette with arbitrary colors and assert exact output bytes
/// with zero dependency on `saola-theme` at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorPalette {
    pub terracotta: saola_theme::tokens::Color,
    pub ink: saola_theme::tokens::Color,
    pub ivory: saola_theme::tokens::Color,
}

impl ColorPalette {
    fn from_theme(theme: &Theme) -> Self {
        ColorPalette {
            terracotta: theme.palette.accent,
            ink: theme.palette.ink,
            ivory: theme.palette.paper,
        }
    }
}

/// The closed set of stroke widths a new annotation is drawn at — a
/// segmented control's worth of presets, per-annotation once chosen (not a
/// free slider; this app has no styled slider yet, and three presets is
/// exactly what the style guide's other segmented controls already look
/// like).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrokeWidth {
    Thin,
    Medium,
    Thick,
}

impl StrokeWidth {
    pub const ALL: [StrokeWidth; 3] = [StrokeWidth::Thin, StrokeWidth::Medium, StrokeWidth::Thick];

    fn label(self) -> &'static str {
        match self {
            StrokeWidth::Thin => "Thin",
            StrokeWidth::Medium => "Medium",
            StrokeWidth::Thick => "Thick",
        }
    }

    /// Image-space pixels. Deliberately not a `saola-theme` token — like
    /// `modules::overlay`'s `HANDLE_RADIUS`, this is a drawing-tool
    /// parameter, not an interface control's size.
    fn pixels(self) -> f32 {
        match self {
            StrokeWidth::Thin => 3.0,
            StrokeWidth::Medium => 6.0,
            StrokeWidth::Thick => 10.0,
        }
    }
}

/// The Text tool's three size stops — PLAN.md Stage 15's "size stops from
/// tokens" requirement. [`EditorModel`] never sees this type: it only ever
/// stores a resolved `f32` (mirroring [`AnnotationColor`]/[`ColorPalette`]'s
/// own "pure model, resolve at the GUI edge" split) — see [`TextSizeScale`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextSizeStop {
    Small,
    Medium,
    Large,
}

impl TextSizeStop {
    pub const ALL: [TextSizeStop; 3] = [
        TextSizeStop::Small,
        TextSizeStop::Medium,
        TextSizeStop::Large,
    ];

    fn label(self) -> &'static str {
        match self {
            TextSizeStop::Small => "Small",
            TextSizeStop::Medium => "Medium",
            TextSizeStop::Large => "Large",
        }
    }
}

/// The logical-pixel sizes [`TextSizeStop`] resolves to — three existing
/// entries from `saola_theme::Theme::typography.size`
/// (`ColorPalette::from_theme`'s sibling for text size), not a bespoke
/// scale: `body` is the smallest named size still comfortably legible
/// pasted onto a screenshot, `section_heading` a mid callout, and
/// `screen_title` — the scale's largest named stop — a banner-sized label.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TextSizeScale {
    small: f32,
    medium: f32,
    large: f32,
}

impl TextSizeScale {
    fn from_theme(theme: &Theme) -> Self {
        TextSizeScale {
            small: theme.typography.size.body,
            medium: theme.typography.size.section_heading,
            large: theme.typography.size.screen_title,
        }
    }

    fn resolve(self, stop: TextSizeStop) -> f32 {
        match stop {
            TextSizeStop::Small => self.small,
            TextSizeStop::Medium => self.medium,
            TextSizeStop::Large => self.large,
        }
    }

    /// The stop whose resolved size is closest to `size` — used only to
    /// decide which segmented-control option to highlight for a selected
    /// Text annotation. [`EditorModel`] stores a plain resolved `f32`, not a
    /// `TextSizeStop` (see [`EditorModel::current_text_size`]'s doc
    /// comment), so there is no stored stop to read back directly; this
    /// reconstructs "the closest one" for display purposes only — it never
    /// feeds back into the model.
    fn nearest(self, size: f32) -> TextSizeStop {
        let candidates = [
            (TextSizeStop::Small, self.small),
            (TextSizeStop::Medium, self.medium),
            (TextSizeStop::Large, self.large),
        ];
        candidates
            .into_iter()
            .min_by(|(_, a), (_, b)| {
                (a - size)
                    .abs()
                    .partial_cmp(&(b - size).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(stop, _)| stop)
            .unwrap_or(TextSizeStop::Medium)
    }
}

/// One placed annotation's geometry, in image-space. `Freehand`'s `points`
/// is the polyline as drawn — no smoothing/simplification, per the module
/// doc comment's performance section (spacing-limited at draw time instead).
///
/// **Stage 15 additions**: `Text`'s `position` is its **top-left** corner
/// (the same top-left-anchored convention `Rectangle`/`Ellipse` already use
/// for their own `rect`), matching how `saola-theme`'s own `text` widgets
/// anchor by default; `Step`'s `position` is its disc's **center** (badges
/// read more naturally click-centers-the-dot than click-is-the-corner).
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Arrow {
        start: Point,
        end: Point,
    },
    Rectangle {
        rect: Rectangle,
    },
    Ellipse {
        rect: Rectangle,
    },
    Freehand {
        points: Vec<Point>,
    },
    Text {
        position: Point,
        content: String,
        size: f32,
    },
    Step {
        position: Point,
        number: u32,
    },
}

/// The bounding box of a [`Shape`] — used for hit-test pruning and, after a
/// crop, to drop annotations that no longer overlap the canvas at all.
fn shape_bounds(shape: &Shape) -> Rectangle {
    match shape {
        Shape::Arrow { start, end } => rect_from_corners(*start, *end),
        Shape::Rectangle { rect } | Shape::Ellipse { rect } => *rect,
        Shape::Freehand { points } => {
            if points.is_empty() {
                return Rectangle {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 0.0,
                };
            }
            let mut min_x = f32::INFINITY;
            let mut min_y = f32::INFINITY;
            let mut max_x = f32::NEG_INFINITY;
            let mut max_y = f32::NEG_INFINITY;
            for point in points {
                min_x = min_x.min(point.x);
                min_y = min_y.min(point.y);
                max_x = max_x.max(point.x);
                max_y = max_y.max(point.y);
            }
            Rectangle {
                x: min_x,
                y: min_y,
                width: (max_x - min_x).max(0.0),
                height: (max_y - min_y).max(0.0),
            }
        }
        // Neither Text nor Step has real glyph metrics available in this
        // pure-geometry layer (that needs the font system — see
        // `raster_text`, which lives in the GUI-wrapper half below and
        // takes no part in hit-testing or crop-survival). Both bounds here
        // are therefore *estimates*, generous enough for select/crop-survival
        // purposes: `shape_hit`'s existing "click the bounding box, not the
        // exact ink" leniency (already true of Rectangle/Ellipse) absorbs
        // the imprecision.
        Shape::Text {
            position,
            content,
            size,
        } => {
            let width = (content.chars().count() as f32) * size * 0.6;
            Rectangle {
                x: position.x,
                y: position.y,
                width: width.max(1.0),
                height: (size * 1.3).max(1.0),
            }
        }
        Shape::Step { position, .. } => Rectangle {
            x: position.x - STEP_BADGE_RADIUS,
            y: position.y - STEP_BADGE_RADIUS,
            width: STEP_BADGE_RADIUS * 2.0,
            height: STEP_BADGE_RADIUS * 2.0,
        },
    }
}

fn translate_shape(shape: &Shape, dx: f32, dy: f32) -> Shape {
    match shape {
        Shape::Arrow { start, end } => Shape::Arrow {
            start: translate_point(*start, dx, dy),
            end: translate_point(*end, dx, dy),
        },
        Shape::Rectangle { rect } => Shape::Rectangle {
            rect: translate_rect(*rect, dx, dy),
        },
        Shape::Ellipse { rect } => Shape::Ellipse {
            rect: translate_rect(*rect, dx, dy),
        },
        Shape::Freehand { points } => Shape::Freehand {
            points: points.iter().map(|p| translate_point(*p, dx, dy)).collect(),
        },
        Shape::Text {
            position,
            content,
            size,
        } => Shape::Text {
            position: translate_point(*position, dx, dy),
            content: content.clone(),
            size: *size,
        },
        Shape::Step { position, number } => Shape::Step {
            position: translate_point(*position, dx, dy),
            number: *number,
        },
    }
}

/// `tolerance` (image-space px): a click has to land within this of an
/// arrow/freehand's *line*, or anywhere inside a rectangle/ellipse's
/// *bounding box* — the same "click the area, not the exact stroke" leniency
/// most vector editors give an outlined shape, and simpler than testing
/// against the ellipse's actual curve.
fn shape_hit(shape: &Shape, point: Point, tolerance: f32) -> bool {
    match shape {
        Shape::Arrow { start, end } => distance_to_segment(point, *start, *end) <= tolerance,
        Shape::Rectangle { rect } | Shape::Ellipse { rect } => {
            rect.expand(tolerance).contains(point)
        }
        Shape::Freehand { points } => points
            .windows(2)
            .any(|pair| distance_to_segment(point, pair[0], pair[1]) <= tolerance),
        Shape::Text { .. } | Shape::Step { .. } => {
            shape_bounds(shape).expand(tolerance).contains(point)
        }
    }
}

/// One placed annotation: its geometry plus the color/width it was drawn
/// with (each shape keeps its own — changing the current color picker never
/// retroactively repaints every earlier shape, only [`EditorModel::
/// set_color`]'s explicit "also update the selection" rule does that).
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub id: AnnotationId,
    pub shape: Shape,
    pub color: AnnotationColor,
    pub stroke_width: f32,
}

/// What the pointer is doing between a press and a release — ephemeral,
/// **never** part of an undo [`Snapshot`] (only a *committed* result is
/// undoable; see [`EditorModel::released`]).
#[derive(Debug, Clone, PartialEq)]
enum Interaction {
    Idle,
    /// Dragging out a new Arrow/Rectangle/Ellipse from a fixed anchor.
    DrawingLine {
        tool: LineTool,
        anchor: Point,
        current: Point,
    },
    /// Dragging out a new Freehand stroke.
    DrawingFreehand {
        points: Vec<Point>,
    },
    /// Moving the annotation `id`. `origin` is its shape as of the press
    /// (unchanged, for the undo comparison); `preview` is where it would
    /// land if released right now, recomputed fresh from `origin` on every
    /// motion (never accumulated), matching `modules::overlay::Interaction::
    /// Moving`'s own "never drift" reasoning.
    Moving {
        id: AnnotationId,
        grab: Point,
        origin: Shape,
        preview: Shape,
    },
    /// Dragging out the Crop tool's pending rectangle. Not committed until
    /// [`EditorModel::apply_crop`] — see the module doc comment.
    Cropping {
        anchor: Point,
        current: Point,
    },
    /// Dragging out a Blur/Pixelate region. Unlike `Cropping`, there is no
    /// pending/Apply step — [`EditorModel::released`] applies the kernel the
    /// instant the drag ends (see [`EditorModel::apply_redaction`]'s doc
    /// comment for why).
    Redacting {
        kind: RedactKind,
        anchor: Point,
        current: Point,
    },
}

/// One undo/redo step: the whole document as it was *before* the action
/// that pushed it. `canvas` is an `Arc<Frame>` clone (O(1) — the multi-
/// megabyte pixel buffer is shared, not copied, until a crop actually
/// produces a new one); `annotations` is a plain `Vec` clone, cheap because
/// annotation counts are small (a handful to a few dozen, never image-sized).
#[derive(Clone)]
struct Snapshot {
    canvas: Arc<Frame>,
    annotations: Vec<Annotation>,
}

/// The whole document: base pixels, placed annotations, current tool/color/
/// width defaults, in-flight interaction, undo/redo. No `Theme`, no iced
/// widget type beyond `Point`/`Rectangle` (plain geometry, not rendering) —
/// see the module doc comment's "tool-state model" section.
#[derive(Debug, Clone)]
pub struct EditorModel {
    canvas: Arc<Frame>,
    annotations: Vec<Annotation>,
    next_id: AnnotationId,
    /// The next Step badge's number — monotonic like `next_id`, never reused
    /// after a delete (see [`Tool::Step`]'s doc comment: renumbering on
    /// delete is real added complexity this stage's task list doesn't ask
    /// for, the same "explicit over clever" call CLAUDE.md's conventions
    /// make elsewhere).
    next_step: u32,
    tool: Tool,
    current_color: AnnotationColor,
    current_width: StrokeWidth,
    /// The Text tool's current size, already resolved to a plain `f32` by
    /// [`EditorState`] (see [`TextSizeScale`]) — never a [`TextSizeStop`],
    /// mirroring `current_color`/`current_width`'s own "the model only
    /// stores resolved values" shape.
    current_text_size: f32,
    selected: Option<AnnotationId>,
    interaction: Interaction,
    pending_crop: Option<Rectangle>,
    /// The last image-space point either tool reported, for
    /// [`EditorCanvas`]'s "the OS delivered a release outside the canvas
    /// widget's bounds" fallback — see that type's doc comment.
    pointer: Option<Point>,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Snapshot {{ canvas: {:?}, {} annotation(s) }}",
            self.canvas,
            self.annotations.len()
        )
    }
}

impl EditorModel {
    pub fn new(canvas: Frame) -> Self {
        EditorModel {
            canvas: Arc::new(canvas),
            annotations: Vec::new(),
            next_id: 0,
            next_step: 1,
            tool: Tool::Select,
            current_color: AnnotationColor::Terracotta,
            current_width: StrokeWidth::Medium,
            // Overwritten by `EditorState::load` via `set_text_size` with a
            // real `TextSizeScale::resolve` value the moment a `Theme` is
            // available; this literal only matters for tests that construct
            // an `EditorModel` directly with no theme at all.
            current_text_size: 20.0,
            selected: None,
            interaction: Interaction::Idle,
            pending_crop: None,
            pointer: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn canvas(&self) -> &Frame {
        &self.canvas
    }

    fn canvas_arc(&self) -> Arc<Frame> {
        Arc::clone(&self.canvas)
    }

    pub fn annotations(&self) -> &[Annotation] {
        &self.annotations
    }

    pub fn tool(&self) -> Tool {
        self.tool
    }

    pub fn current_color(&self) -> AnnotationColor {
        self.current_color
    }

    pub fn current_width(&self) -> StrokeWidth {
        self.current_width
    }

    pub fn selected(&self) -> Option<AnnotationId> {
        self.selected
    }

    pub fn pending_crop(&self) -> Option<Rectangle> {
        self.pending_crop
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    fn width(&self) -> f32 {
        self.canvas.width() as f32
    }

    fn height(&self) -> f32 {
        self.canvas.height() as f32
    }

    /// Switching **away from** Crop discards any undragged pending
    /// rectangle (there is nothing to remember it for); switching **away
    /// from** Select clears the selection (every other tool draws new
    /// shapes, and a stale selection highlight would be confusing chrome
    /// with no button left that acts on it).
    pub fn set_tool(&mut self, tool: Tool) {
        if self.tool == Tool::Crop && tool != Tool::Crop {
            self.pending_crop = None;
        }
        if tool != Tool::Select {
            self.selected = None;
        }
        self.tool = tool;
        self.interaction = Interaction::Idle;
    }

    /// Sets the color new shapes are drawn with, **and** — if something is
    /// selected — repaints that selection immediately. Both effects from one
    /// control: "change the picker" and "recolor what I've got selected" are
    /// the same gesture in most annotation tools, and splitting them into
    /// two controls would be a control this app doesn't need.
    pub fn set_color(&mut self, color: AnnotationColor) {
        self.current_color = color;
        if let Some(id) = self.selected {
            if let Some(annotation) = self.annotation_mut(id) {
                annotation.color = color;
            }
        }
    }

    pub fn set_width(&mut self, width: StrokeWidth) {
        self.current_width = width;
        if let Some(id) = self.selected {
            if let Some(annotation) = self.annotation_mut(id) {
                annotation.stroke_width = width.pixels();
            }
        }
    }

    /// Sets the Text tool's current size (a resolved `f32` — see
    /// [`TextSizeScale`]) **and** — if the current selection is a Text
    /// annotation — resizes it immediately, the same "change the picker,
    /// also restyle what's selected" gesture [`Self::set_color`]/
    /// [`Self::set_width`] already give every other shape. Like those two,
    /// this does **not** push an undo entry — see their doc comments'
    /// shared "no undo for a picker restyle" posture, unchanged here.
    pub fn set_text_size(&mut self, size: f32) {
        let size = size.max(1.0);
        self.current_text_size = size;
        if let Some(id) = self.selected {
            if let Some(annotation) = self.annotation_mut(id) {
                if let Shape::Text {
                    size: shape_size, ..
                } = &mut annotation.shape
                {
                    *shape_size = size;
                }
            }
        }
    }

    /// Replaces the selected Text annotation's content, a no-op if nothing
    /// selected is a Text annotation — the toolbar only ever shows the field
    /// this message comes from when [`Self::selected_text`] is `Some`, so
    /// the no-op case is a defensive default, not an expected path. No undo
    /// entry per keystroke, same reasoning as [`Self::set_text_size`].
    pub fn set_selected_text_content(&mut self, content: String) {
        if let Some(id) = self.selected {
            if let Some(annotation) = self.annotation_mut(id) {
                if let Shape::Text {
                    content: shape_content,
                    ..
                } = &mut annotation.shape
                {
                    *shape_content = content;
                }
            }
        }
    }

    /// The selected annotation's text content and size, if it's a Text
    /// annotation — what the toolbar checks to decide whether to show the
    /// content/size editing controls at all.
    pub fn selected_text(&self) -> Option<(&str, f32)> {
        let id = self.selected?;
        match &self.annotation(id)?.shape {
            Shape::Text { content, size, .. } => Some((content.as_str(), *size)),
            _ => None,
        }
    }

    pub fn pointer_moved(&mut self, point: Point) {
        let point = clamp_to_canvas(point, self.width(), self.height());
        self.pointer = Some(point);
        self.track(point);
    }

    /// The shared "the pointer is now at `point`, update whatever's in
    /// flight" step [`pointer_moved`](Self::pointer_moved) and
    /// [`released`](Self::released) both need — a release should reflect the
    /// exact release position, not whatever the last `CursorMoved` sample
    /// happened to be.
    fn track(&mut self, point: Point) {
        match &mut self.interaction {
            Interaction::Idle => {}
            Interaction::DrawingLine { current, .. } => *current = point,
            Interaction::DrawingFreehand { points } => {
                let far_enough = points
                    .last()
                    .is_none_or(|last| distance(*last, point) >= MIN_FREEHAND_SPACING);
                if far_enough {
                    points.push(point);
                }
            }
            Interaction::Moving {
                grab,
                origin,
                preview,
                ..
            } => {
                *preview = translate_shape(origin, point.x - grab.x, point.y - grab.y);
            }
            Interaction::Cropping { current, .. } => *current = point,
            Interaction::Redacting { current, .. } => *current = point,
        }
    }

    pub fn pressed(&mut self, point: Point) {
        let point = clamp_to_canvas(point, self.width(), self.height());
        self.pointer = Some(point);
        match self.tool {
            Tool::Select => match self.hit_test(point) {
                Some(id) => {
                    self.selected = Some(id);
                    if let Some(annotation) = self.annotation(id) {
                        let origin = annotation.shape.clone();
                        self.interaction = Interaction::Moving {
                            id,
                            grab: point,
                            preview: origin.clone(),
                            origin,
                        };
                    }
                }
                None => {
                    self.selected = None;
                    self.interaction = Interaction::Idle;
                }
            },
            Tool::Crop => {
                self.interaction = Interaction::Cropping {
                    anchor: point,
                    current: point,
                };
            }
            Tool::Arrow => {
                self.interaction = Interaction::DrawingLine {
                    tool: LineTool::Arrow,
                    anchor: point,
                    current: point,
                };
            }
            Tool::Rectangle => {
                self.interaction = Interaction::DrawingLine {
                    tool: LineTool::Rectangle,
                    anchor: point,
                    current: point,
                };
            }
            Tool::Ellipse => {
                self.interaction = Interaction::DrawingLine {
                    tool: LineTool::Ellipse,
                    anchor: point,
                    current: point,
                };
            }
            Tool::Freehand => {
                self.interaction = Interaction::DrawingFreehand {
                    points: vec![point],
                };
            }
            // Text/Step commit on a plain press, not a drag — see `Tool`'s
            // own doc comment. `commit_new_annotation` already pushes undo
            // and selects the new annotation, which is what makes the
            // toolbar's content/size controls appear immediately after this
            // click (they're keyed off "is a Text annotation selected").
            Tool::Text => {
                self.commit_new_annotation(Shape::Text {
                    position: point,
                    content: String::new(),
                    size: self.current_text_size,
                });
                self.interaction = Interaction::Idle;
            }
            Tool::Step => {
                let number = self.next_step;
                self.next_step += 1;
                self.commit_new_annotation(Shape::Step {
                    position: point,
                    number,
                });
                self.interaction = Interaction::Idle;
            }
            Tool::Blur => {
                self.interaction = Interaction::Redacting {
                    kind: RedactKind::Blur,
                    anchor: point,
                    current: point,
                };
            }
            Tool::Pixelate => {
                self.interaction = Interaction::Redacting {
                    kind: RedactKind::Pixelate,
                    anchor: point,
                    current: point,
                };
            }
        }
    }

    /// Returns `true` if the **canvas pixels themselves** changed (a
    /// committed Blur/Pixelate — see [`Self::apply_redaction`]) — every
    /// other arm only ever changes the `annotations` vector, which
    /// [`EditorCanvas`]'s vector overlay redraws live with no separate
    /// refresh. Callers (`EditorState::update`'s `Message::Released` arm)
    /// use this to decide whether the cached
    /// [`EditorState::display_handle`] needs rebuilding — the same signal
    /// `Self::apply_crop`'s `bool` return already gives its own caller.
    pub fn released(&mut self, point: Point) -> bool {
        let point = clamp_to_canvas(point, self.width(), self.height());
        self.pointer = Some(point);
        self.track(point);

        let mut canvas_changed = false;
        match std::mem::replace(&mut self.interaction, Interaction::Idle) {
            Interaction::Idle => {}
            Interaction::DrawingLine {
                tool,
                anchor,
                current,
            } => {
                if distance(anchor, current) >= MIN_SHAPE_SIZE {
                    self.commit_new_annotation(line_tool_shape(tool, anchor, current));
                }
            }
            Interaction::DrawingFreehand { points } => {
                if points.len() >= 2 {
                    self.commit_new_annotation(Shape::Freehand { points });
                }
            }
            Interaction::Moving {
                id,
                origin,
                preview,
                ..
            } => {
                if preview != origin {
                    self.push_undo();
                    if let Some(annotation) = self.annotation_mut(id) {
                        annotation.shape = preview;
                    }
                }
                // Unchanged (a plain click): the selection stays, nothing
                // else does — no undo entry for a no-op.
            }
            Interaction::Cropping { anchor, current } => {
                let rect = rect_from_corners(anchor, current);
                self.pending_crop = if rect.width >= MIN_CROP_SIZE && rect.height >= MIN_CROP_SIZE {
                    Some(rect)
                } else {
                    None
                };
            }
            Interaction::Redacting {
                kind,
                anchor,
                current,
            } => {
                let rect = rect_from_corners(anchor, current);
                if rect.width >= MIN_CROP_SIZE && rect.height >= MIN_CROP_SIZE {
                    canvas_changed = self.apply_redaction(kind, rect);
                }
                // Too small to be a deliberate drag: discarded, same
                // `MIN_CROP_SIZE`-gated leniency `Cropping` already gives a
                // stray click — no redaction, no undo entry.
            }
        }
        canvas_changed
    }

    fn commit_new_annotation(&mut self, shape: Shape) {
        self.push_undo();
        let id = self.next_id;
        self.next_id += 1;
        self.annotations.push(Annotation {
            id,
            shape,
            color: self.current_color,
            stroke_width: self.current_width.pixels(),
        });
        self.selected = Some(id);
    }

    pub fn delete_selected(&mut self) {
        let Some(id) = self.selected else { return };
        if let Some(index) = self.annotations.iter().position(|a| a.id == id) {
            self.push_undo();
            self.annotations.remove(index);
            self.selected = None;
        }
    }

    /// Commits [`Self::pending_crop`]: the canvas shrinks to that rectangle
    /// (via [`Frame::crop`] — already unit-tested clamping and buffer math,
    /// reused rather than duplicated) and every annotation's coordinates
    /// shift to match the new origin. Annotations that no longer overlap the
    /// cropped canvas at all are dropped (a bounding-box test, not a partial
    /// clip — a shape half inside the new bounds keeps its full geometry and
    /// simply renders truncated by every `paint_*` function's own bounds
    /// check). Returns `false` (a no-op) if there is nothing pending or the
    /// rectangle doesn't overlap the canvas.
    pub fn apply_crop(&mut self) -> bool {
        let Some(rect) = self.pending_crop else {
            return false;
        };
        let pixel_rect = PixelRect {
            x: rect.x.round().max(0.0) as u32,
            y: rect.y.round().max(0.0) as u32,
            width: rect.width.round().max(1.0) as u32,
            height: rect.height.round().max(1.0) as u32,
        };
        let Some(cropped) = self.canvas.crop(pixel_rect) else {
            return false;
        };

        self.push_undo();

        let dx = -(pixel_rect.x as f32);
        let dy = -(pixel_rect.y as f32);
        let new_bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: cropped.width() as f32,
            height: cropped.height() as f32,
        };
        self.annotations = self
            .annotations
            .iter()
            .map(|annotation| Annotation {
                shape: translate_shape(&annotation.shape, dx, dy),
                ..annotation.clone()
            })
            .filter(|annotation| rects_intersect(shape_bounds(&annotation.shape), new_bounds))
            .collect();

        self.canvas = Arc::new(cropped);
        self.selected = None;
        self.pending_crop = None;
        self.interaction = Interaction::Idle;
        true
    }

    pub fn cancel_crop(&mut self) {
        self.pending_crop = None;
        self.interaction = Interaction::Idle;
    }

    /// Applies a Blur/Pixelate kernel directly to `self.canvas`'s pixels
    /// within `rect`, in image-space — **immediately**, on the same
    /// `released()` call that ends the drag, with no separate Apply step
    /// the way [`Self::apply_crop`] has one.
    ///
    /// # Why this isn't Crop's two-step pattern (teaching note)
    ///
    /// Crop needs confirmation because it changes the canvas's *bounds* —
    /// every annotation's coordinates shift, some get dropped outright, and
    /// undoing it after a few more edits means reconstructing a scroll of
    /// intermediate state the user may not expect. A redaction changes pixel
    /// *content* only, at a size and position the drag itself already made
    /// visible via the same dimmed-rectangle preview Crop uses
    /// (`draw_region_dimming`) — the drag itself *is* the confirmation, and
    /// [`Self::undo`] reverses it exactly like any other committed action
    /// (arrow, move, delete). PLAN.md's own Stage 15 task list agrees:
    /// "stay editable in-session via the undo stack" — the undo stack, not
    /// select/move/delete and not a pending/apply dance.
    ///
    /// # Why this destroys the original pixels for real (the redaction promise)
    ///
    /// This is not a translucent shape drawn *over* the image at compose
    /// time the way every other annotation is — if it were, the original
    /// pixels would still be sitting right there in `self.canvas`,
    /// recoverable by deleting the annotation or by anyone who obtained the
    /// undo history. Instead, this method reads `self.canvas`'s pixels once,
    /// **replaces** them in `rect` with the kernel's output, and stores the
    /// result as the new `self.canvas`. From that point on, the original
    /// content in `rect` exists only in the undo stack's earlier
    /// `Snapshot` — which lives in this process's memory for this editing
    /// session and is never written to disk. A Saved/Copied file, and any
    /// later `Snapshot` once undo history is pruned or the process exits,
    /// has no path back to the original pixels. That is CLAUDE.md's
    /// Boundaries promise ("blur/pixelate must be irreversible in exported
    /// files") made concrete: irreversibility is a property of what gets
    /// written to disk, not of what a live, undo-capable editing session
    /// remembers while it's still open.
    ///
    /// Returns `false` (nothing changed) if `rect` doesn't overlap the
    /// canvas at all — the same "no sensible answer" case
    /// [`crate::capture::Frame::crop`] already handles for the same reason.
    fn apply_redaction(&mut self, kind: RedactKind, rect: Rectangle) -> bool {
        let width = self.canvas.width();
        let height = self.canvas.height();
        let Some(pixel_rect) = redact_pixel_rect(rect, width, height) else {
            return false;
        };
        let mut pixels = self.canvas.pixels().to_vec();
        match kind {
            RedactKind::Blur => blur_region(&mut pixels, width, height, pixel_rect, BLUR_RADIUS_PX),
            RedactKind::Pixelate => {
                pixelate_region(&mut pixels, width, height, pixel_rect, PIXELATE_BLOCK_PX)
            }
        }
        let Some(new_frame) = Frame::new(width, height, self.canvas.scale(), pixels) else {
            return false;
        };
        self.push_undo();
        self.canvas = Arc::new(new_frame);
        true
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(Snapshot {
            canvas: self.canvas_arc(),
            annotations: self.annotations.clone(),
        });
        self.redo_stack.clear();
    }

    pub fn undo(&mut self) {
        let Some(previous) = self.undo_stack.pop() else {
            return;
        };
        self.redo_stack.push(Snapshot {
            canvas: self.canvas_arc(),
            annotations: self.annotations.clone(),
        });
        self.canvas = previous.canvas;
        self.annotations = previous.annotations;
        self.selected = None;
        self.pending_crop = None;
        self.interaction = Interaction::Idle;
    }

    pub fn redo(&mut self) {
        let Some(next) = self.redo_stack.pop() else {
            return;
        };
        self.undo_stack.push(Snapshot {
            canvas: self.canvas_arc(),
            annotations: self.annotations.clone(),
        });
        self.canvas = next.canvas;
        self.annotations = next.annotations;
        self.selected = None;
        self.pending_crop = None;
        self.interaction = Interaction::Idle;
    }

    fn annotation(&self, id: AnnotationId) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.id == id)
    }

    fn annotation_mut(&mut self, id: AnnotationId) -> Option<&mut Annotation> {
        self.annotations.iter_mut().find(|a| a.id == id)
    }

    /// Topmost first: the last-drawn annotation is drawn on top, so a click
    /// under an overlap should grab what's visibly on top, not the oldest
    /// shape underneath it.
    fn hit_test(&self, point: Point) -> Option<AnnotationId> {
        self.annotations
            .iter()
            .rev()
            .find(|a| shape_hit(&a.shape, point, SELECT_TOLERANCE))
            .map(|a| a.id)
    }
}

// ---------------------------------------------------------------------
// Raster compositing — pure, unit-tested, run only at save/copy time.
// See the module doc comment for why this exists alongside the vector
// interactive painter below rather than replacing it.
// ---------------------------------------------------------------------

/// Paints every annotation over a copy of `base`'s pixels and returns the
/// composed [`Frame`]. `base` itself is never mutated. `font_family` is the
/// theme's UI face (`saola_theme::Theme::typography.family_ui`, "IBM Plex
/// Sans" in the built-in theme) — threaded through to [`paint_shape`]'s
/// Text/Step arms, the only two that need a font at all.
fn compose(
    base: &Frame,
    annotations: &[Annotation],
    palette: ColorPalette,
    font_family: &str,
) -> Frame {
    let width = base.width();
    let height = base.height();
    let mut pixels = base.pixels().to_vec();
    for annotation in annotations {
        paint_shape(&mut pixels, width, height, annotation, palette, font_family);
    }
    // `pixels` is `base.pixels().to_vec()` painted in place: its length
    // never changes, so `Frame::new` cannot fail here except in a way that
    // would also have meant `base` itself was already invalid. Falling back
    // to the unpainted base rather than panicking is the no-panic rule's
    // answer to a case that should be unreachable in practice.
    Frame::new(width, height, base.scale(), pixels).unwrap_or_else(|| base.clone())
}

/// Takes the whole `Annotation` (not just its `Shape`) — Stage 14's original
/// signature took a pre-resolved `color`, which worked while every shape
/// used `annotation.color`; `Step`'s badge is fixed terracotta-disc/
/// ivory-numeral regardless of the color picker (see `Tool::Step`'s doc
/// comment), so this needs the whole [`ColorPalette`], not one color
/// resolved from it ahead of time.
fn paint_shape(
    buf: &mut [u8],
    width: u32,
    height: u32,
    annotation: &Annotation,
    palette: ColorPalette,
    font_family: &str,
) {
    let color = annotation.color.resolve(palette);
    let stroke_width = annotation.stroke_width;
    match &annotation.shape {
        Shape::Arrow { start, end } => {
            paint_arrow(buf, width, height, *start, *end, color, stroke_width)
        }
        Shape::Rectangle { rect } => {
            paint_rect_outline(buf, width, height, *rect, color, stroke_width)
        }
        Shape::Ellipse { rect } => {
            paint_ellipse_outline(buf, width, height, *rect, color, stroke_width)
        }
        Shape::Freehand { points } => {
            paint_polyline(buf, width, height, points, color, stroke_width)
        }
        Shape::Text {
            position,
            content,
            size,
        } => raster_text(
            buf,
            width,
            height,
            TextRasterRequest {
                content,
                family: font_family,
                size_px: *size,
                color,
                position: *position,
                align: TextRasterAlign::TopLeft,
            },
        ),
        Shape::Step { position, number } => {
            paint_step_badge(buf, width, height, *position, *number, palette, font_family)
        }
    }
}

/// A numbered-step badge: a filled terracotta disc plus a centered ivory
/// numeral — deliberately **not** `annotation.color`-driven (see `Tool::
/// Step`'s doc comment: this is a fixed two-tone badge, the same "solid
/// icon" posture CLAUDE.md's design language reserves for record/stop/play,
/// not a customizable shape like Arrow/Rectangle/Ellipse/Freehand/Text).
fn paint_step_badge(
    buf: &mut [u8],
    width: u32,
    height: u32,
    position: Point,
    number: u32,
    palette: ColorPalette,
    font_family: &str,
) {
    fill_circle(
        buf,
        width,
        height,
        position,
        STEP_BADGE_RADIUS,
        palette.terracotta,
    );
    let label = number.to_string();
    let font_size = STEP_BADGE_RADIUS * STEP_BADGE_FONT_RATIO;
    raster_text(
        buf,
        width,
        height,
        TextRasterRequest {
            content: &label,
            family: font_family,
            size_px: font_size,
            color: palette.ivory,
            position,
            align: TextRasterAlign::Center,
        },
    );
}

/// Blends `color` into the pixel at `(x, y)`, a no-op if that's outside the
/// buffer. Straight src-over alpha blend; every [`AnnotationColor`] this
/// stage ships is fully opaque (`a == 255`), which takes the fast "just
/// overwrite" path, but the blended path is exercised and tested too so a
/// future translucent color (Stage 15+?) isn't the first thing to find a bug
/// here.
fn blend_pixel(
    buf: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    color: saola_theme::tokens::Color,
) {
    if x < 0 || y < 0 || x as u32 >= width || y as u32 >= height {
        return;
    }
    let stride = width as usize * 4;
    let index = y as usize * stride + x as usize * 4;
    let Some(pixel) = buf.get_mut(index..index + 4) else {
        return;
    };
    if color.a == 0 {
        return;
    }
    if color.a == 255 {
        pixel[0] = color.r;
        pixel[1] = color.g;
        pixel[2] = color.b;
        pixel[3] = 255;
        return;
    }
    let alpha = f32::from(color.a) / 255.0;
    let blend =
        |src: u8, dst: u8| (f32::from(src) * alpha + f32::from(dst) * (1.0 - alpha)).round() as u8;
    pixel[0] = blend(color.r, pixel[0]);
    pixel[1] = blend(color.g, pixel[1]);
    pixel[2] = blend(color.b, pixel[2]);
    pixel[3] = 255;
}

/// Paints every pixel within `radius` of the segment `a`-`b` — a thick line
/// with naturally rounded ends (a point within `radius` of a segment's
/// *endpoint* is within `radius` of the segment itself, so the capsule shape
/// falls out of [`distance_to_segment`] for free, no separate cap logic
/// needed).
fn paint_segment_capsule(
    buf: &mut [u8],
    width: u32,
    height: u32,
    a: Point,
    b: Point,
    radius: f32,
    color: saola_theme::tokens::Color,
) {
    if radius <= 0.0 {
        return;
    }
    let min_x = (a.x.min(b.x) - radius).floor().max(0.0) as i32;
    let max_x = ((a.x.max(b.x) + radius).ceil() as i32).min(width as i32);
    let min_y = (a.y.min(b.y) - radius).floor().max(0.0) as i32;
    let max_y = ((a.y.max(b.y) + radius).ceil() as i32).min(height as i32);
    for y in min_y..max_y {
        for x in min_x..max_x {
            let sample = Point::new(x as f32 + 0.5, y as f32 + 0.5);
            if distance_to_segment(sample, a, b) <= radius {
                blend_pixel(buf, width, height, x, y, color);
            }
        }
    }
}

fn fill_circle(
    buf: &mut [u8],
    width: u32,
    height: u32,
    center: Point,
    radius: f32,
    color: saola_theme::tokens::Color,
) {
    if radius <= 0.0 {
        return;
    }
    let min_x = (center.x - radius).floor().max(0.0) as i32;
    let max_x = ((center.x + radius).ceil() as i32).min(width as i32);
    let min_y = (center.y - radius).floor().max(0.0) as i32;
    let max_y = ((center.y + radius).ceil() as i32).min(height as i32);
    for y in min_y..max_y {
        for x in min_x..max_x {
            let sample = Point::new(x as f32 + 0.5, y as f32 + 0.5);
            if distance(sample, center) <= radius {
                blend_pixel(buf, width, height, x, y, color);
            }
        }
    }
}

/// A freehand stroke: one capsule per consecutive pair of points, plus a
/// filled circle at each interior vertex so a sharp turn doesn't show a
/// notch between two capsules (each capsule alone only rounds its own two
/// ends, not the angle between segments).
fn paint_polyline(
    buf: &mut [u8],
    width: u32,
    height: u32,
    points: &[Point],
    color: saola_theme::tokens::Color,
    stroke_width: f32,
) {
    let radius = (stroke_width / 2.0).max(MIN_STROKE_RADIUS);
    for pair in points.windows(2) {
        paint_segment_capsule(buf, width, height, pair[0], pair[1], radius, color);
    }
    if points.len() > 2 {
        for point in &points[1..points.len() - 1] {
            fill_circle(buf, width, height, *point, radius, color);
        }
    }
}

/// A rectangle's outline: every pixel inside `rect` but outside `rect`
/// shrunk by `stroke_width` on every side. If the stroke is thick enough to
/// consume the whole rectangle, the shrunk "inner" rectangle collapses to a
/// non-positive size and `Rectangle::contains` never matches it — the ring
/// degrades to a filled rectangle rather than doing anything wrong.
fn paint_rect_outline(
    buf: &mut [u8],
    width: u32,
    height: u32,
    rect: Rectangle,
    color: saola_theme::tokens::Color,
    stroke_width: f32,
) {
    let stroke = stroke_width.max(MIN_STROKE_RADIUS * 2.0);
    let inner = rect.shrink(stroke);
    let min_x = rect.x.floor().max(0.0) as i32;
    let max_x = ((rect.x + rect.width).ceil() as i32).min(width as i32);
    let min_y = rect.y.floor().max(0.0) as i32;
    let max_y = ((rect.y + rect.height).ceil() as i32).min(height as i32);
    for y in min_y..max_y {
        for x in min_x..max_x {
            let sample = Point::new(x as f32 + 0.5, y as f32 + 0.5);
            if rect.contains(sample) && !inner.contains(sample) {
                blend_pixel(buf, width, height, x, y, color);
            }
        }
    }
}

/// An ellipse's outline, via the implicit "is this point inside the ellipse"
/// test (`(x/a)² + (y/b)² <= 1`) at the outer radii and, when they're
/// positive, again at the inner (stroke-shrunk) radii — the same
/// ring-by-subtraction idea [`paint_rect_outline`] uses, adapted to a curve.
fn paint_ellipse_outline(
    buf: &mut [u8],
    width: u32,
    height: u32,
    rect: Rectangle,
    color: saola_theme::tokens::Color,
    stroke_width: f32,
) {
    let radius_x = (rect.width / 2.0).max(MIN_STROKE_RADIUS);
    let radius_y = (rect.height / 2.0).max(MIN_STROKE_RADIUS);
    let center = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    let stroke = stroke_width.max(MIN_STROKE_RADIUS * 2.0);
    let inner_radius_x = (radius_x - stroke).max(0.0);
    let inner_radius_y = (radius_y - stroke).max(0.0);

    let min_x = rect.x.floor().max(0.0) as i32;
    let max_x = ((rect.x + rect.width).ceil() as i32).min(width as i32);
    let min_y = rect.y.floor().max(0.0) as i32;
    let max_y = ((rect.y + rect.height).ceil() as i32).min(height as i32);

    for y in min_y..max_y {
        for x in min_x..max_x {
            let sample_x = x as f32 + 0.5;
            let sample_y = y as f32 + 0.5;
            let outer = ((sample_x - center.x) / radius_x).powi(2)
                + ((sample_y - center.y) / radius_y).powi(2);
            if outer > 1.0 {
                continue;
            }
            let inside_inner = inner_radius_x > 0.0
                && inner_radius_y > 0.0
                && ((sample_x - center.x) / inner_radius_x).powi(2)
                    + ((sample_y - center.y) / inner_radius_y).powi(2)
                    <= 1.0;
            if !inside_inner {
                blend_pixel(buf, width, height, x, y, color);
            }
        }
    }
}

/// An arrow: a capsule shaft plus a filled triangular head at `end`, sized
/// off `stroke_width` so a thicker arrow gets a proportionally bigger head.
/// A degenerate (near-zero-length) arrow paints a single dot instead of
/// nothing — the honest rendering of "the user pressed and released in
/// about the same spot," not a silently dropped annotation.
fn paint_arrow(
    buf: &mut [u8],
    width: u32,
    height: u32,
    start: Point,
    end: Point,
    color: saola_theme::tokens::Color,
    stroke_width: f32,
) {
    let radius = (stroke_width / 2.0).max(MIN_STROKE_RADIUS);
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length = (dx * dx + dy * dy).sqrt();
    if length < 0.5 {
        fill_circle(buf, width, height, end, radius, color);
        return;
    }
    let ux = dx / length;
    let uy = dy / length;
    let head_length = (stroke_width * 4.0).max(12.0).min(length);
    let head_width = head_length * 0.6;

    let shaft_end = Point::new(
        end.x - ux * head_length * 0.6,
        end.y - uy * head_length * 0.6,
    );
    paint_segment_capsule(buf, width, height, start, shaft_end, radius, color);

    let base_center = Point::new(end.x - ux * head_length, end.y - uy * head_length);
    let perp_x = -uy;
    let perp_y = ux;
    let base1 = Point::new(
        base_center.x + perp_x * head_width / 2.0,
        base_center.y + perp_y * head_width / 2.0,
    );
    let base2 = Point::new(
        base_center.x - perp_x * head_width / 2.0,
        base_center.y - perp_y * head_width / 2.0,
    );
    fill_triangle(buf, width, height, end, base1, base2, color);
}

/// Fills the triangle `a`-`b`-`c` via the standard edge-function test
/// (barycentric sign consistency) — works for either winding order, which
/// matters here because [`paint_arrow`]'s `base1`/`base2` order depends on
/// the arrow's own direction.
fn fill_triangle(
    buf: &mut [u8],
    width: u32,
    height: u32,
    a: Point,
    b: Point,
    c: Point,
    color: saola_theme::tokens::Color,
) {
    let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as i32;
    let max_x = (a.x.max(b.x).max(c.x).ceil() as i32).min(width as i32);
    let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as i32;
    let max_y = (a.y.max(b.y).max(c.y).ceil() as i32).min(height as i32);

    let edge = |p1: Point, p2: Point, p: Point| {
        (p2.x - p1.x) * (p.y - p1.y) - (p2.y - p1.y) * (p.x - p1.x)
    };
    if edge(a, b, c).abs() < f32::EPSILON {
        return;
    }

    for y in min_y..max_y {
        for x in min_x..max_x {
            let sample = Point::new(x as f32 + 0.5, y as f32 + 0.5);
            let w0 = edge(b, c, sample);
            let w1 = edge(c, a, sample);
            let w2 = edge(a, b, sample);
            let has_neg = w0 < 0.0 || w1 < 0.0 || w2 < 0.0;
            let has_pos = w0 > 0.0 || w1 > 0.0 || w2 > 0.0;
            if !(has_neg && has_pos) {
                blend_pixel(buf, width, height, x, y, color);
            }
        }
    }
}

// ---------------------------------------------------------------------
// Redaction kernels — Blur/Pixelate, run once per drag (see
// `EditorModel::apply_redaction`), pure `&mut [u8]` operations exactly like
// the `paint_*` functions above, and unit-tested the same way with small
// synthetic buffers. Neither kernel touches `saola-theme` or any color
// token — a redaction transforms existing pixels, it doesn't paint new ones.
// ---------------------------------------------------------------------

/// Clamps a (possibly sub-pixel, possibly partly off-canvas) view-space
/// [`Rectangle`] into a [`PixelRect`] that is guaranteed to lie fully inside
/// `0..width, 0..height` — the redaction-kernel counterpart to
/// [`EditorModel::apply_crop`]'s own `pixel_rect` construction. Returns
/// `None` if the rectangle doesn't overlap the canvas at all (matching
/// `crate::capture::Frame::crop`'s own "no sensible answer" case).
fn redact_pixel_rect(rect: Rectangle, width: u32, height: u32) -> Option<PixelRect> {
    let width_f = width as f32;
    let height_f = height as f32;
    let x0 = rect.x.max(0.0).min(width_f);
    let y0 = rect.y.max(0.0).min(height_f);
    let x1 = (rect.x + rect.width).max(0.0).min(width_f);
    let y1 = (rect.y + rect.height).max(0.0).min(height_f);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(PixelRect {
        x: x0.round() as u32,
        y: y0.round() as u32,
        width: (x1 - x0).round().max(1.0) as u32,
        height: (y1 - y0).round().max(1.0) as u32,
    })
}

/// Mosaics `rect` into `block_size`×`block_size` tiles, each replaced by the
/// average color of the pixels it covers. `O(rect area)`: every pixel is
/// read once (to accumulate its block's sum) and written once (the block's
/// average), regardless of `block_size` — there's no per-block second pass
/// over already-visited pixels.
/// The per-channel average of `sum` over `count` samples, or `None` if
/// `count` is zero (an empty block — only possible if `rect` clipped a
/// block down to nothing, per [`pixelate_region`]'s own bounds checks).
/// `checked_div` rather than a bare `if count > 0 { sum / count }` guard, so
/// the "don't divide by zero" contract is visible in the type instead of
/// relying on the caller reading the surrounding `if`.
fn checked_average(sum: [u64; 4], count: u64) -> Option<[u8; 4]> {
    Some([
        sum[0].checked_div(count)? as u8,
        sum[1].checked_div(count)? as u8,
        sum[2].checked_div(count)? as u8,
        sum[3].checked_div(count)? as u8,
    ])
}

fn pixelate_region(buf: &mut [u8], width: u32, height: u32, rect: PixelRect, block_size: u32) {
    let block_size = block_size.max(1);
    let x_end = rect.x.saturating_add(rect.width).min(width);
    let y_end = rect.y.saturating_add(rect.height).min(height);
    let stride = width as usize * 4;

    let mut block_y = rect.y;
    while block_y < y_end {
        let block_h = block_size.min(y_end - block_y);
        let mut block_x = rect.x;
        while block_x < x_end {
            let block_w = block_size.min(x_end - block_x);

            let mut sum = [0u64; 4];
            let mut count = 0u64;
            for y in block_y..block_y + block_h {
                let row_start = y as usize * stride;
                for x in block_x..block_x + block_w {
                    let i = row_start + x as usize * 4;
                    if let Some(pixel) = buf.get(i..i + 4) {
                        sum[0] += u64::from(pixel[0]);
                        sum[1] += u64::from(pixel[1]);
                        sum[2] += u64::from(pixel[2]);
                        sum[3] += u64::from(pixel[3]);
                        count += 1;
                    }
                }
            }

            if let Some(average) = checked_average(sum, count) {
                for y in block_y..block_y + block_h {
                    let row_start = y as usize * stride;
                    for x in block_x..block_x + block_w {
                        let i = row_start + x as usize * 4;
                        if let Some(pixel) = buf.get_mut(i..i + 4) {
                            pixel.copy_from_slice(&average);
                        }
                    }
                }
            }
            block_x += block_size;
        }
        block_y += block_size;
    }
}

/// A separable box blur (horizontal pass then vertical pass, each an
/// `O(1)`-per-pixel sliding window — see [`box_blur_axis`]) confined to
/// `rect`, sourced from a **padded copy** of the region around it (expanded
/// by `radius` on every side, clamped to the canvas) so pixels near `rect`'s
/// edge blend with real neighboring content instead of a synthetic clamp at
/// `rect`'s own boundary. Only pixels inside `rect` are written back —
/// `buf` outside `rect` is untouched, unit-tested directly
/// (`blur_only_touches_pixels_inside_the_rect`).
///
/// `O(padded region area)` regardless of `radius`: [`box_blur_axis`]'s
/// sliding window is the reason a larger [`BLUR_RADIUS_PX`] doesn't cost
/// more per pixel, only a slightly larger padding border.
fn blur_region(buf: &mut [u8], width: u32, height: u32, rect: PixelRect, radius: u32) {
    let radius = radius.max(1);
    let x0 = rect.x.min(width);
    let y0 = rect.y.min(height);
    let x1 = rect.x.saturating_add(rect.width).min(width);
    let y1 = rect.y.saturating_add(rect.height).min(height);
    if x1 <= x0 || y1 <= y0 {
        return;
    }

    let sx0 = x0.saturating_sub(radius);
    let sy0 = y0.saturating_sub(radius);
    let sx1 = x1.saturating_add(radius).min(width);
    let sy1 = y1.saturating_add(radius).min(height);
    let src_w = sx1 - sx0;
    let src_h = sy1 - sy0;
    if src_w == 0 || src_h == 0 {
        return;
    }

    let mut source = vec![0u8; src_w as usize * src_h as usize * 4];
    let src_stride = src_w as usize * 4;
    let buf_stride = width as usize * 4;
    for row in 0..src_h {
        let buf_start = (sy0 + row) as usize * buf_stride + sx0 as usize * 4;
        let buf_end = buf_start + src_stride;
        let dst_start = row as usize * src_stride;
        if let Some(chunk) = buf.get(buf_start..buf_end) {
            source[dst_start..dst_start + chunk.len()].copy_from_slice(chunk);
        }
    }

    let mut horizontal = vec![0u8; source.len()];
    box_blur_axis(&source, &mut horizontal, src_w, src_h, radius, true);
    let mut blurred = vec![0u8; source.len()];
    box_blur_axis(&horizontal, &mut blurred, src_w, src_h, radius, false);

    for y in y0..y1 {
        let src_row = (y - sy0) as usize;
        let buf_row_start = y as usize * buf_stride;
        for x in x0..x1 {
            let src_col = (x - sx0) as usize;
            let src_i = src_row * src_stride + src_col * 4;
            let buf_i = buf_row_start + x as usize * 4;
            if let (Some(pixel), Some(dest)) =
                (blurred.get(src_i..src_i + 4), buf.get_mut(buf_i..buf_i + 4))
            {
                dest.copy_from_slice(pixel);
            }
        }
    }
}

/// One pass of a separable box blur, either along rows (`horizontal: true`)
/// or columns (`horizontal: false`), edge-clamped (a sample past the
/// image's edge repeats the edge pixel rather than reading black/garbage).
/// A sliding window — each output pixel updates the running sum by
/// subtracting the sample that just left the window and adding the one that
/// just entered, so the whole pass is `O(pixels)` independent of `radius`,
/// not `O(pixels × radius)` a naive re-sum-every-window approach would be.
fn box_blur_axis(
    src: &[u8],
    dst: &mut [u8],
    width: u32,
    height: u32,
    radius: u32,
    horizontal: bool,
) {
    let window = i64::from(radius) * 2 + 1;
    let (outer, inner) = if horizontal {
        (height, width)
    } else {
        (width, height)
    };
    let stride = width as usize * 4;

    for o in 0..outer {
        for channel in 0..4usize {
            let sample = |i: i64| -> i64 {
                let clamped = i.clamp(0, i64::from(inner) - 1) as u32;
                let index = if horizontal {
                    o as usize * stride + clamped as usize * 4 + channel
                } else {
                    clamped as usize * stride + o as usize * 4 + channel
                };
                i64::from(src.get(index).copied().unwrap_or(0))
            };

            let mut sum: i64 = 0;
            for i in -i64::from(radius)..=i64::from(radius) {
                sum += sample(i);
            }
            for i in 0..inner as i64 {
                let index = if horizontal {
                    o as usize * stride + i as usize * 4 + channel
                } else {
                    i as usize * stride + o as usize * 4 + channel
                };
                if let Some(dest) = dst.get_mut(index) {
                    *dest = (sum / window) as u8;
                }
                sum += sample(i + 1 + i64::from(radius));
                sum -= sample(i - i64::from(radius));
            }
        }
    }
}

// ---------------------------------------------------------------------
// Text rasterization — the one `paint_*`-adjacent primitive with a real
// external dependency (`cosmic-text`; see the Cargo.toml survey essay for
// why it's zero net new crates). Every other raster function above is pure
// geometry with no font/glyph concept at all — this is deliberately
// isolated to its own small section so that boundary stays visible.
// ---------------------------------------------------------------------

/// Where [`raster_text`]'s `position` argument anchors the measured text:
/// top-left for the Text tool (matching `Shape::Text`'s own doc comment),
/// center for a Step badge's numeral (so it sits in the middle of the disc
/// regardless of how many digits it has).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextRasterAlign {
    TopLeft,
    Center,
}

/// The process-lifetime font system + glyph cache `raster_text` shares
/// across every call. `cosmic_text::FontSystem::new()` scans the system's
/// installed fonts (via `fontconfig`) — real work, worth paying **once**
/// per process rather than once per Save/Copy. `SwashCache` additionally
/// memoizes rasterized glyph bitmaps by `(font, size, glyph id)`, so a
/// second annotation reusing the same size/family (the common case — most
/// captions share the current Text-tool size) rasterizes nothing new.
struct FontResources {
    system: cosmic_text::FontSystem,
    cache: cosmic_text::SwashCache,
}

/// Lazily initializes [`FontResources`] on first use and returns the shared
/// lock. `Mutex`, not `RefCell`: `raster_text` runs inside
/// `tokio::task::spawn_blocking` (via `run_blocking`, called from
/// `EditorState::start_save`/`start_copy`), and nothing here assumes it
/// runs on any particular thread. A poisoned lock (only possible if an
/// earlier call panicked while holding it, which nothing in this function
/// does — see its own no-panic reasoning) is recovered via
/// `unwrap_or_else(PoisonError::into_inner)` rather than `.unwrap()`,
/// per the no-panic rule: a poisoned cache is still a perfectly usable
/// cache, just possibly missing whatever the panicking call was about to
/// insert.
fn font_resources() -> &'static std::sync::Mutex<FontResources> {
    static RESOURCES: std::sync::OnceLock<std::sync::Mutex<FontResources>> =
        std::sync::OnceLock::new();
    RESOURCES.get_or_init(|| {
        std::sync::Mutex::new(FontResources {
            system: cosmic_text::FontSystem::new(),
            cache: cosmic_text::SwashCache::new(),
        })
    })
}

/// Bundles [`raster_text`]'s per-call arguments beyond the pixel buffer
/// itself — plain data, no behavior — purely so the function stays under
/// clippy's argument-count lint (`buf`/`width`/`height` plus six more scalar
/// arguments would be nine; every other `paint_*` function in this file
/// already sits at or near that same limit with buffer+geometry+color+width
/// alone, so text — which additionally needs content, a family and an
/// alignment — was always going to need this).
struct TextRasterRequest<'a> {
    content: &'a str,
    family: &'a str,
    size_px: f32,
    color: saola_theme::tokens::Color,
    position: Point,
    align: TextRasterAlign,
}

/// Shapes `request.content` in `request.family` at `request.size_px` and
/// blends it into `buf` at `request.position`, anchored per `request.align`.
/// A no-op (paints nothing) for empty/whitespace-only content, a non-finite/
/// non-positive size, or a family `fontconfig` can't resolve to *any* font
/// at all (in which case `cosmic-text` shapes zero glyphs and this
/// function's own `!max_w.is_finite()` guard below catches it) — every one
/// of those is a "draw nothing" outcome, never a panic, matching the
/// no-panic rule.
///
/// `color`'s own alpha is folded into each glyph's per-pixel coverage alpha
/// (`with_pixels`'s mask output — see the Cargo.toml survey essay), so a
/// translucent `AnnotationColor` (none ship today, but see
/// [`blend_pixel`]'s own doc comment on why that path is still exercised)
/// would translucently blend text too, not just opaque-or-nothing.
fn raster_text(buf: &mut [u8], width: u32, height: u32, request: TextRasterRequest) {
    let TextRasterRequest {
        content,
        family,
        size_px,
        color,
        position,
        align,
    } = request;
    if content.trim().is_empty() || !size_px.is_finite() || size_px <= 0.0 {
        return;
    }
    let mut guard = font_resources()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let FontResources { system, cache } = &mut *guard;

    let metrics = cosmic_text::Metrics::new(size_px, size_px * 1.25);
    let mut buffer = cosmic_text::Buffer::new(system, metrics);
    let mut buffer = buffer.borrow_with(system);
    buffer.set_size(None, None);
    let attrs = cosmic_text::Attrs::new().family(cosmic_text::Family::Name(family));
    buffer.set_text(content, &attrs, cosmic_text::Shaping::Advanced, None);
    buffer.shape_until_scroll(true);

    let mut max_w = 0.0f32;
    let mut min_top = f32::INFINITY;
    let mut max_bottom = f32::NEG_INFINITY;
    for run in buffer.layout_runs() {
        max_w = max_w.max(run.line_w);
        min_top = min_top.min(run.line_top);
        max_bottom = max_bottom.max(run.line_top + run.line_height);
    }
    if !max_w.is_finite() || !min_top.is_finite() || !max_bottom.is_finite() {
        // Nothing shaped at all (e.g. no font could be resolved for
        // `family` and no fallback exists on this system) — draw nothing
        // rather than divide by a nonsense measurement below.
        return;
    }

    let (offset_x, offset_y) = match align {
        TextRasterAlign::TopLeft => (position.x, position.y),
        TextRasterAlign::Center => (
            position.x - max_w / 2.0,
            position.y - (min_top + max_bottom) / 2.0,
        ),
    };
    let offset_x = offset_x.round() as i32;
    let offset_y = offset_y.round() as i32;

    let base_color = cosmic_text::Color::rgb(color.r, color.g, color.b);
    buffer.draw(cache, base_color, |gx, gy, gw, gh, pixel| {
        let (pr, pg, pb, coverage) = pixel.as_rgba_tuple();
        if coverage == 0 {
            return;
        }
        let alpha = (u32::from(coverage) * u32::from(color.a)) / 255;
        let paint = saola_theme::tokens::Color {
            r: pr,
            g: pg,
            b: pb,
            a: alpha as u8,
        };
        for row in 0..gh as i32 {
            for col in 0..gw as i32 {
                blend_pixel(
                    buf,
                    width,
                    height,
                    offset_x + gx + col,
                    offset_y + gy + row,
                    paint,
                );
            }
        }
    });
}

// ---------------------------------------------------------------------
// GUI wrapper — not unit-tested (needs a compositor); see the Stage 14
// handoff for how a human should verify this half.
// ---------------------------------------------------------------------

/// The one blocking-work guard this file needs — `Save`/`Save As`/`Copy` all
/// clone+paint+encode+write, which is real CPU/IO work that must not run on
/// iced's own executor thread. Identical shape to (and duplicated from, since
/// that one is private) `dbus::run_blocking`: guarded `spawn_blocking` when a
/// tokio runtime is current (always true here — `modules::app::run` runs
/// under iced's `tokio`-featured executor), falling straight through
/// otherwise rather than panicking.
async fn run_blocking<T, F>(work: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(_) => match tokio::task::spawn_blocking(work).await {
            Ok(result) => result,
            Err(err) => Err(format!("the save task did not finish: {err}")),
        },
        Err(_) => work(),
    }
}

/// Decodes a saved capture into an [`EditorModel`]'s starting canvas — the
/// editor's own counterpart to `modules::app::load_image` (which decodes for
/// *display only*, into an `image::Handle`). This needs the raw RGBA8 bytes
/// too, to build a [`Frame`] annotations can be composited onto at save time.
/// `scale` is `1.0`: a saved capture's pixels are already the physical-pixel
/// image the user will get back, and nothing downstream of this ever
/// consults `Frame::scale` (it exists for `capture::logical_to_pixel_rect`,
/// a screenshot-capture-time concern this file never touches).
fn load_canvas(path: &Path) -> Result<Frame, String> {
    let decoded = ::image::open(path)
        .map_err(|err| err.to_string())?
        .into_rgba8();
    let (width, height) = decoded.dimensions();
    Frame::new(width, height, 1.0, decoded.into_raw())
        .ok_or_else(|| "the decoded image had an unexpected buffer size".to_string())
}

/// Infers the format a path implies from its extension, falling back to
/// `default` (the loaded `capture.toml`'s own `image-format`) for anything
/// else — including no extension at all, which a hand-typed Save As path is
/// entirely likely to have.
fn format_for_path(path: &Path, default: ImageFormat) -> ImageFormat {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("png") => ImageFormat::Png,
        Some(ext) if ext.eq_ignore_ascii_case("webp") => ImageFormat::Webp,
        _ => default,
    }
}

/// Parses the export panel's free-text quality field (see
/// [`EditorState::export_quality_text`]) into a `1..=100` WebP quality,
/// falling back to `default` (the loaded `capture.toml`'s `webp-quality`)
/// for anything that doesn't parse as a whole number — empty, garbage, or a
/// value so large `as u8` would misbehave (`u32` first, *then* clamped,
/// specifically so a huge number like `"99999"` clamps to `100` instead of
/// wrapping via a raw `as u8` truncation). Same "bad knob → warn-shaped
/// default, never a crash" posture `config.rs`'s hand-walked TOML parsing
/// already established for the same knob's file form.
fn parse_quality(text: &str, default: u8) -> u8 {
    text.trim()
        .parse::<u32>()
        .ok()
        .map(|value| value.clamp(1, 100) as u8)
        .unwrap_or(default)
}

fn save_to_path(
    canvas: &Frame,
    annotations: &[Annotation],
    palette: ColorPalette,
    font_family: &str,
    format: ImageFormat,
    webp_quality: u8,
    target: &Path,
) -> Result<PathBuf, String> {
    let composed = compose(canvas, annotations, palette, font_family);
    let bytes =
        storage::encode_frame(&composed, format, webp_quality).map_err(storage_error_string)?;
    storage::write_atomically(target, &bytes).map_err(storage_error_string)?;
    Ok(target.to_path_buf())
}

/// The clipboard always gets PNG, matching `storage.rs`'s own "the clipboard
/// always gets PNG" rule (see that module's doc comment) — whatever the
/// editor's own save format is, a paste target should still get something
/// every paste target understands. This is unaffected by the Stage 15
/// export panel's format picker (see [`EditorState::export_format`]'s doc
/// comment): that picker governs Save/Save As only.
fn copy_composed(
    canvas: &Frame,
    annotations: &[Annotation],
    palette: ColorPalette,
    font_family: &str,
) -> Result<(), String> {
    let composed = compose(canvas, annotations, palette, font_family);
    let png =
        storage::encode_frame(&composed, ImageFormat::Png, 0).map_err(storage_error_string)?;
    // `DetachedHelper`, not `ThisProcess`: this process is the app window,
    // which the user is very likely to close right after copying (that's
    // often the whole point of "copy, then paste it somewhere and I'm
    // done") — see `storage.rs`'s own doc comment on why a copy's owner must
    // outlive the thing that requested it, not the process that's about to
    // go away.
    storage::copy_to_clipboard(&png, ClipboardOwner::DetachedHelper).map_err(|err| err.to_string())
}

fn storage_error_string(err: StorageError) -> String {
    err.to_string()
}

/// The editor's whole GUI-facing state: the pure [`EditorModel`] plus
/// everything that exists only to drive iced widgets — the resolved color
/// palette, the cached display handle (see the module doc comment's
/// performance section), the Save As field, in-flight-request bookkeeping,
/// and the last feedback line.
pub struct EditorState {
    model: EditorModel,
    path: PathBuf,
    palette: ColorPalette,
    /// The theme's UI font family name (`typography.family_ui`, "IBM Plex
    /// Sans"), resolved once at load — [`raster_text`]'s `family` argument
    /// for every Text/Step annotation this session composes.
    text_font_family: String,
    text_sizes: TextSizeScale,
    webp_quality: u8,
    /// The export panel's format picker (PLAN.md Stage 15's "format
    /// (WebP/PNG)") — starts at `default_format` (the loaded `capture.toml`'s
    /// own knob) and from then on is a plain user choice, independent of it.
    /// Governs Save/Save As only: [`format_for_path`] still lets an
    /// explicit `.png`/`.webp` typed into Save As win outright (unchanged
    /// from Stage 14), and Copy is hardcoded PNG regardless of this field —
    /// see [`copy_composed`]'s doc comment.
    export_format: ImageFormat,
    /// The export panel's quality field, kept as the raw text the user is
    /// typing (same "free text field, parsed at use" shape
    /// `save_as_path` already has) rather than a live-parsed `u8` — so a
    /// momentarily-invalid edit (an empty field mid-edit, say) doesn't fight
    /// the user's typing. Parsed via [`parse_quality`] at Save/Copy time.
    export_quality_text: String,
    save_as_path: String,
    busy: bool,
    feedback: Option<Result<String, String>>,
    /// Rebuilt only when [`EditorModel::canvas`]'s pixels/dimensions
    /// themselves change (load, crop, undo/redo across a crop, or — new in
    /// Stage 15 — a committed Blur/Pixelate) — **not** on every interaction
    /// message. See the module doc comment.
    display_handle: image::Handle,
}

impl EditorState {
    /// Loads `path` and builds the editor's whole starting state, or fails
    /// cleanly (a missing/corrupt file is not a reason to crash a window
    /// that could still usefully show the path and let the user close it —
    /// `modules::app`'s caller renders the `Err` case, unchanged from the
    /// Stage 9 stub's own posture).
    pub fn load(
        path: &Path,
        theme: &Theme,
        default_format: ImageFormat,
        webp_quality: u8,
    ) -> Result<Self, String> {
        let canvas = load_canvas(path)?;
        let handle =
            image::Handle::from_rgba(canvas.width(), canvas.height(), canvas.pixels().to_vec());
        let text_sizes = TextSizeScale::from_theme(theme);
        let mut model = EditorModel::new(canvas);
        model.set_text_size(text_sizes.resolve(TextSizeStop::Medium));
        Ok(EditorState {
            model,
            path: path.to_path_buf(),
            palette: ColorPalette::from_theme(theme),
            text_font_family: theme.typography.family_ui.clone(),
            text_sizes,
            webp_quality,
            export_format: default_format,
            export_quality_text: webp_quality.to_string(),
            save_as_path: path.display().to_string(),
            busy: false,
            feedback: None,
            display_handle: handle,
        })
    }

    /// The file this editor currently considers "the" saved file — the path
    /// `load` opened it from, or a later successful Save As's target
    /// (`Message::SaveFinished(Ok(path))` updates this same field). This is
    /// what `modules::app::App::title` shows, not the original launch
    /// argument, so a Save As's rename is reflected in the window's own
    /// header.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn refresh_display_handle(&mut self) {
        let canvas = self.model.canvas();
        self.display_handle =
            image::Handle::from_rgba(canvas.width(), canvas.height(), canvas.pixels().to_vec());
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ToolSelected(tool) => {
                self.model.set_tool(tool);
                Task::none()
            }
            Message::ColorSelected(color) => {
                self.model.set_color(color);
                Task::none()
            }
            Message::WidthSelected(width) => {
                self.model.set_width(width);
                Task::none()
            }
            Message::Undo => {
                self.model.undo();
                self.refresh_display_handle();
                Task::none()
            }
            Message::Redo => {
                self.model.redo();
                self.refresh_display_handle();
                Task::none()
            }
            Message::DeleteSelected => {
                self.model.delete_selected();
                Task::none()
            }
            Message::ApplyCrop => {
                if self.model.apply_crop() {
                    self.refresh_display_handle();
                }
                Task::none()
            }
            Message::CancelCrop => {
                self.model.cancel_crop();
                Task::none()
            }
            Message::PointerMoved(point) => {
                self.model.pointer_moved(point);
                Task::none()
            }
            Message::Pressed(point) => {
                self.model.pressed(point);
                Task::none()
            }
            Message::Released(point) => {
                if self.model.released(point) {
                    self.refresh_display_handle();
                }
                Task::none()
            }
            Message::TextContentChanged(content) => {
                self.model.set_selected_text_content(content);
                Task::none()
            }
            Message::TextSizeSelected(stop) => {
                self.model.set_text_size(self.text_sizes.resolve(stop));
                Task::none()
            }
            Message::ExportFormatSelected(format) => {
                self.export_format = format;
                Task::none()
            }
            Message::ExportQualityChanged(text) => {
                self.export_quality_text = text;
                Task::none()
            }
            Message::SaveAsPathChanged(text) => {
                self.save_as_path = text;
                Task::none()
            }
            Message::SavePressed => {
                let target = self.path.clone();
                self.start_save(target)
            }
            Message::SaveAsPressed => {
                let target = PathBuf::from(self.save_as_path.trim());
                if target.as_os_str().is_empty() {
                    self.feedback = Some(Err("enter a path to save as".to_string()));
                    Task::none()
                } else {
                    self.start_save(target)
                }
            }
            Message::CopyPressed => self.start_copy(),
            Message::SaveAndCopyPressed => self.start_save_and_copy(),
            Message::SaveFinished(Ok(path)) => {
                self.busy = false;
                self.feedback = Some(Ok(format!("Saved: {}", path.display())));
                self.path = path.clone();
                self.save_as_path = path.display().to_string();
                Task::none()
            }
            Message::SaveFinished(Err(err)) => {
                self.busy = false;
                self.feedback = Some(Err(err));
                Task::none()
            }
            Message::CopyFinished(Ok(())) => {
                self.busy = false;
                self.feedback = Some(Ok("Copied to the clipboard".to_string()));
                Task::none()
            }
            Message::CopyFinished(Err(err)) => {
                self.busy = false;
                self.feedback = Some(Err(err));
                Task::none()
            }
            Message::SaveAndCopyFinished(Ok(path)) => {
                self.busy = false;
                self.feedback = Some(Ok(format!("Saved & copied: {}", path.display())));
                self.path = path.clone();
                self.save_as_path = path.display().to_string();
                Task::none()
            }
            Message::SaveAndCopyFinished(Err(err)) => {
                self.busy = false;
                self.feedback = Some(Err(err));
                Task::none()
            }
        }
    }

    fn start_save(&mut self, target: PathBuf) -> Task<Message> {
        if self.busy {
            return Task::none();
        }
        self.busy = true;
        self.feedback = None;
        let canvas = self.model.canvas_arc();
        let annotations = self.model.annotations().to_vec();
        let palette = self.palette;
        let font_family = self.text_font_family.clone();
        let format = format_for_path(&target, self.export_format);
        let webp_quality = parse_quality(&self.export_quality_text, self.webp_quality);
        Task::perform(
            run_blocking(move || {
                save_to_path(
                    &canvas,
                    &annotations,
                    palette,
                    &font_family,
                    format,
                    webp_quality,
                    &target,
                )
            }),
            Message::SaveFinished,
        )
    }

    fn start_copy(&mut self) -> Task<Message> {
        if self.busy {
            return Task::none();
        }
        self.busy = true;
        self.feedback = None;
        let canvas = self.model.canvas_arc();
        let annotations = self.model.annotations().to_vec();
        let palette = self.palette;
        let font_family = self.text_font_family.clone();
        Task::perform(
            run_blocking(move || copy_composed(&canvas, &annotations, palette, &font_family)),
            Message::CopyFinished,
        )
    }

    /// "Copy vs save vs both" (PLAN.md Stage 15's export panel task) — Save
    /// and Copy already exist as their own buttons/messages; this is the
    /// third leg, saving to the current path **and** copying in one action
    /// rather than two separate clicks. Save-then-copy, not the reverse: if
    /// the save fails there is nothing worth copying (a partially-drawn
    /// canvas that couldn't even be written to disk), so the copy is
    /// skipped and the save's own error is what reaches the user.
    fn start_save_and_copy(&mut self) -> Task<Message> {
        if self.busy {
            return Task::none();
        }
        self.busy = true;
        self.feedback = None;
        let canvas = self.model.canvas_arc();
        let annotations = self.model.annotations().to_vec();
        let palette = self.palette;
        let font_family = self.text_font_family.clone();
        let target = self.path.clone();
        let format = format_for_path(&target, self.export_format);
        let webp_quality = parse_quality(&self.export_quality_text, self.webp_quality);
        Task::perform(
            run_blocking(move || {
                let saved = save_to_path(
                    &canvas,
                    &annotations,
                    palette,
                    &font_family,
                    format,
                    webp_quality,
                    &target,
                )?;
                copy_composed(&canvas, &annotations, palette, &font_family)?;
                Ok(saved)
            }),
            Message::SaveAndCopyFinished,
        )
    }

    pub fn view(&self, theme: &Theme) -> Element<'static, Message> {
        let toolbar = self.toolbar_view(theme);
        let canvas_area = self.canvas_view(theme);
        let footer = self.footer_view(theme);

        column![toolbar, canvas_area, footer].into()
    }

    fn toolbar_view(&self, theme: &Theme) -> Element<'static, Message> {
        let tool_options: Vec<(Tool, &'static str)> =
            Tool::ALL.iter().map(|t| (*t, t.label())).collect();
        let tools = segmented_row(
            theme,
            &tool_options,
            self.model.tool(),
            Message::ToolSelected,
        );

        let color_options: Vec<(AnnotationColor, &'static str)> = AnnotationColor::ALL
            .iter()
            .map(|c| (*c, c.label()))
            .collect();
        let colors = segmented_row(
            theme,
            &color_options,
            self.model.current_color(),
            Message::ColorSelected,
        );

        let width_options: Vec<(StrokeWidth, &'static str)> =
            StrokeWidth::ALL.iter().map(|w| (*w, w.label())).collect();
        let widths = segmented_row(
            theme,
            &width_options,
            self.model.current_width(),
            Message::WidthSelected,
        );

        let undo = action_button(
            theme,
            "Undo",
            self.model.can_undo().then_some(Message::Undo),
            false,
        );
        let redo = action_button(
            theme,
            "Redo",
            self.model.can_redo().then_some(Message::Redo),
            false,
        );
        let delete = action_button(
            theme,
            "Delete",
            self.model
                .selected()
                .is_some()
                .then_some(Message::DeleteSelected),
            false,
        );

        // `align_y(Center)` on every mixed-content row in this toolbar: a
        // `segmented_row` is a track container (its segments plus
        // `SEGMENT_INSET` above and below) while an `action_button` is a bare
        // `hit_target_bar` pill, so the two are *not* the same height. A row
        // defaults to `Alignment::Start`, which would hang the shorter pills
        // from the taller track's top edge.
        let mut rows = column![
            row![tools].spacing(theme.sizes.island_gap),
            row![colors, widths, undo, redo, delete]
                .spacing(theme.sizes.island_gap)
                .align_y(Center),
        ]
        .spacing(theme.sizes.pill_gap);

        if self.model.tool() == Tool::Crop {
            let apply = action_button(
                theme,
                "Apply Crop",
                self.model
                    .pending_crop()
                    .is_some()
                    .then_some(Message::ApplyCrop),
                true,
            );
            let cancel = action_button(theme, "Cancel Crop", Some(Message::CancelCrop), false);
            rows = rows.push(
                row![apply, cancel]
                    .spacing(theme.sizes.island_gap)
                    .align_y(Center),
            );
        }

        // Only shown while a Text annotation is selected — which, per
        // `Tool::Text`'s own doc comment, is true immediately after placing
        // one (a click both creates and selects it). Editing an
        // already-placed caption later just means re-selecting it with the
        // Select tool; this row appears exactly the same way.
        if let Some((content, size)) = self.model.selected_text() {
            let size_options: Vec<(TextSizeStop, &'static str)> =
                TextSizeStop::ALL.iter().map(|s| (*s, s.label())).collect();
            let sizes = segmented_row(
                theme,
                &size_options,
                self.text_sizes.nearest(size),
                Message::TextSizeSelected,
            );
            let content_field = text_input("Text…", content)
                .font(saola_theme::convert::ui_font_regular(theme))
                .size(theme.typography.size.secondary)
                .on_input(Message::TextContentChanged)
                .style(saola_theme::style::text_input::rest(theme, Surface::Paper))
                .width(Length::Fill);
            rows = rows.push(
                row![content_field, sizes]
                    .spacing(theme.sizes.island_gap)
                    .align_y(Center),
            );
        }

        container(rows)
            .width(Length::Fill)
            .padding(theme.sizes.popover_padding)
            .into()
    }

    fn canvas_view(&self, theme: &Theme) -> Element<'static, Message> {
        let picture = image(self.display_handle.clone())
            .content_fit(iced::ContentFit::Contain)
            .width(Length::Fill)
            .height(Length::Fill);

        let overlay = canvas(EditorCanvas {
            canvas_size: (
                self.model.canvas().width() as f32,
                self.model.canvas().height() as f32,
            ),
            annotations: self.model.annotations().to_vec(),
            selected: self.model.selected(),
            interaction: self.model.interaction_snapshot(),
            palette: self.palette,
            accent: theme.palette.accent.into_iced(),
            text_font: saola_theme::convert::ui_font(theme),
            last_pointer: self.model.pointer,
        })
        .width(Length::Fill)
        .height(Length::Fill);

        container(iced::widget::Stack::with_children(vec![
            Element::from(picture),
            Element::from(overlay),
        ]))
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(theme.sizes.popover_padding)
        .style(saola_theme::style::container::tile(theme, Surface::Paper))
        .into()
    }

    fn footer_view(&self, theme: &Theme) -> Element<'static, Message> {
        let path_caption = text(self.path.display().to_string())
            .font(saola_theme::convert::mono_font(theme))
            .size(theme.typography.size.meta)
            .color(theme.on_paper.tertiary.into_iced());

        let save_as_field = text_input("Save as…", &self.save_as_path)
            .font(saola_theme::convert::mono_font(theme))
            .size(theme.typography.size.secondary)
            .on_input(Message::SaveAsPathChanged)
            .style(saola_theme::style::text_input::rest(theme, Surface::Paper))
            .width(Length::Fill);

        let save = action_button(
            theme,
            "Save",
            (!self.busy).then_some(Message::SavePressed),
            true,
        );
        let save_as = action_button(
            theme,
            "Save As",
            (!self.busy).then_some(Message::SaveAsPressed),
            false,
        );
        let copy = action_button(
            theme,
            "Copy",
            (!self.busy).then_some(Message::CopyPressed),
            false,
        );
        let save_and_copy = action_button(
            theme,
            "Save & Copy",
            (!self.busy).then_some(Message::SaveAndCopyPressed),
            false,
        );

        // Export panel (PLAN.md Stage 15): format + quality govern
        // Save/Save As only — Copy is always PNG (`copy_composed`'s own
        // doc comment) — and "copy vs save vs both" is the three buttons
        // above (Save, Copy, Save & Copy) rather than a separate mode
        // picker, so there's no new state machine for "what happens on
        // click" beyond which button was pressed.
        let format_options: Vec<(ImageFormat, &'static str)> =
            vec![(ImageFormat::Webp, "WebP"), (ImageFormat::Png, "PNG")];
        let format_picker = segmented_row(
            theme,
            &format_options,
            self.export_format,
            Message::ExportFormatSelected,
        );
        let quality_field = text_input("Quality 1-100", &self.export_quality_text)
            .font(saola_theme::convert::mono_font(theme))
            .size(theme.typography.size.secondary)
            .on_input(Message::ExportQualityChanged)
            .style(saola_theme::style::text_input::rest(theme, Surface::Paper))
            .width(Length::Fixed(QUALITY_FIELD_WIDTH));

        // Same `align_y(Center)` reasoning as the toolbar above, and it bites
        // harder here: a `text_input` is only as tall as its own text plus
        // padding (well under `hit_target_bar`), so a left-aligned row put
        // the Save As field's baseline visibly above its own button's, and
        // the quality field above the WebP/PNG track's.
        let mut lines = column![
            path_caption,
            row![save_as_field, save_as]
                .spacing(theme.sizes.pill_gap)
                .align_y(Center),
            row![format_picker, quality_field]
                .spacing(theme.sizes.island_gap)
                .align_y(Center),
            row![save, copy, save_and_copy]
                .spacing(theme.sizes.island_gap)
                .align_y(Center),
        ]
        .spacing(theme.sizes.pill_gap);

        if let Some(feedback) = &self.feedback {
            let message = match feedback {
                Ok(message) | Err(message) => message.clone(),
            };
            lines = lines.push(
                text(message)
                    .font(saola_theme::convert::ui_font_regular(theme))
                    .size(theme.typography.size.secondary)
                    .color(theme.on_paper.secondary.into_iced()),
            );
        }

        // Themed for the same reason `modules::app::main_view`'s scrollable
        // is: iced's default scrollbar is a near-black rail that ignores the
        // theme and paints over `paper_window`'s rounded corner.
        scrollable(lines.padding(theme.sizes.popover_padding))
            .width(Length::Fill)
            .style(saola_theme::style::scrollable::rest(theme, Surface::Paper))
            .into()
    }
}

/// A read-only summary of the model's in-flight [`Interaction`], for the
/// canvas painter — kept separate from `Interaction` itself only in name
/// (same variants, same data); `EditorModel::interaction_snapshot` exists
/// because `Interaction` is a private type and [`EditorCanvas`] needs
/// something to hold that is cheap to clone into a `'static`-friendly
/// `canvas::Program`.
type InteractionSnapshot = Interaction;

impl EditorModel {
    fn interaction_snapshot(&self) -> InteractionSnapshot {
        self.interaction.clone()
    }
}

/// One §11-style pill button (an ivory `button::rest`, or — when `primary`
/// — the one terracotta `button::active` a surface is allowed per CLAUDE.md
/// Design language's "at most one terracotta element" rule; `Save` is that
/// element here, matching the live action other surfaces already pick — the
/// overlay's Capture button, the main window's Capture/Start Recording
/// button). `None` for `message` renders disabled, same
/// `on_press_maybe`/`Status::Disabled` shape every other surface in this
/// crate already uses.
fn action_button(
    theme: &Theme,
    label: &'static str,
    message: Option<Message>,
    primary: bool,
) -> Element<'static, Message> {
    let padding = Padding {
        top: 0.0,
        right: theme.sizes.island_gap,
        bottom: 0.0,
        left: theme.sizes.island_gap,
    };
    let height = Length::Fixed(theme.sizes.hit_target_bar);
    let content = || {
        container(
            text(label)
                .font(saola_theme::convert::ui_font(theme))
                .size(theme.typography.size.secondary),
        )
        .align_y(Center)
        .height(Length::Fill)
    };

    // Two near-identical builder chains rather than a shared `style` value:
    // `button::active`/`button::rest` are each `impl Fn(...) -> Style`, two
    // different opaque (`-> impl Trait`) types the compiler will not unify
    // through an `if`/`else` — CLAUDE.md's own conventions call for
    // duplicating the `.style(...)` call per branch over reaching for
    // `Box<dyn Fn>` here.
    if primary {
        button(content())
            .height(height)
            .padding(padding)
            .style(saola_theme::style::button::active(theme, Surface::Paper))
            .on_press_maybe(message)
            .into()
    } else {
        button(content())
            .height(height)
            .padding(padding)
            .style(saola_theme::style::button::rest(theme, Surface::Paper))
            .on_press_maybe(message)
            .into()
    }
}

/// A row of ivory/terracotta pill buttons over a `segmented::track` — a
/// local twin of `modules::app::segmented_row` (that one is private and
/// hardcodes `app::Message` as its return type, so it can't be reused
/// directly across modules without a generic-return refactor this stage
/// didn't need to make; duplicating ~25 lines is cheaper and lower-risk than
/// changing already-shipped, tested code in a sibling module for one new
/// caller).
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
    let mut track = row![].spacing(SEGMENT_INSET);
    for &(value, label) in options {
        let is_selected = value == selected;
        let content = container(
            text(label)
                .font(saola_theme::convert::ui_font(theme))
                .size(theme.typography.size.secondary),
        )
        // `button` does no alignment of its own in iced 0.14 (it places its
        // content flush at the padding origin), so the label is centred by
        // this container or not at all — see `modules::app::segmented_row`'s
        // own note, of which this is the twin.
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
                    is_selected,
                ))
                .on_press(on_select(value)),
        );
    }

    container(track)
        .padding(SEGMENT_INSET)
        .style(saola_theme::style::segmented::track(theme, Surface::Paper))
        .into()
}

/// The interactive vector overlay — see the module doc comment's "two
/// rendering paths" section. Every field is plain owned/`Copy` data (no
/// `&EditorModel` borrow), so this is rebuilt fresh in [`EditorState::
/// canvas_view`] on every `view()` call rather than persisted — cheap,
/// because `annotations` is small and `Point`/`Rectangle`/`Shape` are all
/// lightweight.
struct EditorCanvas {
    /// The base image's own pixel dimensions — the space every stored
    /// [`Point`]/[`Rectangle`] is already in.
    canvas_size: (f32, f32),
    annotations: Vec<Annotation>,
    selected: Option<AnnotationId>,
    interaction: InteractionSnapshot,
    palette: ColorPalette,
    accent: iced::Color,
    /// The theme's UI font — [`draw_shape`]'s Text/Step arms need a real
    /// `iced::Font`, not just a color, since (unlike every Stage 14 shape)
    /// they render glyphs. Resolved once in [`EditorState::canvas_view`],
    /// the interactive-path counterpart to [`EditorState::text_font_family`]
    /// (which the *raster* path uses, as a plain family-name `&str` instead
    /// — `raster_text`'s `cosmic-text` pipeline has no `iced::Font` concept
    /// at all).
    text_font: iced::Font,
    /// Image-space fallback for a release event whose `cursor.position_in`
    /// comes back `None` (the OS delivered it after the pointer had already
    /// left the canvas widget's bounds) — see [`EditorModel::pointer`]'s doc
    /// comment.
    last_pointer: Option<Point>,
}

impl EditorCanvas {
    fn resolve_point(
        &self,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        fit: FitTransform,
    ) -> Option<Point> {
        cursor
            .position_in(bounds)
            .map(|p| fit.to_image(p))
            .or(self.last_pointer)
    }

    /// The shape an annotation should be drawn at right now — its committed
    /// geometry, unless it's the one currently being live-dragged by the
    /// Select tool, in which case the drag's preview substitutes for it.
    fn effective_shape<'a>(&'a self, annotation: &'a Annotation) -> &'a Shape {
        if let Interaction::Moving { id, preview, .. } = &self.interaction {
            if *id == annotation.id {
                return preview;
            }
        }
        &annotation.shape
    }
}

impl canvas::Program<Message> for EditorCanvas {
    type State = ();

    fn update(
        &self,
        _state: &mut Self::State,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let fit = FitTransform::new(self.canvas_size, (bounds.width, bounds.height));
        match event {
            iced::Event::Mouse(mouse::Event::CursorMoved { .. }) => cursor
                .position_in(bounds)
                .map(|p| fit.to_image(p))
                .map(|p| canvas::Action::publish(Message::PointerMoved(p))),
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => self
                .resolve_point(bounds, cursor, fit)
                .map(|p| canvas::Action::publish(Message::Pressed(p)).and_capture()),
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => self
                .resolve_point(bounds, cursor, fit)
                .map(|p| canvas::Action::publish(Message::Released(p))),
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let fit = FitTransform::new(self.canvas_size, (bounds.width, bounds.height));
        let mut frame = canvas::Frame::new(renderer, bounds.size());

        for annotation in &self.annotations {
            let shape = self.effective_shape(annotation);
            let color = annotation.color.resolve(self.palette).into_iced();
            draw_shape(
                &mut frame,
                fit,
                shape,
                color,
                annotation.stroke_width * fit.scale,
                self.palette,
                self.text_font,
            );
            if Some(annotation.id) == self.selected {
                draw_selection_highlight(&mut frame, fit, shape, self.accent);
            }
        }

        match &self.interaction {
            Interaction::DrawingLine {
                tool,
                anchor,
                current,
            } => {
                let shape = line_tool_shape(*tool, *anchor, *current);
                draw_shape(
                    &mut frame,
                    fit,
                    &shape,
                    self.accent,
                    4.0,
                    self.palette,
                    self.text_font,
                );
            }
            Interaction::DrawingFreehand { points } => {
                let shape = Shape::Freehand {
                    points: points.clone(),
                };
                draw_shape(
                    &mut frame,
                    fit,
                    &shape,
                    self.accent,
                    4.0,
                    self.palette,
                    self.text_font,
                );
            }
            Interaction::Cropping { anchor, current } => {
                draw_region_dimming(
                    &mut frame,
                    fit,
                    rect_from_corners(*anchor, *current),
                    bounds.size(),
                    self.accent,
                );
            }
            Interaction::Redacting {
                anchor, current, ..
            } => {
                draw_region_dimming(
                    &mut frame,
                    fit,
                    rect_from_corners(*anchor, *current),
                    bounds.size(),
                    self.accent,
                );
            }
            _ => {}
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let fit = FitTransform::new(self.canvas_size, (bounds.width, bounds.height));
        let Some(position) = cursor.position_in(bounds).map(|p| fit.to_image(p)) else {
            return mouse::Interaction::None;
        };
        match &self.interaction {
            Interaction::Moving { .. } => mouse::Interaction::Grabbing,
            Interaction::DrawingLine { .. }
            | Interaction::DrawingFreehand { .. }
            | Interaction::Cropping { .. }
            | Interaction::Redacting { .. } => mouse::Interaction::Crosshair,
            Interaction::Idle => {
                let hit = self
                    .annotations
                    .iter()
                    .any(|a| shape_hit(&a.shape, position, SELECT_TOLERANCE));
                if hit {
                    mouse::Interaction::Grab
                } else {
                    mouse::Interaction::Idle
                }
            }
        }
    }
}

/// Draws one [`Shape`], transformed into view-space by `fit`, as vector
/// geometry — the interactive twin of the `paint_*` raster functions above,
/// visually consistent with them but never required to be pixel-identical
/// (see the module doc comment). `palette`/`text_font` are only consulted by
/// the Text/Step arms (Stage 15) — every Stage 14 shape ignores them, same
/// as `raster_text`'s `font_family` argument being unused by `paint_arrow`
/// et al. on the raster side.
fn draw_shape(
    frame: &mut canvas::Frame,
    fit: FitTransform,
    shape: &Shape,
    color: iced::Color,
    view_stroke_width: f32,
    palette: ColorPalette,
    text_font: iced::Font,
) {
    let stroke = canvas::Stroke {
        style: canvas::Style::Solid(color),
        width: view_stroke_width.max(1.0),
        line_cap: canvas::LineCap::Round,
        line_join: canvas::LineJoin::Round,
        ..canvas::Stroke::default()
    };

    match shape {
        Shape::Arrow { start, end } => {
            let view_start = fit.to_view(*start);
            let view_end = fit.to_view(*end);
            let path = canvas::Path::new(|builder| {
                builder.move_to(view_start);
                builder.line_to(view_end);
            });
            frame.stroke(&path, stroke);
            draw_arrowhead(frame, view_start, view_end, color, view_stroke_width);
        }
        Shape::Rectangle { rect } => {
            let top_left = fit.to_view(Point::new(rect.x, rect.y));
            let size = Size::new(rect.width * fit.scale, rect.height * fit.scale);
            let path = canvas::Path::rectangle(top_left, size);
            frame.stroke(&path, stroke);
        }
        Shape::Ellipse { rect } => {
            let center = fit.to_view(Point::new(
                rect.x + rect.width / 2.0,
                rect.y + rect.height / 2.0,
            ));
            let path = canvas::Path::new(|builder| {
                builder.ellipse(canvas::path::arc::Elliptical {
                    center,
                    radii: iced::Vector::new(
                        rect.width * fit.scale / 2.0,
                        rect.height * fit.scale / 2.0,
                    ),
                    rotation: iced::Radians(0.0),
                    start_angle: iced::Radians(0.0),
                    end_angle: iced::Radians(std::f32::consts::TAU),
                });
            });
            frame.stroke(&path, stroke);
        }
        Shape::Freehand { points } => {
            if points.len() < 2 {
                return;
            }
            let path = canvas::Path::new(|builder| {
                builder.move_to(fit.to_view(points[0]));
                for point in &points[1..] {
                    builder.line_to(fit.to_view(*point));
                }
            });
            frame.stroke(&path, stroke);
        }
        Shape::Text {
            position,
            content,
            size,
        } => {
            frame.fill_text(canvas::Text {
                content: content.clone(),
                position: fit.to_view(*position),
                color,
                size: iced::Pixels((*size * fit.scale).max(1.0)),
                font: text_font,
                align_x: iced::advanced::text::Alignment::Left,
                align_y: iced::alignment::Vertical::Top,
                shaping: iced::advanced::text::Shaping::Advanced,
                ..canvas::Text::default()
            });
        }
        Shape::Step { position, number } => {
            let view_position = fit.to_view(*position);
            let radius = STEP_BADGE_RADIUS * fit.scale;
            let disc = canvas::Path::circle(view_position, radius);
            frame.fill(&disc, palette.terracotta.into_iced());
            frame.fill_text(canvas::Text {
                content: number.to_string(),
                position: view_position,
                color: palette.ivory.into_iced(),
                size: iced::Pixels(
                    (STEP_BADGE_RADIUS * STEP_BADGE_FONT_RATIO * fit.scale).max(1.0),
                ),
                font: text_font,
                align_x: iced::advanced::text::Alignment::Center,
                align_y: iced::alignment::Vertical::Center,
                shaping: iced::advanced::text::Shaping::Advanced,
                ..canvas::Text::default()
            });
        }
    }
}

fn draw_arrowhead(
    frame: &mut canvas::Frame,
    start: Point,
    end: Point,
    color: iced::Color,
    view_stroke_width: f32,
) {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length = (dx * dx + dy * dy).sqrt();
    if length < 0.5 {
        return;
    }
    let ux = dx / length;
    let uy = dy / length;
    let head_length = (view_stroke_width * 4.0).max(12.0).min(length);
    let head_width = head_length * 0.6;
    let base_center = Point::new(end.x - ux * head_length, end.y - uy * head_length);
    let perp_x = -uy;
    let perp_y = ux;
    let base1 = Point::new(
        base_center.x + perp_x * head_width / 2.0,
        base_center.y + perp_y * head_width / 2.0,
    );
    let base2 = Point::new(
        base_center.x - perp_x * head_width / 2.0,
        base_center.y - perp_y * head_width / 2.0,
    );

    let path = canvas::Path::new(|builder| {
        builder.move_to(end);
        builder.line_to(base1);
        builder.line_to(base2);
        builder.close();
    });
    frame.fill(&path, color);
}

/// A dashed bounding-box outline around a selected annotation, in the
/// style's own accent color — the interactive echo of the toast/overlay's
/// established "dashed terracotta means this is the thing selected"
/// language (`modules::overlay`'s selection edge, same [`DASH_SEGMENTS`]-
/// style pattern, re-derived locally rather than importing the overlay's
/// private constant).
fn draw_selection_highlight(
    frame: &mut canvas::Frame,
    fit: FitTransform,
    shape: &Shape,
    accent: iced::Color,
) {
    const DASH_SEGMENTS: [f32; 2] = [6.0, 4.0];
    let bounds = shape_bounds(shape).expand(6.0);
    let top_left = fit.to_view(Point::new(bounds.x, bounds.y));
    let size = Size::new(bounds.width * fit.scale, bounds.height * fit.scale);
    let path = canvas::Path::rectangle(top_left, size);
    frame.stroke(
        &path,
        canvas::Stroke {
            style: canvas::Style::Solid(accent),
            width: 2.0,
            line_cap: canvas::LineCap::Butt,
            line_join: canvas::LineJoin::Round,
            line_dash: canvas::LineDash {
                segments: &DASH_SEGMENTS,
                offset: 0,
            },
        },
    );
}

/// The Crop tool's live preview, reused as-is for Blur/Pixelate's own drag
/// preview since Stage 15 (same "dim everything outside the region, dashed
/// accent outline" visual language for any tool whose gesture is "drag a
/// rectangle that will affect exactly this area" — Crop, Blur and Pixelate
/// all qualify): the same "dim everything outside the selection" scrim
/// `modules::overlay::SelectionPainter` draws, computed in view-space
/// directly (four bands around the transformed rectangle) rather than
/// transforming per-pixel — cheaper, and this surface's dimming amount is
/// the plain accent color at low opacity rather than a theme scrim token,
/// since `scrims.capture` is specified for the full-output capture overlay,
/// not a windowed editor's canvas (a genuine gap this stage didn't upstream —
/// see the Stage 14 handoff).
fn draw_region_dimming(
    frame: &mut canvas::Frame,
    fit: FitTransform,
    rect: Rectangle,
    view_size: Size,
    accent: iced::Color,
) {
    let top_left = fit.to_view(Point::new(rect.x, rect.y));
    let size = Size::new(rect.width * fit.scale, rect.height * fit.scale);
    let dim = iced::Color {
        a: 0.55,
        ..iced::Color::BLACK
    };
    let band = |frame: &mut canvas::Frame, x: f32, y: f32, w: f32, h: f32| {
        if w > 0.0 && h > 0.0 {
            frame.fill_rectangle(Point::new(x, y), Size::new(w, h), dim);
        }
    };
    band(frame, 0.0, 0.0, view_size.width, top_left.y);
    band(
        frame,
        0.0,
        top_left.y + size.height,
        view_size.width,
        (view_size.height - top_left.y - size.height).max(0.0),
    );
    band(frame, 0.0, top_left.y, top_left.x, size.height);
    band(
        frame,
        top_left.x + size.width,
        top_left.y,
        (view_size.width - top_left.x - size.width).max(0.0),
        size.height,
    );

    let outline = canvas::Path::rectangle(top_left, size);
    frame.stroke(
        &outline,
        canvas::Stroke {
            style: canvas::Style::Solid(accent),
            width: 2.0,
            line_cap: canvas::LineCap::Butt,
            line_join: canvas::LineJoin::Miter,
            ..canvas::Stroke::default()
        },
    );
}

// ---------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Message {
    ToolSelected(Tool),
    ColorSelected(AnnotationColor),
    WidthSelected(StrokeWidth),
    Undo,
    Redo,
    DeleteSelected,
    ApplyCrop,
    CancelCrop,
    /// Image-space points, already resolved by [`EditorCanvas::update`] —
    /// see the module doc comment's tool-state model section for why the
    /// model only ever sees this space.
    PointerMoved(Point),
    Pressed(Point),
    Released(Point),
    /// Edits the selected Text annotation's content — see
    /// [`EditorModel::set_selected_text_content`].
    TextContentChanged(String),
    /// Picks one of [`TextSizeStop`]'s three stops — see
    /// [`EditorModel::set_text_size`].
    TextSizeSelected(TextSizeStop),
    ExportFormatSelected(ImageFormat),
    ExportQualityChanged(String),
    SaveAsPathChanged(String),
    SavePressed,
    SaveAsPressed,
    CopyPressed,
    SaveAndCopyPressed,
    SaveFinished(Result<PathBuf, String>),
    CopyFinished(Result<(), String>),
    SaveAndCopyFinished(Result<PathBuf, String>),
}

// ---------------------------------------------------------------------
// Tests — pure logic only. GUI rendering (`EditorCanvas`, `EditorState`'s
// view/update-with-Task plumbing) needs a compositor and is out of reach
// here; see the Stage 14 handoff for how a human should verify it.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rectangle {
        Rectangle {
            x,
            y,
            width: w,
            height: h,
        }
    }

    fn tiny_frame(width: u32, height: u32) -> Frame {
        let pixels = vec![0u8; (width * height * 4) as usize];
        Frame::new(width, height, 1.0, pixels).expect("valid buffer")
    }

    fn test_palette() -> ColorPalette {
        ColorPalette {
            terracotta: saola_theme::tokens::Color::rgb(0xC6, 0x71, 0x39),
            ink: saola_theme::tokens::Color::rgb(0x0C, 0x0A, 0x00),
            ivory: saola_theme::tokens::Color::rgb(0xFF, 0xFF, 0xF0),
        }
    }

    /// Used only by tests that need *some* family string to pass through —
    /// most of them never reach `raster_text` at all (no `Shape::Text`/
    /// `Shape::Step` involved), and the handful that do are explicitly about
    /// not-panicking rather than exact glyph output (see the "raster_text"
    /// section below for why: real glyph shapes depend on which fonts are
    /// actually installed on the machine running `cargo test`, which this
    /// suite has no control over).
    const TEST_FONT_FAMILY: &str = "IBM Plex Sans";

    // -- FitTransform ------------------------------------------------------

    #[test]
    fn fit_transform_is_identity_when_sizes_match() {
        let fit = FitTransform::new((100.0, 100.0), (100.0, 100.0));
        assert_eq!(fit.scale, 1.0);
        assert_eq!((fit.offset_x, fit.offset_y), (0.0, 0.0));
    }

    #[test]
    fn fit_transform_letterboxes_the_shorter_axis() {
        // A 200x100 image in a 100x100 view: limited by width, so it scales
        // to 100x50 and centers vertically with 25px bands top and bottom.
        let fit = FitTransform::new((200.0, 100.0), (100.0, 100.0));
        assert_eq!(fit.scale, 0.5);
        assert_eq!(fit.offset_x, 0.0);
        assert_eq!(fit.offset_y, 25.0);
    }

    #[test]
    fn fit_transform_round_trips() {
        let fit = FitTransform::new((640.0, 480.0), (300.0, 500.0));
        let original = point(123.0, 45.0);
        let round_tripped = fit.to_image(fit.to_view(original));
        assert!((round_tripped.x - original.x).abs() < 0.01);
        assert!((round_tripped.y - original.y).abs() < 0.01);
    }

    #[test]
    fn fit_transform_degenerate_sizes_do_not_panic() {
        let fit = FitTransform::new((0.0, 100.0), (100.0, 100.0));
        assert_eq!(fit.scale, 1.0);
        // `to_image` must not divide by zero even from a degenerate build.
        let _ = fit.to_image(point(10.0, 10.0));
    }

    // -- distance_to_segment ------------------------------------------------

    #[test]
    fn distance_to_segment_zero_on_the_line() {
        assert_eq!(
            distance_to_segment(point(5.0, 0.0), point(0.0, 0.0), point(10.0, 0.0)),
            0.0
        );
    }

    #[test]
    fn distance_to_segment_perpendicular_offset() {
        let d = distance_to_segment(point(5.0, 3.0), point(0.0, 0.0), point(10.0, 0.0));
        assert!((d - 3.0).abs() < 0.001);
    }

    #[test]
    fn distance_to_segment_clamps_beyond_the_endpoint() {
        // Closest point is the segment's own end, not the infinite line.
        let d = distance_to_segment(point(20.0, 0.0), point(0.0, 0.0), point(10.0, 0.0));
        assert!((d - 10.0).abs() < 0.001);
    }

    #[test]
    fn distance_to_segment_handles_a_zero_length_segment() {
        let d = distance_to_segment(point(3.0, 4.0), point(0.0, 0.0), point(0.0, 0.0));
        assert!((d - 5.0).abs() < 0.001);
    }

    // -- rect_from_corners / rects_intersect / shape_bounds -----------------

    #[test]
    fn rect_from_corners_normalizes_every_direction() {
        let expected = rect(10.0, 20.0, 30.0, 40.0);
        assert_eq!(
            rect_from_corners(point(10.0, 20.0), point(40.0, 60.0)),
            expected
        );
        assert_eq!(
            rect_from_corners(point(40.0, 60.0), point(10.0, 20.0)),
            expected
        );
        assert_eq!(
            rect_from_corners(point(40.0, 20.0), point(10.0, 60.0)),
            expected
        );
    }

    #[test]
    fn rects_intersect_detects_overlap_and_disjointness() {
        assert!(rects_intersect(
            rect(0.0, 0.0, 10.0, 10.0),
            rect(5.0, 5.0, 10.0, 10.0)
        ));
        assert!(!rects_intersect(
            rect(0.0, 0.0, 10.0, 10.0),
            rect(20.0, 20.0, 10.0, 10.0)
        ));
    }

    #[test]
    fn shape_bounds_covers_every_variant() {
        assert_eq!(
            shape_bounds(&Shape::Arrow {
                start: point(0.0, 0.0),
                end: point(10.0, 10.0)
            }),
            rect(0.0, 0.0, 10.0, 10.0)
        );
        assert_eq!(
            shape_bounds(&Shape::Rectangle {
                rect: rect(1.0, 2.0, 3.0, 4.0)
            }),
            rect(1.0, 2.0, 3.0, 4.0)
        );
        assert_eq!(
            shape_bounds(&Shape::Freehand {
                points: vec![point(5.0, 5.0), point(0.0, 10.0), point(15.0, 2.0)]
            }),
            rect(0.0, 2.0, 15.0, 8.0)
        );
    }

    // -- EditorModel: drawing ------------------------------------------------

    #[test]
    fn a_new_model_starts_on_select_with_no_annotations() {
        let model = EditorModel::new(tiny_frame(20, 20));
        assert_eq!(model.tool(), Tool::Select);
        assert!(model.annotations().is_empty());
        assert!(!model.can_undo());
    }

    #[test]
    fn dragging_the_rectangle_tool_adds_one_annotation() {
        let mut model = EditorModel::new(tiny_frame(20, 20));
        model.set_tool(Tool::Rectangle);
        model.pressed(point(2.0, 2.0));
        model.pointer_moved(point(10.0, 12.0));
        model.released(point(10.0, 12.0));

        assert_eq!(model.annotations().len(), 1);
        assert!(model.can_undo());
        match &model.annotations()[0].shape {
            Shape::Rectangle { rect } => {
                assert_eq!(*rect, rect_from_corners(point(2.0, 2.0), point(10.0, 12.0)))
            }
            other => panic!("expected a rectangle, got {other:?}"),
        }
    }

    #[test]
    fn a_tiny_drag_is_discarded_as_a_stray_click() {
        let mut model = EditorModel::new(tiny_frame(20, 20));
        model.set_tool(Tool::Arrow);
        model.pressed(point(5.0, 5.0));
        model.released(point(5.5, 5.2));

        assert!(model.annotations().is_empty());
        assert!(!model.can_undo());
    }

    #[test]
    fn a_single_point_freehand_is_discarded() {
        let mut model = EditorModel::new(tiny_frame(20, 20));
        model.set_tool(Tool::Freehand);
        model.pressed(point(5.0, 5.0));
        model.released(point(5.0, 5.0));

        assert!(model.annotations().is_empty());
    }

    #[test]
    fn a_real_freehand_drag_commits_a_polyline() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_tool(Tool::Freehand);
        model.pressed(point(0.0, 0.0));
        model.pointer_moved(point(10.0, 10.0));
        model.pointer_moved(point(20.0, 0.0));
        model.released(point(20.0, 0.0));

        assert_eq!(model.annotations().len(), 1);
        match &model.annotations()[0].shape {
            Shape::Freehand { points } => assert!(points.len() >= 2),
            other => panic!("expected freehand, got {other:?}"),
        }
    }

    #[test]
    fn new_shapes_use_the_current_color_and_width() {
        let mut model = EditorModel::new(tiny_frame(20, 20));
        model.set_color(AnnotationColor::Ink);
        model.set_width(StrokeWidth::Thick);
        model.set_tool(Tool::Arrow);
        model.pressed(point(0.0, 0.0));
        model.released(point(10.0, 10.0));

        let annotation = &model.annotations()[0];
        assert_eq!(annotation.color, AnnotationColor::Ink);
        assert_eq!(annotation.stroke_width, StrokeWidth::Thick.pixels());
    }

    // -- EditorModel: select / move / delete ---------------------------------

    #[test]
    fn selecting_clicks_a_placed_shape() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_tool(Tool::Rectangle);
        model.pressed(point(5.0, 5.0));
        model.released(point(20.0, 20.0));
        let id = model.annotations()[0].id;

        model.set_tool(Tool::Select);
        model.pressed(point(10.0, 10.0));
        model.released(point(10.0, 10.0));

        assert_eq!(model.selected(), Some(id));
    }

    #[test]
    fn clicking_empty_space_deselects() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_tool(Tool::Rectangle);
        model.pressed(point(5.0, 5.0));
        model.released(point(20.0, 20.0));

        model.set_tool(Tool::Select);
        model.pressed(point(10.0, 10.0));
        model.released(point(10.0, 10.0));
        assert!(model.selected().is_some());

        model.pressed(point(45.0, 45.0));
        model.released(point(45.0, 45.0));
        assert_eq!(model.selected(), None);
    }

    #[test]
    fn a_plain_click_to_select_does_not_push_an_undo_entry() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_tool(Tool::Rectangle);
        model.pressed(point(5.0, 5.0));
        model.released(point(20.0, 20.0));
        let undo_depth_after_draw = undo_depth(&model);

        model.set_tool(Tool::Select);
        model.pressed(point(10.0, 10.0));
        model.released(point(10.0, 10.0));

        assert_eq!(undo_depth(&model), undo_depth_after_draw);
    }

    #[test]
    fn dragging_a_selected_shape_moves_it_and_pushes_undo() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_tool(Tool::Rectangle);
        model.pressed(point(5.0, 5.0));
        model.released(point(15.0, 15.0));
        let original_rect = rect_from_corners(point(5.0, 5.0), point(15.0, 15.0));
        let before = undo_depth(&model);

        model.set_tool(Tool::Select);
        model.pressed(point(10.0, 10.0));
        model.pointer_moved(point(15.0, 12.0));
        model.released(point(15.0, 12.0));

        assert_eq!(undo_depth(&model), before + 1);
        match &model.annotations()[0].shape {
            Shape::Rectangle { rect } => {
                assert_eq!(rect.x, original_rect.x + 5.0);
                assert_eq!(rect.y, original_rect.y + 2.0);
            }
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn deleting_the_selection_removes_it_and_can_be_undone() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_tool(Tool::Arrow);
        model.pressed(point(0.0, 0.0));
        model.released(point(20.0, 20.0));

        model.set_tool(Tool::Select);
        model.pressed(point(10.0, 10.0));
        model.released(point(10.0, 10.0));
        assert_eq!(model.annotations().len(), 1);

        model.delete_selected();
        assert!(model.annotations().is_empty());

        model.undo();
        assert_eq!(model.annotations().len(), 1);
    }

    #[test]
    fn deleting_with_nothing_selected_is_a_no_op() {
        let mut model = EditorModel::new(tiny_frame(20, 20));
        model.delete_selected();
        assert!(!model.can_undo());
    }

    // -- EditorModel: undo/redo -----------------------------------------------

    #[test]
    fn undo_then_redo_round_trips() {
        let mut model = EditorModel::new(tiny_frame(30, 30));
        model.set_tool(Tool::Ellipse);
        model.pressed(point(0.0, 0.0));
        model.released(point(10.0, 10.0));
        assert_eq!(model.annotations().len(), 1);

        model.undo();
        assert!(model.annotations().is_empty());
        assert!(model.can_redo());

        model.redo();
        assert_eq!(model.annotations().len(), 1);
        assert!(!model.can_redo());
    }

    #[test]
    fn a_new_action_after_undo_clears_the_redo_stack() {
        let mut model = EditorModel::new(tiny_frame(30, 30));
        model.set_tool(Tool::Ellipse);
        model.pressed(point(0.0, 0.0));
        model.released(point(10.0, 10.0));
        model.undo();
        assert!(model.can_redo());

        model.set_tool(Tool::Arrow);
        model.pressed(point(0.0, 0.0));
        model.released(point(5.0, 5.0));
        assert!(!model.can_redo());
    }

    // -- EditorModel: crop ------------------------------------------------------

    #[test]
    fn a_crop_drag_then_apply_shrinks_the_canvas_and_translates_annotations() {
        let mut model = EditorModel::new(tiny_frame(100, 100));
        model.set_tool(Tool::Arrow);
        model.pressed(point(20.0, 20.0));
        model.released(point(30.0, 30.0));

        model.set_tool(Tool::Crop);
        model.pressed(point(10.0, 10.0));
        model.pointer_moved(point(50.0, 50.0));
        model.released(point(50.0, 50.0));
        assert_eq!(model.pending_crop(), Some(rect(10.0, 10.0, 40.0, 40.0)));

        assert!(model.apply_crop());
        assert_eq!((model.canvas().width(), model.canvas().height()), (40, 40));
        match &model.annotations()[0].shape {
            Shape::Arrow { start, .. } => assert_eq!(*start, point(10.0, 10.0)),
            other => panic!("expected arrow, got {other:?}"),
        }
        assert_eq!(model.pending_crop(), None);
    }

    #[test]
    fn a_shape_entirely_outside_the_crop_is_dropped() {
        let mut model = EditorModel::new(tiny_frame(100, 100));
        model.set_tool(Tool::Arrow);
        model.pressed(point(90.0, 90.0));
        model.released(point(99.0, 99.0));

        model.set_tool(Tool::Crop);
        model.pressed(point(0.0, 0.0));
        model.released(point(20.0, 20.0));
        model.apply_crop();

        assert!(model.annotations().is_empty());
    }

    #[test]
    fn switching_away_from_crop_discards_the_pending_rectangle() {
        let mut model = EditorModel::new(tiny_frame(100, 100));
        model.set_tool(Tool::Crop);
        model.pressed(point(10.0, 10.0));
        model.released(point(50.0, 50.0));
        assert!(model.pending_crop().is_some());

        model.set_tool(Tool::Select);
        assert_eq!(model.pending_crop(), None);
    }

    #[test]
    fn a_tiny_crop_drag_leaves_nothing_pending() {
        let mut model = EditorModel::new(tiny_frame(100, 100));
        model.set_tool(Tool::Crop);
        model.pressed(point(10.0, 10.0));
        model.released(point(10.5, 10.2));
        assert_eq!(model.pending_crop(), None);
    }

    #[test]
    fn applying_a_crop_can_be_undone_restoring_the_canvas_size() {
        let mut model = EditorModel::new(tiny_frame(80, 80));
        model.set_tool(Tool::Crop);
        model.pressed(point(0.0, 0.0));
        model.released(point(40.0, 40.0));
        model.apply_crop();
        assert_eq!(model.canvas().width(), 40);

        model.undo();
        assert_eq!(model.canvas().width(), 80);
    }

    #[test]
    fn applying_with_nothing_pending_is_a_no_op() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_tool(Tool::Crop);
        assert!(!model.apply_crop());
        assert_eq!(model.canvas().width(), 50);
    }

    // -- EditorModel: color/width applied to the selection -----------------

    #[test]
    fn changing_color_repaints_the_current_selection() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_tool(Tool::Arrow);
        model.pressed(point(0.0, 0.0));
        model.released(point(10.0, 10.0));
        model.set_tool(Tool::Select);
        model.pressed(point(5.0, 5.0));
        model.released(point(5.0, 5.0));

        model.set_color(AnnotationColor::Ivory);
        assert_eq!(model.annotations()[0].color, AnnotationColor::Ivory);
        assert_eq!(model.current_color(), AnnotationColor::Ivory);
    }

    #[test]
    fn changing_color_with_nothing_selected_only_sets_the_default() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_color(AnnotationColor::Ink);
        assert_eq!(model.current_color(), AnnotationColor::Ink);
    }

    /// `undo_stack` is a private field, not a production-code accessor —
    /// this test module is a descendant of `editor`, so it can read it
    /// directly, which is simpler and less fragile than adding a getter
    /// nothing outside tests needs.
    fn undo_depth(model: &EditorModel) -> usize {
        model.undo_stack.len()
    }

    // -- Raster: blend_pixel --------------------------------------------------

    #[test]
    fn blend_pixel_out_of_bounds_is_a_no_op() {
        let mut buf = vec![0u8; 4 * 2 * 2];
        blend_pixel(
            &mut buf,
            2,
            2,
            -1,
            0,
            saola_theme::tokens::Color::rgb(255, 0, 0),
        );
        blend_pixel(
            &mut buf,
            2,
            2,
            5,
            5,
            saola_theme::tokens::Color::rgb(255, 0, 0),
        );
        assert_eq!(buf, vec![0u8; 16]);
    }

    #[test]
    fn blend_pixel_opaque_color_overwrites() {
        let mut buf = vec![0u8; 4 * 2 * 2];
        blend_pixel(
            &mut buf,
            2,
            2,
            1,
            1,
            saola_theme::tokens::Color::rgb(10, 20, 30),
        );
        assert_eq!(&buf[12..16], &[10, 20, 30, 255]);
    }

    #[test]
    fn blend_pixel_translucent_color_mixes_with_the_background() {
        let mut buf = vec![255u8, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        blend_pixel(
            &mut buf,
            2,
            2,
            0,
            0,
            saola_theme::tokens::Color::rgba(0, 0, 0, 128),
        );
        // Halfway between black (the paint) and white (the background).
        assert!(buf[0] > 100 && buf[0] < 155);
    }

    // -- Raster: shapes ---------------------------------------------------------

    #[test]
    fn paint_segment_capsule_colors_the_line_and_leaves_far_pixels_alone() {
        let (w, h) = (20, 5);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let color = saola_theme::tokens::Color::rgb(200, 100, 50);
        paint_segment_capsule(
            &mut buf,
            w,
            h,
            point(2.0, 2.0),
            point(17.0, 2.0),
            1.0,
            color,
        );

        // A pixel on the line's own row, near the middle, is painted.
        let mid_index = (2 * w as usize + 10) * 4;
        assert_eq!(&buf[mid_index..mid_index + 4], &[200, 100, 50, 255]);

        // A pixel far above the line is untouched.
        let far_index = 10 * 4;
        assert_eq!(&buf[far_index..far_index + 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn paint_rect_outline_colors_the_border_and_not_the_interior() {
        let (w, h) = (20, 20);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let color = saola_theme::tokens::Color::rgb(255, 0, 0);
        paint_rect_outline(&mut buf, w, h, rect(2.0, 2.0, 16.0, 16.0), color, 2.0);

        // Just inside the top-left corner: on the border.
        let border_index = (3 * w as usize + 3) * 4;
        assert_eq!(&buf[border_index..border_index + 4], &[255, 0, 0, 255]);

        // Dead center: inside the ring, untouched.
        let center_index = (10 * w as usize + 10) * 4;
        assert_eq!(&buf[center_index..center_index + 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn paint_ellipse_outline_rings_and_does_not_fill_the_center() {
        let (w, h) = (30, 30);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let color = saola_theme::tokens::Color::rgb(0, 255, 0);
        paint_ellipse_outline(&mut buf, w, h, rect(5.0, 5.0, 20.0, 20.0), color, 2.0);

        let center_index = (15 * w as usize + 15) * 4;
        assert_eq!(&buf[center_index..center_index + 4], &[0, 0, 0, 0]);

        // The topmost point of the ellipse's outer edge should be painted.
        let top_index = (5 * w as usize + 15) * 4;
        assert_eq!(&buf[top_index..top_index + 4], &[0, 255, 0, 255]);
    }

    #[test]
    fn paint_arrow_colors_the_shaft_and_the_head() {
        let (w, h) = (40, 10);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let color = saola_theme::tokens::Color::rgb(1, 2, 3);
        paint_arrow(
            &mut buf,
            w,
            h,
            point(2.0, 5.0),
            point(35.0, 5.0),
            color,
            2.0,
        );

        // Shaft, near the start.
        let shaft_index = (5 * w as usize + 5) * 4;
        assert_eq!(&buf[shaft_index..shaft_index + 4], &[1, 2, 3, 255]);

        // Inside the arrowhead's body (not the exact apex pixel, which is a
        // single infinitesimal point at the triangle's tip and isn't
        // guaranteed to cover its containing pixel's sample center).
        let head_index = (5 * w as usize + 28) * 4;
        assert_eq!(&buf[head_index..head_index + 4], &[1, 2, 3, 255]);
    }

    #[test]
    fn paint_arrow_degenerate_length_paints_a_dot_not_nothing() {
        let (w, h) = (10, 10);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let color = saola_theme::tokens::Color::rgb(9, 9, 9);
        paint_arrow(
            &mut buf,
            w,
            h,
            point(5.0, 5.0),
            point(5.05, 5.05),
            color,
            2.0,
        );
        let index = (5 * w as usize + 5) * 4;
        assert_eq!(&buf[index..index + 4], &[9, 9, 9, 255]);
    }

    // -- compose ------------------------------------------------------------

    #[test]
    fn compose_preserves_dimensions_and_paints_annotations() {
        let base = tiny_frame(20, 20);
        let annotations = vec![Annotation {
            id: 0,
            shape: Shape::Rectangle {
                rect: rect(2.0, 2.0, 10.0, 10.0),
            },
            color: AnnotationColor::Terracotta,
            stroke_width: 2.0,
        }];
        let composed = compose(&base, &annotations, test_palette(), TEST_FONT_FAMILY);

        assert_eq!((composed.width(), composed.height()), (20, 20));
        // Some pixel on the drawn rectangle's border differs from the
        // all-zero base.
        let border_index = (2 * 20 + 2) * 4;
        assert_ne!(
            &composed.pixels()[border_index..border_index + 4],
            &[0, 0, 0, 0]
        );
    }

    #[test]
    fn compose_with_no_annotations_matches_the_base_exactly() {
        let base = tiny_frame(10, 10);
        let composed = compose(&base, &[], test_palette(), TEST_FONT_FAMILY);
        assert_eq!(composed.pixels(), base.pixels());
    }

    // -- format_for_path ------------------------------------------------------

    #[test]
    fn format_for_path_infers_from_the_extension() {
        assert_eq!(
            format_for_path(Path::new("/tmp/a.png"), ImageFormat::Webp),
            ImageFormat::Png
        );
        assert_eq!(
            format_for_path(Path::new("/tmp/a.PNG"), ImageFormat::Webp),
            ImageFormat::Png
        );
        assert_eq!(
            format_for_path(Path::new("/tmp/a.webp"), ImageFormat::Png),
            ImageFormat::Webp
        );
        assert_eq!(
            format_for_path(Path::new("/tmp/a"), ImageFormat::Png),
            ImageFormat::Png
        );
        assert_eq!(
            format_for_path(Path::new("/tmp/a.jpg"), ImageFormat::Webp),
            ImageFormat::Webp
        );
    }

    // -- load_canvas ------------------------------------------------------------

    #[test]
    fn load_canvas_reports_a_clean_error_for_a_missing_file() {
        let result = load_canvas(Path::new("/nonexistent/definitely-not-a-file.webp"));
        assert!(result.is_err());
    }

    // -- Stage 15: Text tool -------------------------------------------------

    #[test]
    fn clicking_the_text_tool_places_an_empty_selected_annotation() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_tool(Tool::Text);
        model.pressed(point(10.0, 12.0));

        assert_eq!(model.annotations().len(), 1);
        let annotation = &model.annotations()[0];
        assert_eq!(model.selected(), Some(annotation.id));
        match &annotation.shape {
            Shape::Text {
                position, content, ..
            } => {
                assert_eq!(*position, point(10.0, 12.0));
                assert!(content.is_empty());
            }
            other => panic!("expected text, got {other:?}"),
        }
        // Unlike drawing tools, Text commits on a single press — releasing
        // at the same point must not place a second annotation.
        model.released(point(10.0, 12.0));
        assert_eq!(model.annotations().len(), 1);
    }

    #[test]
    fn selected_text_reports_content_and_size_only_for_a_text_annotation() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_tool(Tool::Rectangle);
        model.pressed(point(2.0, 2.0));
        model.released(point(10.0, 10.0));
        assert_eq!(
            model.selected_text(),
            None,
            "a selected Rectangle isn't text"
        );

        model.set_tool(Tool::Text);
        model.pressed(point(5.0, 5.0));
        let (content, size) = model
            .selected_text()
            .expect("a Text annotation is selected");
        assert!(content.is_empty());
        assert_eq!(size, model.current_text_size);
    }

    #[test]
    fn editing_selected_text_content_updates_the_shape_in_place() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_tool(Tool::Text);
        model.pressed(point(5.0, 5.0));
        model.set_selected_text_content("hello".to_string());

        match &model.annotations()[0].shape {
            Shape::Text { content, .. } => assert_eq!(content, "hello"),
            other => panic!("expected text, got {other:?}"),
        }
        // Content edits are a picker-style restyle, not a drawing action —
        // no separate undo entry, same posture as `set_color`/`set_width`.
        assert_eq!(undo_depth(&model), 1);
    }

    #[test]
    fn set_text_size_resizes_the_selection_and_future_placements() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_tool(Tool::Text);
        model.pressed(point(5.0, 5.0));
        model.set_text_size(40.0);

        match &model.annotations()[0].shape {
            Shape::Text { size, .. } => assert_eq!(*size, 40.0),
            other => panic!("expected text, got {other:?}"),
        }

        model.pressed(point(20.0, 20.0));
        match &model.annotations()[1].shape {
            Shape::Text { size, .. } => assert_eq!(*size, 40.0),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn set_text_size_clamps_to_a_positive_minimum() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_text_size(-5.0);
        model.set_tool(Tool::Text);
        model.pressed(point(1.0, 1.0));
        match &model.annotations()[0].shape {
            Shape::Text { size, .. } => assert!(*size >= 1.0),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn undoing_a_placed_text_annotation_removes_it() {
        let mut model = EditorModel::new(tiny_frame(50, 50));
        model.set_tool(Tool::Text);
        model.pressed(point(5.0, 5.0));
        assert_eq!(model.annotations().len(), 1);

        model.undo();
        assert!(model.annotations().is_empty());
    }

    #[test]
    fn text_shape_bounds_hit_and_translate() {
        let shape = Shape::Text {
            position: point(10.0, 10.0),
            content: "hi".to_string(),
            size: 20.0,
        };
        let bounds = shape_bounds(&shape);
        assert_eq!((bounds.x, bounds.y), (10.0, 10.0));
        assert!(bounds.width > 0.0 && bounds.height > 0.0);

        assert!(shape_hit(&shape, point(11.0, 11.0), SELECT_TOLERANCE));
        assert!(!shape_hit(&shape, point(1000.0, 1000.0), SELECT_TOLERANCE));

        let translated = translate_shape(&shape, 5.0, -5.0);
        match translated {
            Shape::Text {
                position,
                content,
                size,
            } => {
                assert_eq!(position, point(15.0, 5.0));
                assert_eq!(content, "hi");
                assert_eq!(size, 20.0);
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    // -- Stage 15: Step tool --------------------------------------------------

    #[test]
    fn step_badges_auto_increment_and_never_reuse_a_number() {
        // Badges well apart (`STEP_BADGE_RADIUS`'s bounding-box hit test is
        // deliberately generous — see `Shape::Step`'s `shape_bounds` — so
        // three badges spaced closer than roughly `2 * STEP_BADGE_RADIUS`
        // would have overlapping hit boxes and this test would end up
        // selecting the wrong one).
        let mut model = EditorModel::new(tiny_frame(150, 150));
        model.set_tool(Tool::Step);
        model.pressed(point(5.0, 5.0));
        model.pressed(point(70.0, 70.0));
        model.pressed(point(140.0, 140.0));

        let numbers: Vec<u32> = model
            .annotations()
            .iter()
            .map(|a| match a.shape {
                Shape::Step { number, .. } => number,
                _ => panic!("expected step"),
            })
            .collect();
        assert_eq!(numbers, vec![1, 2, 3]);

        // Deleting the middle one and placing a new one continues from 4,
        // not from a renumbered 3 — see `EditorModel::next_step`'s doc
        // comment for why that's the deliberate choice.
        model.set_tool(Tool::Select);
        model.pressed(point(70.0, 70.0));
        model.delete_selected();
        model.set_tool(Tool::Step);
        model.pressed(point(140.0, 5.0));

        let numbers_after: Vec<u32> = model
            .annotations()
            .iter()
            .map(|a| match a.shape {
                Shape::Step { number, .. } => number,
                _ => panic!("expected step"),
            })
            .collect();
        assert_eq!(numbers_after, vec![1, 3, 4]);
    }

    #[test]
    fn step_shape_bounds_is_centered_on_its_position() {
        let shape = Shape::Step {
            position: point(20.0, 20.0),
            number: 1,
        };
        let bounds = shape_bounds(&shape);
        assert_eq!(bounds.x, 20.0 - STEP_BADGE_RADIUS);
        assert_eq!(bounds.y, 20.0 - STEP_BADGE_RADIUS);
        assert_eq!(bounds.width, STEP_BADGE_RADIUS * 2.0);
        assert_eq!(bounds.height, STEP_BADGE_RADIUS * 2.0);
    }

    // -- Stage 15: Blur/Pixelate tools (model-level) ---------------------------

    #[test]
    fn dragging_the_pixelate_tool_mutates_canvas_pixels_and_pushes_undo() {
        // A non-uniform base — pixelating a uniform region is a legitimate
        // no-op for the kernel itself (see `pixelate_region_leaves_a_
        // uniform_region_unchanged`), so `tiny_frame`'s all-zero pixels
        // would make this test assert the wrong thing.
        let mut pixels = vec![0u8; 40 * 40 * 4];
        for (i, chunk) in pixels.chunks_mut(4).enumerate() {
            let v = if i % 2 == 0 { 255 } else { 0 };
            chunk.copy_from_slice(&[v, v, v, 255]);
        }
        let checkerboard = Frame::new(40, 40, 1.0, pixels).expect("valid buffer");
        let mut model = EditorModel::new(checkerboard);
        let before = model.canvas().pixels().to_vec();
        model.set_tool(Tool::Pixelate);
        model.pressed(point(2.0, 2.0));
        model.pointer_moved(point(20.0, 20.0));
        let changed = model.released(point(20.0, 20.0));

        assert!(changed, "a real redaction drag must report a canvas change");
        assert_ne!(model.canvas().pixels(), before.as_slice());
        assert!(model.can_undo());

        model.undo();
        assert_eq!(model.canvas().pixels(), before.as_slice());
    }

    #[test]
    fn dragging_the_blur_tool_mutates_canvas_pixels_and_pushes_undo() {
        // A non-uniform base so a blur has something to smooth — a uniform
        // image is a poor test here (see `blur_region_leaves_a_uniform_
        // region_unchanged` above, which covers that case on the kernel
        // directly).
        let mut pixels = vec![0u8; 40 * 40 * 4];
        for (i, chunk) in pixels.chunks_mut(4).enumerate() {
            let v = if i % 2 == 0 { 255 } else { 0 };
            chunk.copy_from_slice(&[v, v, v, 255]);
        }
        let checkerboard = Frame::new(40, 40, 1.0, pixels).expect("valid buffer");
        let mut model = EditorModel::new(checkerboard);
        let before = model.canvas().pixels().to_vec();

        model.set_tool(Tool::Blur);
        model.pressed(point(2.0, 2.0));
        model.pointer_moved(point(30.0, 30.0));
        let changed = model.released(point(30.0, 30.0));

        assert!(changed);
        assert_ne!(model.canvas().pixels(), before.as_slice());
        assert!(model.can_undo());
    }

    #[test]
    fn a_tiny_redaction_drag_is_discarded_like_a_stray_click() {
        let mut model = EditorModel::new(tiny_frame(40, 40));
        model.set_tool(Tool::Blur);
        model.pressed(point(5.0, 5.0));
        let changed = model.released(point(5.2, 5.1));

        assert!(!changed);
        assert!(!model.can_undo());
    }

    #[test]
    fn released_returns_false_for_every_non_redaction_interaction() {
        let mut model = EditorModel::new(tiny_frame(40, 40));
        model.set_tool(Tool::Rectangle);
        model.pressed(point(2.0, 2.0));
        assert!(!model.released(point(20.0, 20.0)));

        model.set_tool(Tool::Crop);
        model.pressed(point(2.0, 2.0));
        assert!(!model.released(point(20.0, 20.0)));
    }

    // -- redact_pixel_rect ------------------------------------------------------

    #[test]
    fn redact_pixel_rect_clamps_into_the_canvas() {
        let clamped =
            redact_pixel_rect(rect(-10.0, -10.0, 30.0, 30.0), 20, 20).expect("overlaps the canvas");
        assert_eq!(
            clamped,
            PixelRect {
                x: 0,
                y: 0,
                width: 20,
                height: 20
            }
        );
    }

    #[test]
    fn redact_pixel_rect_rejects_a_rect_entirely_off_canvas() {
        assert_eq!(
            redact_pixel_rect(rect(100.0, 100.0, 10.0, 10.0), 20, 20),
            None
        );
    }

    // -- pixelate_region kernel ---------------------------------------------

    #[test]
    fn pixelate_region_flattens_a_block_to_its_average() {
        let width = 4u32;
        let height = 2u32;
        // Row 0: [0,0,0,255] [255,255,255,255] ...; a 2x2 block starting at
        // (0,0) should average to [128,128,128,255] (integer division).
        let mut buf = vec![0u8; (width * height * 4) as usize];
        let set = |buf: &mut [u8], x: u32, y: u32, v: u8| {
            let i = (y * width + x) as usize * 4;
            buf[i..i + 4].copy_from_slice(&[v, v, v, 255]);
        };
        set(&mut buf, 0, 0, 0);
        set(&mut buf, 1, 0, 255);
        set(&mut buf, 0, 1, 0);
        set(&mut buf, 1, 1, 255);

        pixelate_region(
            &mut buf,
            width,
            height,
            PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
            2,
        );

        for y in 0..2 {
            for x in 0..2 {
                let i = (y * width + x) as usize * 4;
                assert_eq!(&buf[i..i + 4], &[127, 127, 127, 255]);
            }
        }
    }

    #[test]
    fn pixelate_region_leaves_a_uniform_region_unchanged() {
        let width = 10u32;
        let height = 10u32;
        let original = vec![[42u8, 10, 200, 255]; (width * height) as usize].concat();
        let mut buf = original.clone();
        pixelate_region(
            &mut buf,
            width,
            height,
            PixelRect {
                x: 1,
                y: 1,
                width: 6,
                height: 6,
            },
            PIXELATE_BLOCK_PX,
        );
        assert_eq!(buf, original);
    }

    #[test]
    fn pixelate_region_only_touches_pixels_inside_the_rect() {
        let width = 10u32;
        let height = 10u32;
        let mut buf: Vec<u8> = (0..(width * height * 4)).map(|i| (i % 251) as u8).collect();
        let before = buf.clone();
        pixelate_region(
            &mut buf,
            width,
            height,
            PixelRect {
                x: 2,
                y: 2,
                width: 3,
                height: 3,
            },
            3,
        );

        for y in 0..height {
            for x in 0..width {
                let inside = (2..5).contains(&x) && (2..5).contains(&y);
                let i = (y * width + x) as usize * 4;
                if !inside {
                    assert_eq!(
                        &buf[i..i + 4],
                        &before[i..i + 4],
                        "pixel ({x},{y}) outside the rect changed"
                    );
                }
            }
        }
    }

    #[test]
    fn pixelate_region_does_not_panic_on_a_rect_extending_past_the_image() {
        let width = 5u32;
        let height = 5u32;
        let mut buf = vec![7u8; (width * height * 4) as usize];
        pixelate_region(
            &mut buf,
            width,
            height,
            PixelRect {
                x: 3,
                y: 3,
                width: 50,
                height: 50,
            },
            4,
        );
        // No panic is the assertion; a light sanity check that something
        // still looks like valid pixel data.
        assert_eq!(buf.len(), (width * height * 4) as usize);
    }

    #[test]
    fn pixelate_region_block_size_zero_does_not_infinite_loop_or_panic() {
        let width = 6u32;
        let height = 6u32;
        let mut buf = vec![3u8; (width * height * 4) as usize];
        pixelate_region(
            &mut buf,
            width,
            height,
            PixelRect {
                x: 0,
                y: 0,
                width: 6,
                height: 6,
            },
            0,
        );
        assert_eq!(buf.len(), (width * height * 4) as usize);
    }

    // -- blur_region kernel ---------------------------------------------------

    #[test]
    fn blur_region_leaves_a_uniform_region_unchanged() {
        let width = 20u32;
        let height = 20u32;
        let original = vec![[80u8, 80, 80, 255]; (width * height) as usize].concat();
        let mut buf = original.clone();
        blur_region(
            &mut buf,
            width,
            height,
            PixelRect {
                x: 3,
                y: 3,
                width: 10,
                height: 10,
            },
            BLUR_RADIUS_PX,
        );
        assert_eq!(buf, original);
    }

    #[test]
    fn blur_region_smooths_a_sharp_edge() {
        let width = 40u32;
        let height = 10u32;
        // Left half black, right half white.
        let mut buf = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let v = if x < width / 2 { 0 } else { 255 };
                let i = (y * width + x) as usize * 4;
                buf[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        blur_region(
            &mut buf,
            width,
            height,
            PixelRect {
                x: 0,
                y: 0,
                width,
                height,
            },
            6,
        );

        // A pixel right at the old boundary is now some mid-gray, not pure
        // black or pure white.
        let boundary_i = (5 * width + width / 2) as usize * 4;
        let boundary_value = buf[boundary_i];
        assert!(
            boundary_value > 10 && boundary_value < 245,
            "expected a blended gray at the boundary, got {boundary_value}"
        );
    }

    #[test]
    fn blur_region_only_touches_pixels_inside_the_rect() {
        let width = 20u32;
        let height = 20u32;
        let mut buf: Vec<u8> = (0..(width * height * 4)).map(|i| (i % 199) as u8).collect();
        let before = buf.clone();
        blur_region(
            &mut buf,
            width,
            height,
            PixelRect {
                x: 5,
                y: 5,
                width: 4,
                height: 4,
            },
            3,
        );

        for y in 0..height {
            for x in 0..width {
                let inside = (5..9).contains(&x) && (5..9).contains(&y);
                let i = (y * width + x) as usize * 4;
                if !inside {
                    assert_eq!(
                        &buf[i..i + 4],
                        &before[i..i + 4],
                        "pixel ({x},{y}) outside the rect changed"
                    );
                }
            }
        }
    }

    #[test]
    fn blur_region_does_not_panic_near_every_image_edge() {
        let width = 8u32;
        let height = 8u32;
        let mut buf = vec![99u8; (width * height * 4) as usize];
        // A rect that touches all four edges, with a blur radius larger
        // than the image itself — the padding math must clamp, not
        // underflow/overflow.
        blur_region(
            &mut buf,
            width,
            height,
            PixelRect {
                x: 0,
                y: 0,
                width,
                height,
            },
            50,
        );
        assert_eq!(buf.len(), (width * height * 4) as usize);
    }

    #[test]
    #[ignore = "manual perf check for the Stage 15 handoff — run with \
                `cargo test --release -- --ignored redaction_kernels_are_fast_at_4k --nocapture`"]
    fn redaction_kernels_are_fast_at_4k() {
        let width = 3840u32;
        let height = 2160u32;
        let rect_whole = PixelRect {
            x: 0,
            y: 0,
            width,
            height,
        };

        let mut buf = vec![128u8; width as usize * height as usize * 4];
        let start = std::time::Instant::now();
        pixelate_region(&mut buf, width, height, rect_whole, PIXELATE_BLOCK_PX);
        let pixelate_elapsed = start.elapsed();

        let mut buf = vec![128u8; width as usize * height as usize * 4];
        let start = std::time::Instant::now();
        blur_region(&mut buf, width, height, rect_whole, BLUR_RADIUS_PX);
        let blur_elapsed = start.elapsed();

        // A realistic redaction — a face, a license plate, a phone number —
        // on a 4K screenshot, not "the whole frame" (that's the worst case
        // above, deliberately included because it bounds the cost
        // regardless of how a user drags).
        let typical_rect = PixelRect {
            x: 1000,
            y: 900,
            width: 600,
            height: 400,
        };
        let mut buf = vec![128u8; width as usize * height as usize * 4];
        let start = std::time::Instant::now();
        pixelate_region(&mut buf, width, height, typical_rect, PIXELATE_BLOCK_PX);
        let typical_pixelate_elapsed = start.elapsed();

        let mut buf = vec![128u8; width as usize * height as usize * 4];
        let start = std::time::Instant::now();
        blur_region(&mut buf, width, height, typical_rect, BLUR_RADIUS_PX);
        let typical_blur_elapsed = start.elapsed();

        eprintln!(
            "4K canvas, whole-frame region (worst case): pixelate {pixelate_elapsed:?}, blur {blur_elapsed:?}"
        );
        eprintln!(
            "4K canvas, 600x400 region (typical redaction): pixelate {typical_pixelate_elapsed:?}, blur {typical_blur_elapsed:?}"
        );
    }

    // -- raster_text (plumbing only — see the module doc comment on why
    //    exact glyph output isn't asserted here) ------------------------------

    #[test]
    fn raster_text_does_not_panic_on_empty_or_degenerate_input() {
        let mut buf = vec![0u8; 20 * 20 * 4];
        let color = test_palette().terracotta;
        let request = |content: &'static str,
                       family: &'static str,
                       size_px: f32,
                       position: Point,
                       align: TextRasterAlign| {
            TextRasterRequest {
                content,
                family,
                size_px,
                color,
                position,
                align,
            }
        };
        raster_text(
            &mut buf,
            20,
            20,
            request(
                "",
                TEST_FONT_FAMILY,
                12.0,
                point(0.0, 0.0),
                TextRasterAlign::TopLeft,
            ),
        );
        raster_text(
            &mut buf,
            20,
            20,
            request(
                "   ",
                TEST_FONT_FAMILY,
                12.0,
                point(0.0, 0.0),
                TextRasterAlign::TopLeft,
            ),
        );
        raster_text(
            &mut buf,
            20,
            20,
            request(
                "x",
                TEST_FONT_FAMILY,
                0.0,
                point(0.0, 0.0),
                TextRasterAlign::TopLeft,
            ),
        );
        raster_text(
            &mut buf,
            20,
            20,
            request(
                "x",
                TEST_FONT_FAMILY,
                f32::NAN,
                point(0.0, 0.0),
                TextRasterAlign::TopLeft,
            ),
        );
        raster_text(
            &mut buf,
            20,
            20,
            request(
                "hi",
                TEST_FONT_FAMILY,
                12.0,
                point(-500.0, -500.0),
                TextRasterAlign::Center,
            ),
        );
        raster_text(
            &mut buf,
            20,
            20,
            request(
                "unresolvable-family-xyz",
                "definitely-not-a-real-font-family-xyz",
                12.0,
                point(0.0, 0.0),
                TextRasterAlign::TopLeft,
            ),
        );
    }

    // -- TextSizeScale --------------------------------------------------------

    #[test]
    fn text_size_scale_resolves_and_finds_the_nearest_stop() {
        let scale = TextSizeScale {
            small: 12.0,
            medium: 20.0,
            large: 40.0,
        };
        assert_eq!(scale.resolve(TextSizeStop::Small), 12.0);
        assert_eq!(scale.resolve(TextSizeStop::Medium), 20.0);
        assert_eq!(scale.resolve(TextSizeStop::Large), 40.0);

        assert_eq!(scale.nearest(13.0), TextSizeStop::Small);
        assert_eq!(scale.nearest(19.0), TextSizeStop::Medium);
        assert_eq!(scale.nearest(41.0), TextSizeStop::Large);
    }

    // -- parse_quality ----------------------------------------------------------

    #[test]
    fn parse_quality_clamps_and_falls_back_on_garbage() {
        assert_eq!(parse_quality("90", 50), 90);
        assert_eq!(parse_quality(" 100 ", 50), 100);
        assert_eq!(parse_quality("0", 50), 1);
        assert_eq!(parse_quality("99999", 50), 100);
        assert_eq!(parse_quality("not a number", 50), 50);
        assert_eq!(parse_quality("", 50), 50);
        assert_eq!(parse_quality("-5", 50), 50);
    }
}
