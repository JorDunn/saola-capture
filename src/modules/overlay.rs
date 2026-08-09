//! The region-selection overlay: the frozen-frame surface a `shot --region`
//! with no `--geometry` maps so the user can drag out a rectangle (PLAN.md
//! Stage 7; `docs/SAOLA-STYLE-GUIDE.md` §7's capture-overlay row —
//! "Dashed terracotta selection edge with round terracotta handles, size
//! readout, floating toolbar").
//!
//! # Freeze first, map second (the rule this module exists to honour)
//!
//! `zwlr_screencopy_v1` composites layer-shell surfaces
//! (`docs/CAPTURE-RESEARCH.md` §1.5, live-verified pre-plan: a `grim` taken
//! while niri's own screenshot UI was mapped captured that UI). So the
//! region flow captures the **whole output first**, hands the pixels to this
//! module as a frozen background, and crops that in-memory frame after the
//! user confirms. There is deliberately **no second screencopy** once the
//! overlay drops — the overlay would be in it. `dbus.rs`'s
//! `CaptureService::interactive_region` is the one place that sequence is
//! spelled out.
//!
//! # This module owns pixels and geometry, not the surface
//!
//! Same split every sibling module uses: `main.rs` owns the layer-shell
//! surface (`SurfaceRole::Overlay`, `overlay_surface_settings`) and the
//! reply channel back to the D-Bus method; [`Overlay`] owns only the
//! selection state, and answers with an [`Action`]. Everything geometric is
//! a **pure function** over [`Rect`]/[`Handle`] with no `Theme`, no iced
//! widget and no clock in it, which is what makes the drag model exhaustively
//! unit-testable without a compositor (PLAN.md Stage 7, task 2).
//!
//! # Three coordinate spaces, and where each one stops
//!
//! 1. **Surface-logical** — iced pointer coordinates, `(0, 0)` at the
//!    overlay surface's top-left. [`Rect`] and every pure function here are
//!    in this space. CAPTURE-RESEARCH §6.3 verified live (nested niri,
//!    injected pointer) that these land within 0.25 px of the injected
//!    logical position, so no fudge factor is needed.
//! 2. **Desktop-logical** — the space `--geometry WxH+X+Y`, `slurp` and
//!    niri's IPC speak. [`Overlay::to_logical`] converts by adding the
//!    output's own logical origin, and that is the last thing this module
//!    produces: an [`Action::Confirm`] carries a
//!    [`LogicalRect`](crate::capture::LogicalRect).
//! 3. **Physical pixels** — never computed here except for the readout;
//!    `capture::logical_to_pixel_rect` (already unit-tested against niri's
//!    own rounding in §1.4) owns that conversion for the actual crop.
//!
//! # The size readout counts *physical* pixels
//!
//! A 400×300 logical drag on Jordan's 1.5-scale eDP-1 saves a 600×450 image.
//! The readout shows **600 × 450** — the dimensions of the file the user is
//! about to get — not the logical drag size, because "how big is this
//! screenshot" is the question a size readout is asked. (On a scale-1 output
//! the two are identical, so this only shows up on fractional scale.)
//!
//! # saola-theme v0.5.0: what this surface needed and what it found
//!
//! Same posture as `modules::flash`/`modules::toast` (CLAUDE.md Design
//! language): derive from tokens that exist, spell the derivation out, and
//! name a genuine gap rather than quietly hardcoding.
//!
//! **Present, used verbatim** (all verified in `saola-tokens/src/`):
//! `scrim.capture` (the 62% ink dimming outside the selection, §2),
//! `radii.selection` (6 px, §4), `palette.accent` (the dashed edge and the
//! handles), `sizes.window_border` (2 px — the style guide's own "window
//! decoration is a 2px border and nothing else", reused here as the
//! selection edge's stroke width: it is the system's one *thin decorative
//! line* thickness), `container::popover` (the floating toolbar's opaque-ink
//! 30 px card with the popover shadow — §6's popover is exactly the
//! "floating panel of controls" this needs), `container::bar_pill` (the
//! readout's solid ink pill), `button::rest` (§6's at-rest ivory pill,
//! including its `Status::Disabled` arm — see [`toolbar`]).
//!
//! **Genuine design-token gaps** (flagged for a future consolidated
//! saola-theme pass, not upstreamed this stage, no tag bump):
//!
//! - No handle size. §7 says "round terracotta handles" with no dimension
//!   anywhere in the style guide. [`HANDLE_RADIUS`].
//! - No dash pattern for a dashed edge — §7 is the only place in the whole
//!   guide that asks for one. [`DASH_SEGMENTS`].
//! - No width for a small numeric readout pill. [`READOUT_WIDTH`].
//!
//! **Not gaps — interaction constants, which are deliberately not theme
//! tokens**: [`HANDLE_HIT_RADIUS`], [`EDGE_SNAP_DISTANCE`] and
//! [`MIN_SELECTION`] describe how the *pointer* behaves, not how anything
//! looks. A design system has no opinion on how close to an edge a drag
//! should snap; putting them in `saola-theme` would be miscategorising
//! behaviour as style.

use std::fmt;

use iced::widget::{button, canvas, container, image, mouse_area, row, text, Stack};
use iced::{keyboard, mouse, Element, Length, Padding, Point, Rectangle, Size};
use saola_theme::{ColorExt, Surface, Theme};

use crate::capture::{LogicalRect, OutputInfo, WindowRef};

// ---------------------------------------------------------------------
// Constants — see the module doc comment for which of these are theme
// gaps and which are deliberately not design tokens at all.
// ---------------------------------------------------------------------

/// The drawn radius of one round selection handle, in logical pixels. §7
/// asks for "round terracotta handles" and never sizes them; 5 px (a 10 px
/// dot) is small enough that eight of them don't crowd a modest selection
/// and large enough to read as a grab point next to a 2 px edge.
const HANDLE_RADIUS: f32 = 5.0;

/// How far from a handle's centre a press still counts as grabbing it.
/// Deliberately more than double [`HANDLE_RADIUS`]: the drawn dot is a
/// *hint*, and a selection edge is a one-pixel-precise thing to aim at.
/// Not a theme token — see the module doc comment.
const HANDLE_HIT_RADIUS: f32 = 12.0;

/// The selection edge's dash pattern: `[on, off]`, in logical pixels.
const DASH_SEGMENTS: [f32; 2] = [6.0, 4.0];

/// How close (logical px) an edge has to come to the output's own edge
/// before it snaps flush to it. This is what makes "select the whole left
/// half" land exactly on x = 0 instead of x = 1. Not a theme token.
const EDGE_SNAP_DISTANCE: f32 = 8.0;

/// The smallest selection, per side, that survives a mouse release. A press
/// with no real drag produces a 0×0 (or 1×2) rectangle that nobody wants
/// saved as an image, so anything under this clears the selection instead —
/// making a stray click a no-op rather than a one-pixel screenshot. Not a
/// theme token.
const MIN_SELECTION: f32 = 4.0;

/// The size readout pill's fixed width. Fixed rather than hugging its text
/// so the pill doesn't twitch as the digit count changes mid-drag — the
/// same reason the readout uses tabular numerals (IBM Plex's figures are
/// tabular by default; see `saola_theme::convert::ui_font`).
const READOUT_WIDTH: f32 = 136.0;

// ---------------------------------------------------------------------
// The geometry core — pure, `Theme`-free, exhaustively unit-tested
// ---------------------------------------------------------------------

/// A rectangle in **surface-logical** coordinates (see the module doc
/// comment's coordinate-space list). Always normalized: `width`/`height` are
/// never negative, because every constructor here goes through
/// [`Rect::from_corners`] or takes already-ordered edges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Rect {
            x,
            y,
            width: width.max(0.0),
            height: height.max(0.0),
        }
    }

    /// The rectangle spanned by two corners, in either order — a drag that
    /// goes up-and-left produces the same rectangle as the same drag
    /// reversed.
    pub fn from_corners(a: Point, b: Point) -> Self {
        let left = a.x.min(b.x);
        let top = a.y.min(b.y);
        Rect::new(left, top, (b.x - a.x).abs(), (b.y - a.y).abs())
    }

    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    /// Inclusive of the edges — a press exactly on the boundary counts as
    /// inside, which is what makes a 0-width selection still grabbable.
    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x
            && point.x <= self.right()
            && point.y >= self.y
            && point.y <= self.bottom()
    }

    pub fn translated(&self, dx: f32, dy: f32) -> Self {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
    }
}

/// Which horizontal edge a [`Handle`] drags, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HEdge {
    Left,
    None,
    Right,
}

/// Which vertical edge a [`Handle`] drags, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VEdge {
    Top,
    None,
    Bottom,
}

impl HEdge {
    /// The edge a handle becomes when the drag crosses the opposite edge —
    /// see [`resize`]'s doc comment on flipping.
    fn flipped(self) -> Self {
        match self {
            HEdge::Left => HEdge::Right,
            HEdge::None => HEdge::None,
            HEdge::Right => HEdge::Left,
        }
    }
}

impl VEdge {
    fn flipped(self) -> Self {
        match self {
            VEdge::Top => VEdge::Bottom,
            VEdge::None => VEdge::None,
            VEdge::Bottom => VEdge::Top,
        }
    }
}

/// One of §7's eight round drag handles: four corners and four edge
/// midpoints.
///
/// Modelled as a *pair of edges* rather than eight opaque cases, because
/// every operation on a handle (which edges does it move? what does it
/// become when the drag flips the rectangle inside out?) is per-axis. The
/// eight-way enum is what the rest of the code and the tests read; the
/// [`HEdge`]/[`VEdge`] decomposition is what [`resize`] actually computes
/// with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
    Top,
    Right,
    Bottom,
    Left,
}

impl Handle {
    /// **Corners first.** [`hit_test`] returns the first match, and on a
    /// small selection a corner's hit area overlaps its two neighbouring
    /// edge midpoints — a press near a corner should resize both axes,
    /// which is what the user aimed at.
    pub const ALL: [Handle; 8] = [
        Handle::TopLeft,
        Handle::TopRight,
        Handle::BottomRight,
        Handle::BottomLeft,
        Handle::Top,
        Handle::Right,
        Handle::Bottom,
        Handle::Left,
    ];

    fn edges(self) -> (HEdge, VEdge) {
        match self {
            Handle::TopLeft => (HEdge::Left, VEdge::Top),
            Handle::TopRight => (HEdge::Right, VEdge::Top),
            Handle::BottomRight => (HEdge::Right, VEdge::Bottom),
            Handle::BottomLeft => (HEdge::Left, VEdge::Bottom),
            Handle::Top => (HEdge::None, VEdge::Top),
            Handle::Right => (HEdge::Right, VEdge::None),
            Handle::Bottom => (HEdge::None, VEdge::Bottom),
            Handle::Left => (HEdge::Left, VEdge::None),
        }
    }

    fn from_edges(horizontal: HEdge, vertical: VEdge) -> Option<Handle> {
        Some(match (horizontal, vertical) {
            (HEdge::Left, VEdge::Top) => Handle::TopLeft,
            (HEdge::Right, VEdge::Top) => Handle::TopRight,
            (HEdge::Right, VEdge::Bottom) => Handle::BottomRight,
            (HEdge::Left, VEdge::Bottom) => Handle::BottomLeft,
            (HEdge::None, VEdge::Top) => Handle::Top,
            (HEdge::Right, VEdge::None) => Handle::Right,
            (HEdge::None, VEdge::Bottom) => Handle::Bottom,
            (HEdge::Left, VEdge::None) => Handle::Left,
            // A handle that drags neither axis is not a handle.
            (HEdge::None, VEdge::None) => return None,
        })
    }

    /// Where this handle is drawn on `rect`.
    pub fn center(self, rect: Rect) -> Point {
        let (horizontal, vertical) = self.edges();
        let x = match horizontal {
            HEdge::Left => rect.x,
            HEdge::None => rect.x + rect.width / 2.0,
            HEdge::Right => rect.right(),
        };
        let y = match vertical {
            VEdge::Top => rect.y,
            VEdge::None => rect.y + rect.height / 2.0,
            VEdge::Bottom => rect.bottom(),
        };
        Point::new(x, y)
    }

    /// The pointer shape that says what this handle does.
    fn interaction(self) -> mouse::Interaction {
        match self {
            Handle::TopLeft | Handle::BottomRight => mouse::Interaction::ResizingDiagonallyDown,
            Handle::TopRight | Handle::BottomLeft => mouse::Interaction::ResizingDiagonallyUp,
            Handle::Top | Handle::Bottom => mouse::Interaction::ResizingVertically,
            Handle::Left | Handle::Right => mouse::Interaction::ResizingHorizontally,
        }
    }
}

/// What lies under a point, for the press that is about to happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    /// Grab this handle and resize.
    Handle(Handle),
    /// Grab the selection body and move it.
    Inside,
    /// Start dragging out a new selection, discarding the old one.
    Outside,
}

/// `f32::clamp`, minus the four ways it can panic.
///
/// `f32::clamp` panics if `min > max` or if either bound is NaN — and
/// CLAUDE.md's no-panic rule has no exception for "a compositor would never
/// send us a NaN pointer position". A non-finite value collapses to `min`
/// (the inert answer: the top-left of whatever range was asked for), and an
/// inverted range answers `min` rather than asserting.
fn clamp_f32(value: f32, min: f32, max: f32) -> f32 {
    if !value.is_finite() || !min.is_finite() || !max.is_finite() || max < min {
        return if min.is_finite() { min } else { 0.0 };
    }
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// A point pinned inside `bounds`.
fn clamp_point(point: Point, bounds: Rect) -> Point {
    Point::new(
        clamp_f32(point.x, bounds.x, bounds.right()),
        clamp_f32(point.y, bounds.y, bounds.bottom()),
    )
}

/// What a press at `point` would grab, given the current `selection`.
///
/// With no selection there is nothing to grab, so every press starts a new
/// drag. `hit_radius` is a square half-extent rather than a true circular
/// radius — a corner's grab area being a square rather than a disc is
/// imperceptible and one comparison cheaper.
pub fn hit_test(selection: Option<Rect>, point: Point, hit_radius: f32) -> HitTarget {
    let Some(rect) = selection else {
        return HitTarget::Outside;
    };

    for handle in Handle::ALL {
        let center = handle.center(rect);
        if (point.x - center.x).abs() <= hit_radius && (point.y - center.y).abs() <= hit_radius {
            return HitTarget::Handle(handle);
        }
    }

    if rect.contains(point) {
        HitTarget::Inside
    } else {
        HitTarget::Outside
    }
}

/// Drags `handle` of `rect` to `pointer`, clamped inside `bounds`.
///
/// Returns the new rectangle **and the handle the drag is now holding**,
/// which is not always the one it started with: dragging the left edge past
/// the right edge turns the rectangle inside out, and the honest answer is
/// that the pointer is now holding the *right* edge. Returning the flipped
/// handle (rather than normalizing the rectangle and silently keeping the
/// old handle) is what makes a drag that crosses over and comes back
/// symmetric instead of sticky — the caller stores it in
/// [`Interaction::Resizing`] and the next motion continues from the correct
/// edge.
pub fn resize(rect: Rect, handle: Handle, pointer: Point, bounds: Rect) -> (Rect, Handle) {
    let pointer = clamp_point(pointer, bounds);
    let (horizontal, vertical) = handle.edges();

    let mut left = rect.x;
    let mut top = rect.y;
    let mut right = rect.right();
    let mut bottom = rect.bottom();

    match horizontal {
        HEdge::Left => left = pointer.x,
        HEdge::Right => right = pointer.x,
        HEdge::None => {}
    }
    match vertical {
        VEdge::Top => top = pointer.y,
        VEdge::Bottom => bottom = pointer.y,
        VEdge::None => {}
    }

    let mut horizontal = horizontal;
    let mut vertical = vertical;
    if left > right {
        std::mem::swap(&mut left, &mut right);
        horizontal = horizontal.flipped();
    }
    if top > bottom {
        std::mem::swap(&mut top, &mut bottom);
        vertical = vertical.flipped();
    }

    let next = Rect::new(left, top, right - left, bottom - top);
    // `from_edges` only returns `None` for the (None, None) pair, which
    // cannot be produced by flipping a real handle — but the no-panic rule
    // wants a value, not an `unwrap`.
    let next_handle = Handle::from_edges(horizontal, vertical).unwrap_or(handle);
    (next, next_handle)
}

/// Slides `rect` so it sits inside `bounds` **without changing its size** —
/// the move-drag's clamp. A rectangle wider than `bounds` is pinned to the
/// left/top edge rather than stretched or rejected.
pub fn clamp_position(rect: Rect, bounds: Rect) -> Rect {
    let max_x = (bounds.right() - rect.width).max(bounds.x);
    let max_y = (bounds.bottom() - rect.height).max(bounds.y);
    Rect {
        x: clamp_f32(rect.x, bounds.x, max_x),
        y: clamp_f32(rect.y, bounds.y, max_y),
        ..rect
    }
}

/// Intersects `rect` with `bounds` — the shape-drag's clamp, which *may*
/// change the size (an edge dragged past the screen stops at the screen).
pub fn clamp_shape(rect: Rect, bounds: Rect) -> Rect {
    let left = rect.x.max(bounds.x);
    let top = rect.y.max(bounds.y);
    let right = rect.right().min(bounds.right());
    let bottom = rect.bottom().min(bounds.bottom());
    Rect::new(left, top, right - left, bottom - top)
}

/// Snaps any edge within `distance` of the corresponding edge of `bounds`
/// flush to it. Each edge snaps independently, so this can change the
/// rectangle's size — correct for a create/resize drag, wrong for a move
/// (which uses [`snap_position_to_edges`] instead).
pub fn snap_edges_to_bounds(rect: Rect, bounds: Rect, distance: f32) -> Rect {
    let snap = |value: f32, target: f32| {
        if (value - target).abs() <= distance {
            target
        } else {
            value
        }
    };
    let left = snap(rect.x, bounds.x);
    let top = snap(rect.y, bounds.y);
    let right = snap(rect.right(), bounds.right());
    let bottom = snap(rect.bottom(), bounds.bottom());
    Rect::new(left, top, right - left, bottom - top)
}

/// Snaps a rectangle flush to an edge of `bounds` by **translating** it, so
/// its size is preserved exactly — the move-drag's snap. Left/top win over
/// right/bottom when a rectangle is small enough for both to be in range,
/// which only happens on a selection nearly as large as the output.
pub fn snap_position_to_edges(rect: Rect, bounds: Rect, distance: f32) -> Rect {
    let mut out = rect;
    if (out.right() - bounds.right()).abs() <= distance {
        out = Rect {
            x: bounds.right() - out.width,
            ..out
        };
    }
    if (out.x - bounds.x).abs() <= distance {
        out = Rect { x: bounds.x, ..out };
    }
    if (out.bottom() - bounds.bottom()).abs() <= distance {
        out = Rect {
            y: bounds.bottom() - out.height,
            ..out
        };
    }
    if (out.y - bounds.y).abs() <= distance {
        out = Rect { y: bounds.y, ..out };
    }
    out
}

/// Rounds a rectangle onto the whole-logical-pixel grid, **rounding each
/// edge independently** and taking the size as the difference of the rounded
/// edges.
///
/// Exactly the rule `capture::logical_to_pixel_rect` uses for the
/// logical→physical step, and for the same reason (CAPTURE-RESEARCH §1.4):
/// `round(width)` of an unrounded origin drifts, while rounding the edges
/// makes two adjacent selections tile with no seam and no overlap. It also
/// means the number in the size readout is exactly the number that reaches
/// `Frame::crop` — a readout that rounds differently from the crop is a
/// readout that lies.
pub fn snap_to_pixels(rect: Rect) -> Rect {
    let left = rect.x.round();
    let top = rect.y.round();
    let right = rect.right().round();
    let bottom = rect.bottom().round();
    Rect::new(left, top, right - left, bottom - top)
}

/// The full create/resize pipeline: snap to the output's edges, round onto
/// the pixel grid, then pin inside the output. Applied on every motion (not
/// only on release) so the readout and the saved crop can never disagree.
pub fn settle_shape(rect: Rect, bounds: Rect) -> Rect {
    clamp_shape(
        snap_to_pixels(snap_edges_to_bounds(rect, bounds, EDGE_SNAP_DISTANCE)),
        bounds,
    )
}

/// The move pipeline: the same three steps, in size-preserving form.
pub fn settle_position(rect: Rect, bounds: Rect) -> Rect {
    let snapped = snap_position_to_edges(rect, bounds, EDGE_SNAP_DISTANCE);
    let rounded = Rect {
        x: snapped.x.round(),
        y: snapped.y.round(),
        ..snapped
    };
    clamp_position(rounded, bounds)
}

// ---------------------------------------------------------------------
// The overlay's state
// ---------------------------------------------------------------------

/// The frozen full-output capture, ready for iced to draw.
///
/// A newtype for one reason: `iced::widget::image::Handle` derives `Clone`,
/// `PartialEq` and `Eq` but **not** `Debug` (checked directly in
/// `iced_core-0.14.0/src/image.rs`), and this rides inside `main.rs`'s
/// `Message`, which `#[to_layer_message(multi)]` requires to derive `Debug`.
/// Same trick, same reason as `main.rs`'s `Thumbnail` — kept as a second,
/// separate newtype rather than reusing that one because a full-output
/// frozen frame and a 36 px toast thumbnail are different things that happen
/// to share a representation.
#[derive(Clone)]
pub struct FrozenFrame(image::Handle);

impl FrozenFrame {
    pub fn new(handle: image::Handle) -> Self {
        FrozenFrame(handle)
    }
}

impl fmt::Debug for FrozenFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FrozenFrame(..)")
    }
}

/// What the pointer is doing between a press and its release.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Interaction {
    Idle,
    /// Dragging out a brand-new selection from a fixed anchor corner.
    Creating {
        anchor: Point,
    },
    /// Sliding an existing selection. `grab` is where the press landed and
    /// `origin` the selection as it was then — every motion translates
    /// `origin` by the total delta rather than accumulating per-frame
    /// deltas, so rounding and clamping can never make a move drift.
    Moving {
        grab: Point,
        origin: Rect,
    },
    /// Dragging one handle. The handle can change mid-drag — see [`resize`].
    Resizing {
        handle: Handle,
    },
}

/// The region-selection overlay's whole state.
#[derive(Debug)]
pub struct Overlay {
    frame: FrozenFrame,
    /// The output this overlay covers — its logical origin (for
    /// [`Self::to_logical`]), its scale and its physical size (for the
    /// readout).
    output: OutputInfo,
    /// The output's logical size, as a rectangle at the surface origin.
    /// Everything the pure geometry functions see is clamped to this.
    bounds: Rect,
    selection: Option<Rect>,
    interaction: Interaction,
    cursor: Option<Point>,
    /// Whichever window was focused at the moment the frozen frame was
    /// taken (PLAN.md Stage 8) — resolved once, in `dbus.rs`'s
    /// `interactive_region`, and carried in rather than looked up here so
    /// this module stays free of any niri-ipc call of its own (the "pure
    /// geometry, `main.rs`/`dbus.rs` own the surface and the outside world"
    /// split the module doc comment describes). `None` when nothing was
    /// focused (or the lookup failed) — the toolbar's Window button renders
    /// disabled in that case, the same `Option`-gates-`on_press` pattern
    /// [`toolbar`] already uses for Capture.
    focused_window: Option<WindowRef>,
}

/// What [`Overlay::update`] asks `main.rs` to do — the same "return a value,
/// not a `Task`" shape `modules::toast::Action` uses, so the decision stays
/// testable without a compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// The user is done: crop this **desktop-logical** rectangle out of the
    /// frozen frame and save it.
    Confirm(LogicalRect),
    /// The toolbar's Window button: capture this window instead of
    /// anything cropped from the frozen frame (PLAN.md Stage 8). A
    /// genuinely different capture mechanism from `Confirm` above — see
    /// `dbus.rs`'s `interactive_region`, which branches on
    /// `RegionOutcome::SelectedWindow` to call
    /// `CaptureBackend::capture_window` fresh rather than crop anything.
    ConfirmWindow(WindowRef),
    /// Escape, or the toolbar's Cancel — unmap and save nothing.
    Cancel,
}

/// The overlay's message type, nested into `main.rs`'s `Message` as
/// `Message::Overlay(..)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Message {
    /// The pointer moved to a surface-logical position.
    CursorMoved(Point),
    /// The left button went down, at the last known cursor position (iced's
    /// `ButtonPressed` carries no coordinates of its own).
    Pressed,
    /// The left button came up.
    Released,
    /// Enter, or the toolbar's Capture button.
    Confirm,
    /// Escape, or the toolbar's Cancel button.
    Cancel,
    /// The toolbar's "Full screen" button: take the whole output instead.
    SelectFullOutput,
    /// The toolbar's "Window" button: capture whichever window was focused
    /// when the overlay's frozen frame was taken, instead of anything
    /// dragged out on this surface (PLAN.md Stage 8). A no-op if nothing
    /// was focused when the frame was frozen — the toolbar renders the
    /// button disabled in that case anyway, so this arm
    /// only covers a message somehow arriving without a live button (there
    /// isn't another way to send it today, but `Action::None` is the same
    /// honest answer `Message::Confirm`'s empty-selection arm gives).
    SelectWindow,
    /// A press that landed on the floating toolbar but not on one of its
    /// buttons — the card's own padding, the gaps between pills, or the
    /// disabled Window button.
    ///
    /// **This variant exists to be a no-op**, and it earns its place: iced's
    /// `button` only marks a press `Captured` when it *has* an `on_press`,
    /// so without something swallowing these the toolbar's own background
    /// would fall through to [`Message::Pressed`] and start dragging out a
    /// new selection underneath the toolbar — "you missed Cancel by three
    /// pixels, enjoy your new drag". A `mouse_area` around the whole bar
    /// (see [`toolbar`]) publishes this and captures the event; iced skips
    /// it entirely when a child button already captured, so a real button
    /// press is unaffected.
    ///
    /// The size readout deliberately does **not** do this: it floats over
    /// part of the screen the user might well want to select, and starting
    /// a drag there is the right answer.
    ToolbarPressed,
}

impl Overlay {
    pub fn new(frame: FrozenFrame, output: OutputInfo, focused_window: Option<WindowRef>) -> Self {
        let bounds = Rect::new(
            0.0,
            0.0,
            output.logical.width as f32,
            output.logical.height as f32,
        );
        Overlay {
            frame,
            output,
            bounds,
            selection: None,
            interaction: Interaction::Idle,
            cursor: None,
            focused_window,
        }
    }

    /// The current selection, in surface-logical coordinates — `None` until
    /// the first real drag, and again after a drag too small to be anything
    /// but a stray click.
    pub fn selection(&self) -> Option<Rect> {
        self.selection
    }

    pub fn update(&mut self, message: Message) -> Action {
        match message {
            Message::CursorMoved(position) => {
                self.cursor = Some(position);
                self.drag_to(position);
                Action::None
            }
            Message::Pressed => {
                self.press();
                Action::None
            }
            Message::Released => {
                self.release();
                Action::None
            }
            Message::Confirm => match self.selection {
                Some(rect) => Action::Confirm(self.to_logical(rect)),
                // Confirm with nothing selected is a no-op, not a
                // whole-screen capture: silently capturing something the
                // user never selected is exactly the "looks like success,
                // isn't" failure CLAUDE.md's no-panic rule is aimed at. The
                // toolbar renders its Capture button disabled in this state
                // anyway; this arm covers the Enter key.
                None => Action::None,
            },
            Message::Cancel => Action::Cancel,
            Message::SelectFullOutput => Action::Confirm(self.output.logical),
            Message::SelectWindow => match self.focused_window {
                Some(window) => Action::ConfirmWindow(window),
                None => Action::None,
            },
            // Deliberately nothing — see the variant's doc comment. It has
            // already done its whole job by being published (which is what
            // marks the event captured).
            Message::ToolbarPressed => Action::None,
        }
    }

    fn press(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        let point = clamp_point(cursor, self.bounds);
        match hit_test(self.selection, point, HANDLE_HIT_RADIUS) {
            HitTarget::Handle(handle) => {
                self.interaction = Interaction::Resizing { handle };
            }
            HitTarget::Inside => {
                if let Some(origin) = self.selection {
                    self.interaction = Interaction::Moving {
                        grab: point,
                        origin,
                    };
                }
            }
            HitTarget::Outside => {
                self.interaction = Interaction::Creating { anchor: point };
                // The old selection goes away the instant a new drag starts;
                // the first motion is what creates the replacement.
                self.selection = None;
            }
        }
    }

    fn drag_to(&mut self, position: Point) {
        let point = clamp_point(position, self.bounds);
        match self.interaction {
            Interaction::Idle => {}
            Interaction::Creating { anchor } => {
                let rect = Rect::from_corners(anchor, point);
                self.selection = Some(settle_shape(rect, self.bounds));
            }
            Interaction::Moving { grab, origin } => {
                let moved = origin.translated(point.x - grab.x, point.y - grab.y);
                self.selection = Some(settle_position(moved, self.bounds));
            }
            Interaction::Resizing { handle } => {
                if let Some(rect) = self.selection {
                    let (next, next_handle) = resize(rect, handle, point, self.bounds);
                    self.interaction = Interaction::Resizing {
                        handle: next_handle,
                    };
                    self.selection = Some(settle_shape(next, self.bounds));
                }
            }
        }
    }

    fn release(&mut self) {
        self.interaction = Interaction::Idle;
        if let Some(rect) = self.selection {
            if rect.width < MIN_SELECTION || rect.height < MIN_SELECTION {
                self.selection = None;
            }
        }
    }

    /// Surface-logical → desktop-logical: add the output's own origin.
    ///
    /// `as` casts on floats saturate at the integer type's bounds in Rust
    /// (they have since 1.45), so a nonsense rectangle clamps rather than
    /// wrapping — and `rect` has already been pinned inside [`Self::bounds`]
    /// by [`settle_shape`]/[`settle_position`] before it can get here.
    fn to_logical(&self, rect: Rect) -> LogicalRect {
        LogicalRect {
            x: self.output.logical.x.saturating_add(rect.x.round() as i32),
            y: self.output.logical.y.saturating_add(rect.y.round() as i32),
            width: (rect.width.round().max(1.0)) as u32,
            height: (rect.height.round().max(1.0)) as u32,
        }
    }

    /// The selection's size **in physical pixels** — the dimensions of the
    /// image the user is about to get. See the module doc comment.
    ///
    /// Falls back to the logical size if the conversion can't be made (an
    /// output with a nonsense scale, which `capture::logical_to_pixel_rect`
    /// answers `None` for): a readout showing the logical size is wrong by a
    /// scale factor, but a readout showing nothing at all is worse.
    fn readout_size(&self, rect: Rect) -> (u32, u32) {
        let logical = self.to_logical(rect);
        match crate::capture::logical_to_pixel_rect(logical, &self.output) {
            Some(pixels) => (pixels.width, pixels.height),
            None => (logical.width, logical.height),
        }
    }

    /// The whole surface: frozen frame, scrim + selection chrome, size
    /// readout, floating toolbar — bottom to top.
    pub fn view(&self, theme: &Theme) -> Element<'static, Message> {
        let background = image(self.frame.0.clone())
            .width(Length::Fill)
            .height(Length::Fill)
            // `Fill` (not `Contain`): the frozen frame is this exact
            // output's framebuffer, so its aspect ratio already matches the
            // surface to within the half-pixel that fractional scale
            // introduces. `Contain` would letterbox that half pixel into a
            // visible seam.
            .content_fit(iced::ContentFit::Fill);

        let painter = canvas(SelectionPainter {
            selection: self.selection,
            bounds: self.bounds,
            scrim: theme.scrim.capture.into_iced(),
            accent: theme.palette.accent.into_iced(),
            radius: theme.radii.selection,
            edge_width: theme.sizes.window_border,
            handle_radius: HANDLE_RADIUS,
            hit_radius: HANDLE_HIT_RADIUS,
        })
        .width(Length::Fill)
        .height(Length::Fill);

        let mut layers: Vec<Element<'static, Message>> = vec![background.into(), painter.into()];
        if let Some(rect) = self.selection() {
            layers.push(self.readout(theme, rect));
        }
        layers.push(toolbar(
            theme,
            self.selection().is_some(),
            self.focused_window.is_some(),
        ));

        Stack::with_children(layers)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// The size readout, floated just above the selection's top-left corner
    /// (and just inside it, when the selection is too close to the top of
    /// the output for the pill to fit above).
    fn readout(&self, theme: &Theme, rect: Rect) -> Element<'static, Message> {
        let (width, height) = self.readout_size(rect);
        let height_px = theme.sizes.panel_pill;
        let gap = theme.sizes.island_gap;

        let pill = container(
            text(format!("{width} × {height}"))
                .font(saola_theme::convert::ui_font(theme))
                .size(theme.typography.size.body)
                .color(theme.on_ink.primary.into_iced()),
        )
        .width(Length::Fixed(READOUT_WIDTH))
        .height(Length::Fixed(height_px))
        .align_x(iced::Center)
        .align_y(iced::Center)
        .style(saola_theme::style::container::bar_pill(theme));

        let above = rect.y - gap - height_px;
        let top = if above >= self.bounds.y {
            above
        } else {
            rect.y + gap
        };
        let left = clamp_f32(
            rect.x,
            self.bounds.x,
            (self.bounds.right() - READOUT_WIDTH).max(self.bounds.x),
        );

        container(pill)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::Left)
            .align_y(iced::Top)
            .padding(Padding {
                top: clamp_f32(top, self.bounds.y, self.bounds.bottom()),
                right: 0.0,
                bottom: 0.0,
                left,
            })
            .into()
    }
}

/// The floating toolbar: Capture / Cancel / Full screen / Window, anchored
/// bottom-centre.
///
/// **§11 item 2, walked deliberately**: every button here is `button::rest`
/// — §6's *at-rest ivory pill* — and **none** is the terracotta "primary"
/// variant, even though Capture is the live action. On this surface the one
/// terracotta element is the selection itself (its dashed edge and its eight
/// handles, which §7 names explicitly); painting Capture terracotta too
/// would put two terracotta things on one surface, which is the exact thing
/// §11 item 2 asks about. Item 3 ("is every control at rest ivory") is
/// satisfied by the same choice.
///
/// **The Window button, wired as of PLAN.md Stage 8.** It does *not* open a
/// picker — CAPTURE-RESEARCH D3's "documented v0.1 limitation" is that niri
/// exposes no pixel position for a tiled window, so a hover-to-highlight
/// list isn't implementable at all, and a click-through list surface on top
/// of an already-exclusive-keyboard overlay is a second UI this stage chose
/// not to build for a feature the research itself calls out as awkward.
/// Instead the button confirms **whichever window was focused when the
/// overlay's frozen frame was captured** — the same "by the focused window"
/// answer CAPTURE-RESEARCH D3 offers as the alternative to a list, and the
/// same resolution a bare `shot --window` (no `--window-id`) uses via
/// `CaptureBackend::focused_window`, so pressing this button and typing
/// `shot --window` from a terminal answer "which window?" identically. Read
/// as "capture the window I was just looking at instead of dragging a
/// region over it" — a real shortcut, not a stand-in for a picker that
/// didn't ship.
///
/// It renders **disabled** (no `on_press`, same `button::rest`
/// `Status::Disabled` arm §6 specifies — `fill_subtle` background, `disabled`
/// label) exactly when `has_focused_window` (below) is `false`: nothing was
/// focused (or the niri-ipc lookup failed) when the frame was frozen, so
/// there is nothing this button could honestly confirm. Visible-but-inert
/// beats either hiding it (undiscoverable) or leaving it clickable-but-silent
/// (the worst failure mode CLAUDE.md names).
fn toolbar(
    theme: &Theme,
    has_selection: bool,
    has_focused_window: bool,
) -> Element<'static, Message> {
    let capture = pill_button(theme, "Capture", has_selection.then_some(Message::Confirm));
    let cancel = pill_button(theme, "Cancel", Some(Message::Cancel));
    let fullscreen = pill_button(theme, "Full screen", Some(Message::SelectFullOutput));
    let window = pill_button(
        theme,
        "Window",
        has_focused_window.then_some(Message::SelectWindow),
    );

    let bar = container(row![capture, cancel, fullscreen, window].spacing(theme.sizes.island_gap))
        .padding(theme.sizes.island_gap)
        .style(saola_theme::style::container::popover(theme));

    // Swallow presses on the bar itself — see `Message::ToolbarPressed`.
    let bar = mouse_area(bar)
        .on_press(Message::ToolbarPressed)
        .interaction(mouse::Interaction::Idle);

    container(bar)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::Center)
        .align_y(iced::Bottom)
        .padding(Padding {
            top: 0.0,
            right: 0.0,
            bottom: theme.sizes.panel_margin_islands,
            left: 0.0,
        })
        .into()
}

/// One §6 pill button. `None` for `message` renders it disabled (see
/// [`toolbar`]).
///
/// Height is `sizes.hit_target_touch` (44 px) — §6 asks for "46–48px in
/// overlays" and 44 is the nearest existing token, the one that already
/// means "a comfortable non-bar hit target"; horizontal padding is
/// `sizes.popover_padding` (20 px), inside §6's own 16–22 px range.
fn pill_button(
    theme: &Theme,
    label: &'static str,
    message: Option<Message>,
) -> Element<'static, Message> {
    let content = container(
        text(label)
            .font(saola_theme::convert::ui_font(theme))
            .size(theme.typography.size.body),
    )
    .align_y(iced::Center)
    .height(Length::Fill);

    let mut widget = button(content)
        .height(Length::Fixed(theme.sizes.hit_target_touch))
        .padding(Padding {
            top: 0.0,
            right: theme.sizes.popover_padding,
            bottom: 0.0,
            left: theme.sizes.popover_padding,
        })
        .style(saola_theme::style::button::rest(theme, Surface::Ink));

    if let Some(message) = message {
        widget = widget.on_press(message);
    }

    widget.into()
}

// ---------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------

/// Translates one raw iced event into an overlay [`Message`].
///
/// `captured` is `iced::event::Status::Captured` — true when a widget on the
/// surface (in practice, a toolbar button) already handled the event. It is
/// consulted for **presses only**, and that is the whole reason the toolbar
/// can live inside the same surface as the drag area: without it, clicking
/// "Cancel" would also start dragging out a new selection underneath the
/// toolbar. A release is always forwarded (ending a drag that began on the
/// canvas but finished over the toolbar is correct, and a release with no
/// drag in progress is a no-op), and cursor motion is always forwarded (the
/// overlay has to keep knowing where the pointer is even while it is over a
/// button).
///
/// Keyboard events ignore `captured` entirely: nothing on this surface takes
/// keyboard focus, and Escape must work unconditionally — CAPTURE-RESEARCH
/// §6.6 flags multi-surface Escape arbitration as unspecified, and its
/// advice is "handle Escape identically on every surface".
pub fn message_from_event(event: &iced::Event, captured: bool) -> Option<Message> {
    match event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) => match key {
            keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::Cancel),
            keyboard::Key::Named(keyboard::key::Named::Enter) => Some(Message::Confirm),
            _ => None,
        },
        iced::Event::Mouse(mouse::Event::CursorMoved { position }) => {
            Some(Message::CursorMoved(*position))
        }
        iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if !captured => {
            Some(Message::Pressed)
        }
        iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
            Some(Message::Released)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------

/// The scrim, the dashed selection edge and the eight handles, drawn on a
/// `canvas` because none of the three is expressible as a widget:
/// `scrims.capture` has to be painted as four bands *around* an
/// arbitrary rectangle (a container can't have a hole), the edge is dashed
/// (no iced border style is), and the handles sit half outside the
/// rectangle they belong to.
///
/// Every field is a plain `Copy` value rather than a `&Theme`, so the
/// program is `'static` and `view` can return `Element<'static, _>` — the
/// same shape `modules::toast::ink_card_style` uses.
struct SelectionPainter {
    selection: Option<Rect>,
    bounds: Rect,
    scrim: iced::Color,
    accent: iced::Color,
    radius: f32,
    edge_width: f32,
    handle_radius: f32,
    hit_radius: f32,
}

impl canvas::Program<Message> for SelectionPainter {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());

        let Some(rect) = self.selection else {
            // Nothing selected yet: the whole output dims, which is what
            // says "you are in capture mode" before the first drag.
            frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.scrim);
            return vec![frame.into_geometry()];
        };

        // Four scrim bands around the selection, so the frozen frame shows
        // through the selection at full strength. Sizes are `max(0.0)` — a
        // selection flush against an edge produces a zero-height band, and a
        // negative one would be a silent painting bug.
        let (width, height) = (bounds.width, bounds.height);
        let (left, top, right, bottom) = (rect.x, rect.y, rect.right(), rect.bottom());
        let band = |frame: &mut canvas::Frame, x: f32, y: f32, w: f32, h: f32| {
            if w > 0.0 && h > 0.0 {
                frame.fill_rectangle(Point::new(x, y), Size::new(w, h), self.scrim);
            }
        };
        band(&mut frame, 0.0, 0.0, width, top);
        band(&mut frame, 0.0, bottom, width, (height - bottom).max(0.0));
        band(&mut frame, 0.0, top, left, (bottom - top).max(0.0));
        band(
            &mut frame,
            right,
            top,
            (width - right).max(0.0),
            (bottom - top).max(0.0),
        );

        let outline = canvas::Path::rounded_rectangle(
            Point::new(left, top),
            Size::new(rect.width, rect.height),
            self.radius.into(),
        );
        frame.stroke(
            &outline,
            canvas::Stroke {
                style: canvas::Style::Solid(self.accent),
                width: self.edge_width,
                line_cap: canvas::LineCap::Butt,
                line_join: canvas::LineJoin::Round,
                line_dash: canvas::LineDash {
                    segments: &DASH_SEGMENTS,
                    offset: 0,
                },
            },
        );

        for handle in Handle::ALL {
            frame.fill(
                &canvas::Path::circle(handle.center(rect), self.handle_radius),
                self.accent,
            );
        }

        vec![frame.into_geometry()]
    }

    /// The pointer shape follows [`hit_test`], so the cursor says what a
    /// press would do before it happens.
    fn mouse_interaction(
        &self,
        _state: &(),
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let Some(position) = cursor.position_in(bounds) else {
            return mouse::Interaction::None;
        };
        match hit_test(
            self.selection,
            clamp_point(position, self.bounds),
            self.hit_radius,
        ) {
            HitTarget::Handle(handle) => handle.interaction(),
            HitTarget::Inside => mouse::Interaction::Grab,
            HitTarget::Outside => mouse::Interaction::Crosshair,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::LogicalRect;

    fn bounds() -> Rect {
        Rect::new(0.0, 0.0, 800.0, 600.0)
    }

    /// The nested-niri output CAPTURE-RESEARCH §6 actually used: 825×971
    /// logical from a 1238×1457 mode at scale 1.5.
    fn nested_output() -> OutputInfo {
        OutputInfo {
            name: "winit".to_string(),
            logical: LogicalRect {
                x: 0,
                y: 0,
                width: 825,
                height: 971,
            },
            scale: 1.5,
            physical_width: 1238,
            physical_height: 1457,
        }
    }

    fn output_at(x: i32, y: i32) -> OutputInfo {
        OutputInfo {
            name: "DP-2".to_string(),
            logical: LogicalRect {
                x,
                y,
                width: 800,
                height: 600,
            },
            scale: 1.0,
            physical_width: 800,
            physical_height: 600,
        }
    }

    fn overlay() -> Overlay {
        Overlay::new(
            FrozenFrame::new(image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255])),
            output_at(0, 0),
            None,
        )
    }

    fn point(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    // -- Rect ------------------------------------------------------------

    #[test]
    fn from_corners_normalizes_every_drag_direction() {
        let expected = Rect::new(10.0, 20.0, 30.0, 40.0);
        assert_eq!(
            Rect::from_corners(point(10.0, 20.0), point(40.0, 60.0)),
            expected
        );
        assert_eq!(
            Rect::from_corners(point(40.0, 60.0), point(10.0, 20.0)),
            expected,
            "up-and-left"
        );
        assert_eq!(
            Rect::from_corners(point(40.0, 20.0), point(10.0, 60.0)),
            expected,
            "down-and-left"
        );
        assert_eq!(
            Rect::from_corners(point(10.0, 60.0), point(40.0, 20.0)),
            expected,
            "up-and-right"
        );
    }

    #[test]
    fn a_rect_never_has_a_negative_size() {
        let rect = Rect::new(10.0, 10.0, -5.0, -5.0);
        assert_eq!((rect.width, rect.height), (0.0, 0.0));
    }

    #[test]
    fn contains_includes_the_edges() {
        let rect = Rect::new(10.0, 10.0, 20.0, 20.0);
        assert!(rect.contains(point(10.0, 10.0)));
        assert!(rect.contains(point(30.0, 30.0)));
        assert!(rect.contains(point(20.0, 20.0)));
        assert!(!rect.contains(point(9.9, 20.0)));
        assert!(!rect.contains(point(20.0, 30.1)));
    }

    // -- clamp_f32 -------------------------------------------------------

    #[test]
    fn clamp_f32_never_panics_on_the_inputs_that_would_trip_std() {
        // `f32::clamp` panics on all three of these.
        assert_eq!(clamp_f32(f32::NAN, 0.0, 10.0), 0.0);
        assert_eq!(clamp_f32(5.0, 10.0, 0.0), 10.0, "inverted range");
        assert_eq!(clamp_f32(5.0, f32::NAN, 10.0), 0.0);
        assert_eq!(clamp_f32(f32::INFINITY, 0.0, 10.0), 0.0);
        // And it still clamps normally.
        assert_eq!(clamp_f32(-1.0, 0.0, 10.0), 0.0);
        assert_eq!(clamp_f32(11.0, 0.0, 10.0), 10.0);
        assert_eq!(clamp_f32(5.0, 0.0, 10.0), 5.0);
    }

    // -- handles ----------------------------------------------------------

    #[test]
    fn every_handle_sits_where_its_name_says() {
        let rect = Rect::new(100.0, 200.0, 40.0, 60.0);
        assert_eq!(Handle::TopLeft.center(rect), point(100.0, 200.0));
        assert_eq!(Handle::Top.center(rect), point(120.0, 200.0));
        assert_eq!(Handle::TopRight.center(rect), point(140.0, 200.0));
        assert_eq!(Handle::Right.center(rect), point(140.0, 230.0));
        assert_eq!(Handle::BottomRight.center(rect), point(140.0, 260.0));
        assert_eq!(Handle::Bottom.center(rect), point(120.0, 260.0));
        assert_eq!(Handle::BottomLeft.center(rect), point(100.0, 260.0));
        assert_eq!(Handle::Left.center(rect), point(100.0, 230.0));
    }

    #[test]
    fn handle_edges_round_trip_through_from_edges() {
        for handle in Handle::ALL {
            let (h, v) = handle.edges();
            assert_eq!(Handle::from_edges(h, v), Some(handle), "{handle:?}");
        }
        assert_eq!(Handle::from_edges(HEdge::None, VEdge::None), None);
    }

    // -- hit_test ---------------------------------------------------------

    #[test]
    fn hit_test_finds_every_handle() {
        let rect = Rect::new(100.0, 100.0, 200.0, 200.0);
        for handle in Handle::ALL {
            let center = handle.center(rect);
            assert_eq!(
                hit_test(Some(rect), center, HANDLE_HIT_RADIUS),
                HitTarget::Handle(handle),
                "{handle:?}"
            );
        }
    }

    #[test]
    fn hit_test_prefers_a_corner_when_a_tiny_selection_overlaps_its_handles() {
        // A 10x10 selection: every handle is within `HANDLE_HIT_RADIUS` of
        // every other. A press on the top-left corner must resize *both*
        // axes, which is why `Handle::ALL` lists corners first.
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(
            hit_test(Some(rect), point(0.0, 0.0), HANDLE_HIT_RADIUS),
            HitTarget::Handle(Handle::TopLeft)
        );
    }

    #[test]
    fn hit_test_distinguishes_inside_from_outside() {
        let rect = Rect::new(100.0, 100.0, 200.0, 200.0);
        assert_eq!(
            hit_test(Some(rect), point(200.0, 200.0), HANDLE_HIT_RADIUS),
            HitTarget::Inside
        );
        assert_eq!(
            hit_test(Some(rect), point(500.0, 500.0), HANDLE_HIT_RADIUS),
            HitTarget::Outside
        );
    }

    #[test]
    fn hit_test_with_no_selection_always_starts_a_new_drag() {
        assert_eq!(
            hit_test(None, point(0.0, 0.0), HANDLE_HIT_RADIUS),
            HitTarget::Outside
        );
    }

    // -- resize -----------------------------------------------------------

    #[test]
    fn each_handle_moves_exactly_the_edges_it_names() {
        let rect = Rect::new(100.0, 100.0, 100.0, 100.0);
        let b = bounds();

        let (out, _) = resize(rect, Handle::Left, point(50.0, 999.0), b);
        assert_eq!(out, Rect::new(50.0, 100.0, 150.0, 100.0));

        let (out, _) = resize(rect, Handle::Right, point(250.0, 0.0), b);
        assert_eq!(out, Rect::new(100.0, 100.0, 150.0, 100.0));

        let (out, _) = resize(rect, Handle::Top, point(999.0, 50.0), b);
        assert_eq!(out, Rect::new(100.0, 50.0, 100.0, 150.0));

        let (out, _) = resize(rect, Handle::Bottom, point(0.0, 250.0), b);
        assert_eq!(out, Rect::new(100.0, 100.0, 100.0, 150.0));

        let (out, _) = resize(rect, Handle::TopLeft, point(50.0, 50.0), b);
        assert_eq!(out, Rect::new(50.0, 50.0, 150.0, 150.0));

        let (out, _) = resize(rect, Handle::TopRight, point(250.0, 50.0), b);
        assert_eq!(out, Rect::new(100.0, 50.0, 150.0, 150.0));

        let (out, _) = resize(rect, Handle::BottomRight, point(250.0, 250.0), b);
        assert_eq!(out, Rect::new(100.0, 100.0, 150.0, 150.0));

        let (out, _) = resize(rect, Handle::BottomLeft, point(50.0, 250.0), b);
        assert_eq!(out, Rect::new(50.0, 100.0, 150.0, 150.0));
    }

    #[test]
    fn resize_keeps_its_handle_while_the_rect_stays_right_side_out() {
        let rect = Rect::new(100.0, 100.0, 100.0, 100.0);
        let (_, handle) = resize(rect, Handle::TopLeft, point(120.0, 120.0), bounds());
        assert_eq!(handle, Handle::TopLeft);
    }

    #[test]
    fn dragging_the_left_edge_past_the_right_flips_the_handle() {
        let rect = Rect::new(100.0, 100.0, 100.0, 100.0);
        let (out, handle) = resize(rect, Handle::Left, point(260.0, 150.0), bounds());
        assert_eq!(out, Rect::new(200.0, 100.0, 60.0, 100.0));
        assert_eq!(
            handle,
            Handle::Right,
            "the pointer now holds the right edge"
        );
    }

    #[test]
    fn dragging_the_top_edge_past_the_bottom_flips_the_handle() {
        let rect = Rect::new(100.0, 100.0, 100.0, 100.0);
        let (out, handle) = resize(rect, Handle::Top, point(150.0, 260.0), bounds());
        assert_eq!(out, Rect::new(100.0, 200.0, 100.0, 60.0));
        assert_eq!(handle, Handle::Bottom);
    }

    #[test]
    fn dragging_a_corner_past_the_opposite_one_flips_both_axes() {
        let rect = Rect::new(100.0, 100.0, 100.0, 100.0);
        let (out, handle) = resize(rect, Handle::TopLeft, point(260.0, 270.0), bounds());
        assert_eq!(out, Rect::new(200.0, 200.0, 60.0, 70.0));
        assert_eq!(handle, Handle::BottomRight);
    }

    #[test]
    fn resize_clamps_the_pointer_to_the_output() {
        let rect = Rect::new(100.0, 100.0, 100.0, 100.0);
        let (out, _) = resize(rect, Handle::BottomRight, point(9000.0, 9000.0), bounds());
        assert_eq!(out, Rect::new(100.0, 100.0, 700.0, 500.0));
        let (out, _) = resize(rect, Handle::TopLeft, point(-9000.0, -9000.0), bounds());
        assert_eq!(out, Rect::new(0.0, 0.0, 200.0, 200.0));
    }

    // -- clamping ---------------------------------------------------------

    #[test]
    fn clamp_position_slides_without_resizing() {
        let b = bounds();
        assert_eq!(
            clamp_position(Rect::new(-50.0, -50.0, 100.0, 100.0), b),
            Rect::new(0.0, 0.0, 100.0, 100.0)
        );
        assert_eq!(
            clamp_position(Rect::new(780.0, 580.0, 100.0, 100.0), b),
            Rect::new(700.0, 500.0, 100.0, 100.0)
        );
    }

    #[test]
    fn clamp_position_pins_a_rect_bigger_than_the_output_to_its_origin() {
        let b = bounds();
        assert_eq!(
            clamp_position(Rect::new(-10.0, -10.0, 2000.0, 2000.0), b),
            Rect::new(0.0, 0.0, 2000.0, 2000.0)
        );
    }

    #[test]
    fn clamp_shape_trims_instead_of_sliding() {
        let b = bounds();
        assert_eq!(
            clamp_shape(Rect::new(-50.0, -50.0, 100.0, 100.0), b),
            Rect::new(0.0, 0.0, 50.0, 50.0)
        );
        assert_eq!(
            clamp_shape(Rect::new(750.0, 550.0, 100.0, 100.0), b),
            Rect::new(750.0, 550.0, 50.0, 50.0)
        );
    }

    // -- snapping ---------------------------------------------------------

    #[test]
    fn edges_within_range_snap_flush_to_the_output() {
        let b = bounds();
        let snapped = snap_edges_to_bounds(Rect::new(3.0, 5.0, 790.0, 590.0), b, 8.0);
        assert_eq!(snapped, Rect::new(0.0, 0.0, 800.0, 600.0));
    }

    #[test]
    fn edges_outside_range_are_left_alone() {
        let b = bounds();
        let rect = Rect::new(20.0, 20.0, 100.0, 100.0);
        assert_eq!(snap_edges_to_bounds(rect, b, 8.0), rect);
    }

    #[test]
    fn only_the_near_edge_snaps() {
        let b = bounds();
        // Left edge 2 px off; right edge nowhere near.
        let snapped = snap_edges_to_bounds(Rect::new(2.0, 100.0, 100.0, 100.0), b, 8.0);
        assert_eq!(snapped, Rect::new(0.0, 100.0, 102.0, 100.0));
    }

    #[test]
    fn position_snapping_preserves_the_size_exactly() {
        let b = bounds();
        // Left edge 3 px from x=0; bottom edge (497 + 100 = 597) 3 px from
        // y=600. Both snap, and neither changes the size — which is the
        // whole difference between this and `snap_edges_to_bounds`.
        let snapped = snap_position_to_edges(Rect::new(3.0, 497.0, 100.0, 100.0), b, 8.0);
        assert_eq!(snapped.width, 100.0);
        assert_eq!(snapped.height, 100.0);
        assert_eq!(snapped.x, 0.0, "left edge snapped flush");
        assert_eq!(snapped.y, 500.0, "bottom edge snapped flush");
    }

    #[test]
    fn position_snapping_leaves_a_rect_hanging_off_the_edge_to_the_clamp() {
        let b = bounds();
        // 697 is nowhere near 600, so this is *not* a snap case — it is a
        // clamp case, and `snap_position_to_edges` must not quietly fix it.
        let rect = Rect::new(3.0, 597.0, 100.0, 100.0);
        let snapped = snap_position_to_edges(rect, b, 8.0);
        assert_eq!(snapped.y, 597.0, "untouched by the snap");
        assert_eq!(
            clamp_position(snapped, b).y,
            500.0,
            "the clamp is what fixes it"
        );
    }

    #[test]
    fn pixel_snapping_rounds_each_edge_independently() {
        // Two adjacent half-pixel columns must tile: edges at 0.4 -> 0,
        // 10.5 -> 11 (round-half-away-from-zero), 20.4 -> 20. Widths 11 and
        // 9, no gap, no overlap. `round(width)` would have given 10 and 10.
        let first = snap_to_pixels(Rect::new(0.4, 0.0, 10.1, 10.0));
        let second = snap_to_pixels(Rect::new(10.5, 0.0, 9.9, 10.0));
        assert_eq!((first.x, first.width), (0.0, 11.0));
        assert_eq!((second.x, second.width), (11.0, 9.0));
        assert_eq!(first.right(), second.x, "adjacent, with no seam");
    }

    #[test]
    fn settling_a_shape_always_lands_inside_the_output() {
        let b = bounds();
        for rect in [
            Rect::new(-100.0, -100.0, 5000.0, 5000.0),
            Rect::new(799.5, 599.5, 10.0, 10.0),
            Rect::new(0.0, 0.0, 0.0, 0.0),
        ] {
            let settled = settle_shape(rect, b);
            assert!(settled.x >= b.x, "{settled:?}");
            assert!(settled.y >= b.y, "{settled:?}");
            assert!(settled.right() <= b.right(), "{settled:?}");
            assert!(settled.bottom() <= b.bottom(), "{settled:?}");
        }
    }

    #[test]
    fn settling_a_position_keeps_the_size_and_lands_inside() {
        let b = bounds();
        let settled = settle_position(Rect::new(760.4, 30.6, 100.0, 100.0), b);
        assert_eq!((settled.width, settled.height), (100.0, 100.0));
        assert_eq!(settled.x, 700.0, "clamped flush against the right edge");
        assert_eq!(settled.y, 31.0, "rounded onto the pixel grid");
    }

    // -- the drag state machine --------------------------------------------

    #[test]
    fn a_full_create_drag_produces_the_dragged_rectangle() {
        let mut overlay = overlay();
        assert_eq!(
            overlay.update(Message::CursorMoved(point(100.0, 100.0))),
            Action::None
        );
        overlay.update(Message::Pressed);
        overlay.update(Message::CursorMoved(point(300.0, 250.0)));
        overlay.update(Message::Released);
        assert_eq!(
            overlay.selection(),
            Some(Rect::new(100.0, 100.0, 200.0, 150.0))
        );
    }

    #[test]
    fn a_backwards_drag_produces_the_same_rectangle() {
        let mut overlay = overlay();
        overlay.update(Message::CursorMoved(point(300.0, 250.0)));
        overlay.update(Message::Pressed);
        overlay.update(Message::CursorMoved(point(100.0, 100.0)));
        overlay.update(Message::Released);
        assert_eq!(
            overlay.selection(),
            Some(Rect::new(100.0, 100.0, 200.0, 150.0))
        );
    }

    #[test]
    fn a_click_with_no_drag_leaves_nothing_selected() {
        let mut overlay = overlay();
        overlay.update(Message::CursorMoved(point(100.0, 100.0)));
        overlay.update(Message::Pressed);
        overlay.update(Message::CursorMoved(point(101.0, 101.0)));
        overlay.update(Message::Released);
        assert_eq!(overlay.selection(), None, "a 1x1 rect is a stray click");
    }

    #[test]
    fn a_press_outside_discards_the_old_selection() {
        let mut overlay = overlay();
        overlay.update(Message::CursorMoved(point(100.0, 100.0)));
        overlay.update(Message::Pressed);
        overlay.update(Message::CursorMoved(point(200.0, 200.0)));
        overlay.update(Message::Released);
        assert!(overlay.selection().is_some());

        overlay.update(Message::CursorMoved(point(500.0, 500.0)));
        overlay.update(Message::Pressed);
        assert_eq!(
            overlay.selection(),
            None,
            "cleared the instant the new drag starts"
        );
    }

    #[test]
    fn dragging_the_middle_moves_the_selection() {
        let mut overlay = overlay();
        overlay.update(Message::CursorMoved(point(100.0, 100.0)));
        overlay.update(Message::Pressed);
        overlay.update(Message::CursorMoved(point(300.0, 300.0)));
        overlay.update(Message::Released);

        overlay.update(Message::CursorMoved(point(200.0, 200.0)));
        overlay.update(Message::Pressed);
        overlay.update(Message::CursorMoved(point(250.0, 230.0)));
        overlay.update(Message::Released);

        assert_eq!(
            overlay.selection(),
            Some(Rect::new(150.0, 130.0, 200.0, 200.0))
        );
    }

    #[test]
    fn a_move_that_leaves_the_output_is_clamped_not_resized() {
        let mut overlay = overlay();
        overlay.update(Message::CursorMoved(point(100.0, 100.0)));
        overlay.update(Message::Pressed);
        overlay.update(Message::CursorMoved(point(300.0, 300.0)));
        overlay.update(Message::Released);

        overlay.update(Message::CursorMoved(point(200.0, 200.0)));
        overlay.update(Message::Pressed);
        overlay.update(Message::CursorMoved(point(9000.0, 9000.0)));
        overlay.update(Message::Released);

        let selection = overlay.selection().expect("still selected");
        assert_eq!((selection.width, selection.height), (200.0, 200.0));
        assert_eq!((selection.x, selection.y), (600.0, 400.0));
    }

    #[test]
    fn grabbing_a_handle_resizes_rather_than_moves() {
        let mut overlay = overlay();
        overlay.update(Message::CursorMoved(point(100.0, 100.0)));
        overlay.update(Message::Pressed);
        overlay.update(Message::CursorMoved(point(300.0, 300.0)));
        overlay.update(Message::Released);

        // Grab the bottom-right corner and pull it out.
        overlay.update(Message::CursorMoved(point(300.0, 300.0)));
        overlay.update(Message::Pressed);
        overlay.update(Message::CursorMoved(point(400.0, 350.0)));
        overlay.update(Message::Released);

        assert_eq!(
            overlay.selection(),
            Some(Rect::new(100.0, 100.0, 300.0, 250.0))
        );
    }

    #[test]
    fn a_press_with_no_cursor_yet_does_nothing() {
        let mut overlay = overlay();
        overlay.update(Message::Pressed);
        overlay.update(Message::Released);
        assert_eq!(overlay.selection(), None);
    }

    // -- actions ------------------------------------------------------------

    #[test]
    fn a_press_swallowed_by_the_toolbar_changes_nothing() {
        let mut overlay = overlay();
        overlay.update(Message::CursorMoved(point(100.0, 100.0)));
        overlay.update(Message::Pressed);
        overlay.update(Message::CursorMoved(point(300.0, 250.0)));
        overlay.update(Message::Released);
        let before = overlay.selection();

        // The pointer wanders onto the toolbar and presses it.
        overlay.update(Message::CursorMoved(point(600.0, 560.0)));
        assert_eq!(overlay.update(Message::ToolbarPressed), Action::None);
        overlay.update(Message::CursorMoved(point(620.0, 560.0)));
        overlay.update(Message::Released);

        assert_eq!(
            overlay.selection(),
            before,
            "the selection survived a fumbled toolbar click"
        );
    }

    #[test]
    fn escape_cancels() {
        let mut overlay = overlay();
        assert_eq!(overlay.update(Message::Cancel), Action::Cancel);
    }

    #[test]
    fn confirm_without_a_selection_does_nothing() {
        let mut overlay = overlay();
        assert_eq!(overlay.update(Message::Confirm), Action::None);
    }

    #[test]
    fn confirm_reports_the_selection_in_desktop_logical_coordinates() {
        let mut overlay = Overlay::new(
            FrozenFrame::new(image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255])),
            // A second monitor whose origin is *not* the desktop origin.
            output_at(1706, 40),
            None,
        );
        overlay.update(Message::CursorMoved(point(100.0, 100.0)));
        overlay.update(Message::Pressed);
        overlay.update(Message::CursorMoved(point(300.0, 250.0)));
        overlay.update(Message::Released);

        assert_eq!(
            overlay.update(Message::Confirm),
            Action::Confirm(LogicalRect {
                x: 1806,
                y: 140,
                width: 200,
                height: 150,
            })
        );
    }

    #[test]
    fn the_fullscreen_button_confirms_the_whole_output() {
        let mut overlay = Overlay::new(
            FrozenFrame::new(image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255])),
            output_at(1706, 40),
            None,
        );
        assert_eq!(
            overlay.update(Message::SelectFullOutput),
            Action::Confirm(LogicalRect {
                x: 1706,
                y: 40,
                width: 800,
                height: 600,
            })
        );
    }

    // -- the window button (Stage 8) ----------------------------------------

    #[test]
    fn the_window_button_confirms_the_focused_window() {
        let mut overlay = Overlay::new(
            FrozenFrame::new(image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255])),
            output_at(0, 0),
            Some(WindowRef(7)),
        );
        assert_eq!(
            overlay.update(Message::SelectWindow),
            Action::ConfirmWindow(WindowRef(7))
        );
    }

    #[test]
    fn the_window_button_does_nothing_with_no_focused_window() {
        let mut overlay = overlay(); // built with `focused_window: None`
        assert_eq!(overlay.update(Message::SelectWindow), Action::None);
    }

    // -- the readout ---------------------------------------------------------

    #[test]
    fn the_readout_counts_physical_pixels_not_logical_ones() {
        let overlay = Overlay::new(
            FrozenFrame::new(image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255])),
            nested_output(),
            None,
        );
        // CAPTURE-RESEARCH §1.4's own worked example: logical 400x300 at
        // scale 1.5 is physical 600x450.
        assert_eq!(
            overlay.readout_size(Rect::new(100.0, 100.0, 400.0, 300.0)),
            (600, 450)
        );
    }

    #[test]
    fn the_readout_is_the_logical_size_on_an_unscaled_output() {
        let overlay = overlay();
        assert_eq!(
            overlay.readout_size(Rect::new(0.0, 0.0, 320.0, 240.0)),
            (320, 240)
        );
    }

    // -- event translation ----------------------------------------------------

    fn key(named: keyboard::key::Named) -> iced::Event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(named),
            modified_key: keyboard::Key::Named(named),
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        })
    }

    #[test]
    fn escape_and_enter_map_to_cancel_and_confirm() {
        assert_eq!(
            message_from_event(&key(keyboard::key::Named::Escape), false),
            Some(Message::Cancel)
        );
        assert_eq!(
            message_from_event(&key(keyboard::key::Named::Enter), false),
            Some(Message::Confirm)
        );
        assert_eq!(
            message_from_event(&key(keyboard::key::Named::Tab), false),
            None
        );
    }

    #[test]
    fn escape_still_cancels_when_a_widget_captured_the_event() {
        assert_eq!(
            message_from_event(&key(keyboard::key::Named::Escape), true),
            Some(Message::Cancel)
        );
    }

    #[test]
    fn a_press_captured_by_a_toolbar_button_never_starts_a_drag() {
        let press = iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        assert_eq!(message_from_event(&press, false), Some(Message::Pressed));
        assert_eq!(
            message_from_event(&press, true),
            None,
            "the button already handled it"
        );
    }

    #[test]
    fn releases_and_motion_are_always_forwarded() {
        let release = iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        assert_eq!(message_from_event(&release, true), Some(Message::Released));
        let moved = iced::Event::Mouse(mouse::Event::CursorMoved {
            position: point(4.0, 5.0),
        });
        assert_eq!(
            message_from_event(&moved, true),
            Some(Message::CursorMoved(point(4.0, 5.0)))
        );
    }

    #[test]
    fn other_buttons_and_events_are_ignored() {
        let right = iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right));
        assert_eq!(message_from_event(&right, false), None);
        let wheel = iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
        });
        assert_eq!(message_from_event(&wheel, false), None);
    }
}
