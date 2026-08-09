//! The `CaptureBackend` trait boundary and the pixel types that cross it
//! (PLAN.md Architecture, "Trait boundaries (binding)"; PLAN.md Stage 5,
//! task 1).
//!
//! # Why this trait exists at all (teaching note)
//!
//! CLAUDE.md's Boundaries section forbids xdg-desktop-portal outright and
//! sends every capture path straight at the compositor: stills through
//! `zwlr_screencopy_v1` ([`screencopy`]), video through niri's
//! `org.gnome.Mutter.ScreenCast` (`capture/screencast.rs`, Stage 10). That
//! is a deliberate, niri-shaped choice — and this trait is the single seam
//! where a *different* shape (a portal, a different compositor, a test
//! fake) could be substituted later without any caller changing. Everything
//! above this boundary — `storage.rs`, `dbus.rs`'s `Screenshot` method, the
//! `--no-daemon` CLI path, and eventually the overlay and the editor — only
//! ever sees [`Frame`]s and [`OutputInfo`]s, never a `wl_output`, never a
//! `zwlr_screencopy_frame_v1`, never a niri IPC socket.
//!
//! The practical rule from CLAUDE.md: **new capture paths go behind this
//! trait, never around it.**
//!
//! # Two coordinate systems, kept apart by the type system
//!
//! Mixing these up is the single most likely way to get a subtly wrong crop,
//! so they are two distinct types rather than two `(x, y, w, h)` tuples:
//!
//! - [`LogicalRect`] — **logical** (compositor) coordinates. This is what
//!   niri's own IPC reports, what `slurp`/`grim` print, and therefore what
//!   `saola-capture shot --region --geometry WxH+X+Y` means (CAPTURE-RESEARCH
//!   D2). On Jordan's laptop the single output is 2560×1600 physical at scale
//!   1.5, i.e. 1706×1066 *logical*.
//! - [`PixelRect`] — **physical** pixels, relative to the top-left of one
//!   [`Frame`]. This is what a crop actually operates on.
//!
//! [`logical_to_pixel_rect`] is the only sanctioned conversion between them,
//! and it applies CAPTURE-RESEARCH §1.4's verified rule (`round(logical *
//! scale)`, measured byte-exact against the compositor's own rounding).
//!
//! # What's implemented, stage by stage
//!
//! [`ScreencopyBackend`](screencopy::ScreencopyBackend) implements every
//! [`CaptureBackend`] method for real as of Stage 8: [`CaptureBackend::
//! outputs`]/[`focused_output`](CaptureBackend::focused_output)/
//! [`capture_output`](CaptureBackend::capture_output)/
//! [`capture_region`](CaptureBackend::capture_region) landed in Stage 5;
//! [`CaptureBackend::capture_window`] and [`CaptureBackend::
//! focused_window`] landed in Stage 8, via niri-ipc's
//! `Action::ScreenshotWindow` (CAPTURE-RESEARCH D3 — *not* a geometry crop;
//! niri exposes no pixel position for tiled windows). Interactive region
//! selection (a `--region` with no `--geometry`) needs the Stage 7 overlay
//! surface and is intercepted by `dbus.rs` before it ever reaches this
//! module; a `--no-daemon --region` with no `--geometry` has no surface to
//! map and reports a clean, actionable error instead of guessing a
//! rectangle.

pub mod screencopy;

use std::fmt;

use crate::cli::{CaptureOptions, ShotKind};

/// A rectangle in **logical** (compositor) coordinates — the coordinate
/// space `--geometry WxH+X+Y`, `slurp`, `grim` and niri's own IPC all speak.
///
/// `x`/`y` are signed because an output can sit left of or above the origin
/// in a multi-output layout; `width`/`height` are unsigned because a
/// zero-or-negative-sized rectangle is not a thing this app ever has a use
/// for (`cli::Geometry::parse` already rejects zero dimensions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogicalRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl LogicalRect {
    /// The exclusive right edge (`x + width`), in `i64` so a rectangle
    /// parked at `i32::MAX` cannot overflow into a wrong answer — every
    /// comparison this type does is done in `i64` for the same reason.
    fn right(&self) -> i64 {
        i64::from(self.x) + i64::from(self.width)
    }

    /// The exclusive bottom edge (`y + height`). See [`Self::right`].
    fn bottom(&self) -> i64 {
        i64::from(self.y) + i64::from(self.height)
    }

    /// Area of the overlap between two logical rectangles, in logical
    /// pixels² — used by [`output_for_region`] to pick which output a
    /// `--geometry` rectangle "belongs to" when a desktop spans several.
    /// Zero when they don't overlap at all.
    fn intersection_area(&self, other: &LogicalRect) -> u64 {
        let left = i64::from(self.x).max(i64::from(other.x));
        let top = i64::from(self.y).max(i64::from(other.y));
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        let width = (right - left).max(0) as u64;
        let height = (bottom - top).max(0) as u64;
        width * height
    }
}

/// A rectangle in **physical pixels**, relative to the top-left corner of a
/// [`Frame`]. This is what [`Frame::crop`] operates on, and the only rect
/// shape that is ever an index into pixel data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One connected output ("monitor"), as the capture layer sees it.
///
/// `name` (`"eDP-1"`, `"DP-2"`, …) is the **id** every [`CaptureBackend`]
/// method takes: it is stable, human-readable, the same string niri's IPC
/// and `wl_output`'s own `name` event use, and the same string
/// `iced_layershell`'s `OutputOption::OutputName` will want when Stage 7
/// spawns a per-output overlay surface (CAPTURE-RESEARCH D9).
///
/// `scale` is a **fractional** `f64` (1.5 on Jordan's laptop), not
/// `wl_output`'s integer `scale` event — see
/// [`screencopy::ScreencopyBackend`]'s doc comment for why the integer one
/// is not usable for coordinate math and where the fractional value comes
/// from.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputInfo {
    pub name: String,
    /// Where this output sits in the logical desktop, and how big it is
    /// there.
    pub logical: LogicalRect,
    /// Physical pixels per logical pixel. Always finite and `> 0`.
    pub scale: f64,
    /// The output's size in **physical** pixels — the size a full-output
    /// [`Frame`] from it will have.
    ///
    /// Carried separately rather than computed as `logical * scale` because
    /// the two disagree by a pixel or two in the fractional-scale case, and
    /// the disagreement is not in our favour: niri reports eDP-1's 2560×1600
    /// panel as logical **1706×1066** at scale 1.5, and `round(1706 * 1.5)`
    /// is 2559, not 2560. Clamping a `--geometry` against that derived value
    /// would quietly shave the last column off any region dragged to the
    /// right edge. `wl_output`'s own `mode` event knows the true number, so
    /// [`logical_to_pixel_rect`] uses it instead.
    pub physical_width: u32,
    pub physical_height: u32,
}

/// A captured image: **RGBA8, tightly packed, row 0 is the top row**, plus
/// its physical size and the scale of the output it came from — exactly the
/// shape PLAN.md's Architecture specifies ("`Frame` is RGBA8 + dimensions +
/// scale").
///
/// Three invariants every producer of a `Frame` must uphold, and every
/// consumer may rely on (all three are enforced by [`Frame::new`], which is
/// the only way to build one):
///
/// 1. `pixels.len() == width * height * 4` — **no stride padding**. The
///    compositor's own buffer very much can have padding (`stride` is the
///    authority there, per CAPTURE-RESEARCH D1); [`screencopy`] strips it
///    while swizzling, so nothing above this boundary has to think about it.
/// 2. Channel order is R, G, B, A — the compositor hands out B, G, R, X;
///    the swizzle happens below this boundary too.
/// 3. Row 0 is the **top** row. If a compositor ever sets the screencopy
///    `y_invert` flag (niri never does — §1.2), the backend un-inverts
///    before building the `Frame`.
///
/// Alpha is always `0xff`. Screenshots are opaque by construction: niri only
/// ever offers `Xrgb8888` (no alpha channel at all, D1), and even on a
/// compositor that offered an alpha format, a *screenshot* of a composited
/// desktop is opaque — carrying a stray transparency through to a saved PNG
/// would be a surprise, not a feature.
// No `Eq`: `scale` is an `f64`, and `f64` is deliberately not `Eq` in Rust
// (NaN != NaN). `PartialEq` is all the tests need, and `Frame::new` already
// guarantees `scale` is never NaN anyway.
#[derive(Clone, PartialEq)]
pub struct Frame {
    width: u32,
    height: u32,
    scale: f64,
    pixels: Vec<u8>,
}

impl fmt::Debug for Frame {
    /// Hand-written so a `Frame` in an error message or a test failure
    /// prints `Frame { 2560x1600, scale 1.5, 16384000 bytes }` instead of
    /// sixteen megabytes of byte literals.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Frame {{ {}x{}, scale {}, {} bytes }}",
            self.width,
            self.height,
            self.scale,
            self.pixels.len()
        )
    }
}

impl Frame {
    /// Builds a frame, validating invariant 1 above. Returns `None` — never
    /// panics, never truncates — if `pixels` isn't exactly
    /// `width * height * 4` bytes, or if either dimension is zero.
    ///
    /// A `scale` that isn't finite and positive is clamped to `1.0` rather
    /// than rejected: a nonsense scale from a misbehaving compositor should
    /// degrade a *crop* (which is what scale feeds), not lose an otherwise
    /// perfectly good screenshot.
    pub fn new(width: u32, height: u32, scale: f64, pixels: Vec<u8>) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        if pixels.len() != expected {
            return None;
        }
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        Some(Frame {
            width,
            height,
            scale,
            pixels,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// The RGBA8 bytes, tightly packed (`width * 4` per row, top row first).
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Bytes per row. Always `width * 4` — see invariant 1.
    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }

    /// The whole frame as a [`PixelRect`], i.e. the identity crop.
    fn bounds(&self) -> PixelRect {
        PixelRect {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        }
    }

    /// Crops to `rect`, **clamped to the frame's own bounds**.
    ///
    /// Clamping rather than rejecting is deliberate: a `--geometry` that
    /// hangs off the right edge of the screen (or a drag that ends outside
    /// the output, once Stage 7's overlay exists) should produce the part
    /// that *is* on screen, which is unambiguously what the user meant —
    /// the same thing `grim -g` does. Only a rectangle that misses the frame
    /// **entirely** (or is zero-sized) has no sensible answer, and that
    /// returns `None`.
    ///
    /// The result is a new tightly-packed frame carrying the same `scale`
    /// (a crop doesn't change how many physical pixels a logical one is).
    pub fn crop(&self, rect: PixelRect) -> Option<Frame> {
        let clamped = clamp_rect(rect, self.bounds())?;

        let src_stride = self.stride();
        let dst_stride = (clamped.width as usize).checked_mul(4)?;
        let mut out = Vec::with_capacity(dst_stride.checked_mul(clamped.height as usize)?);

        for row in 0..clamped.height as usize {
            let src_y = clamped.y as usize + row;
            let start = src_y
                .checked_mul(src_stride)?
                .checked_add((clamped.x as usize).checked_mul(4)?)?;
            let end = start.checked_add(dst_stride)?;
            out.extend_from_slice(self.pixels.get(start..end)?);
        }

        Frame::new(clamped.width, clamped.height, self.scale, out)
    }
}

/// Intersects `rect` with `bounds`, returning `None` if the overlap is
/// empty. Pulled out of [`Frame::crop`] as a free function so the clamping
/// rule is unit-testable on its own, with no 16 MB buffer in the way.
fn clamp_rect(rect: PixelRect, bounds: PixelRect) -> Option<PixelRect> {
    let left = rect.x.max(bounds.x);
    let top = rect.y.max(bounds.y);
    // `u32 + u32` can overflow, so the edges are computed in `u64`.
    let right = (u64::from(rect.x) + u64::from(rect.width))
        .min(u64::from(bounds.x) + u64::from(bounds.width));
    let bottom = (u64::from(rect.y) + u64::from(rect.height))
        .min(u64::from(bounds.y) + u64::from(bounds.height));

    if right <= u64::from(left) || bottom <= u64::from(top) {
        return None;
    }

    Some(PixelRect {
        x: left,
        y: top,
        // Both differences are `<= u32::MAX` because `right`/`bottom` are
        // themselves clamped to `bounds`, whose edges fit in `u32`.
        width: (right - u64::from(left)) as u32,
        height: (bottom - u64::from(top)) as u32,
    })
}

/// Converts a **logical** rectangle (a `--geometry`, or a future overlay
/// drag) into the **physical** pixel rectangle to crop out of `output`'s
/// full-output frame.
///
/// Two things happen here, and both are load-bearing:
///
/// 1. **Rebasing.** `region` is in whole-desktop logical coordinates
///    (`grim -g "100,100 400x300"` means "100 logical px from the left edge
///    of the *desktop*"), but a [`Frame`] starts at its own output's
///    top-left. So the output's own logical origin is subtracted first.
/// 2. **Scaling, per CAPTURE-RESEARCH §1.4.** Each *edge* is rounded
///    independently (`round(logical * scale)`), then the width is the
///    difference of the rounded edges — not `round(width * scale)`. That
///    matters: it is what makes adjacent regions tile without a one-pixel
///    seam or overlap, and it is exactly what niri's own
///    `to_physical_precise_round` does (verified byte-exact in §1.4, where
///    logical `100,100 400x300` at scale 1.5 produced physical `150,150
///    600x450`).
///
/// Returns `None` if the region lies entirely off `output`, or if the
/// output's scale is not a usable number.
pub fn logical_to_pixel_rect(region: LogicalRect, output: &OutputInfo) -> Option<PixelRect> {
    if !output.scale.is_finite() || output.scale <= 0.0 {
        return None;
    }

    let rel_left = i64::from(region.x) - i64::from(output.logical.x);
    let rel_top = i64::from(region.y) - i64::from(output.logical.y);
    let rel_right = rel_left + i64::from(region.width);
    let rel_bottom = rel_top + i64::from(region.height);

    let to_physical = |value: i64| -> i64 { (value as f64 * output.scale).round() as i64 };

    let left = to_physical(rel_left);
    let top = to_physical(rel_top);
    let right = to_physical(rel_right);
    let bottom = to_physical(rel_bottom);

    // Clamp against the *output's own* physical size before handing the
    // rect to `Frame::crop`, so a region hanging off the left/top edge
    // (negative physical coordinates, which `PixelRect`'s `u32` cannot even
    // represent) becomes "start at 0" rather than an underflow. The bound
    // is the output's true physical size (see [`OutputInfo::physical_width`]
    // for why that is *not* `logical * scale`); `Frame::crop` clamps a
    // second time against the frame actually delivered, so a compositor
    // that hands back a differently-sized buffer than its own mode event
    // advertised (a rotated output, say) still cannot produce an
    // out-of-bounds read.
    let out_width = i64::from(output.physical_width);
    let out_height = i64::from(output.physical_height);

    let left = left.clamp(0, out_width);
    let top = top.clamp(0, out_height);
    let right = right.clamp(0, out_width);
    let bottom = bottom.clamp(0, out_height);

    if right <= left || bottom <= top {
        return None;
    }

    Some(PixelRect {
        x: left as u32,
        y: top as u32,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    })
}

/// Which output a logical rectangle should be cropped out of: the one it
/// overlaps most.
///
/// "Most overlap" rather than "contains the origin" so that a region dragged
/// mostly onto the second monitor doesn't get cropped out of the first one
/// just because its top-left corner started there. Ties (including the
/// all-zero case where the region touches nothing) fall back to `None`, and
/// the caller reports a clean error rather than picking arbitrarily.
pub fn output_for_region(outputs: &[OutputInfo], region: LogicalRect) -> Option<&OutputInfo> {
    outputs
        .iter()
        .map(|output| (output, output.logical.intersection_area(&region)))
        .filter(|(_, area)| *area > 0)
        .max_by_key(|(_, area)| *area)
        .map(|(output, _)| output)
}

/// A window, as identified to [`CaptureBackend::capture_window`].
///
/// The wrapped `u64` is niri's own window id — CAPTURE-RESEARCH §5.1 proved
/// the id space is unified (`niri-ipc Window.id` ==
/// `ext_foreign_toplevel_handle_v1.identifier` == ScreenCast `window-id`,
/// all one `MappedId(u64)` inside niri), so one number is enough and no
/// per-mechanism handle type is needed. **Real as of Stage 8**:
/// `cli::CaptureOptions::window_id` (`--window-id`, the scriptable path),
/// [`CaptureBackend::focused_window`] (the no-picker default — CAPTURE-
/// RESEARCH D3's "or by the focused window"), and `modules::overlay`'s
/// Window toolbar button (the interactive path, same resolution) all
/// produce one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowRef(pub u64);

/// Every way a capture can fail.
///
/// Deliberately an enum of named cases rather than a `Box<dyn Error>`:
/// CLAUDE.md's no-panic rule is really a rule about *failure being visible
/// and actionable* ("a dead daemon means `Print` silently does nothing —
/// silent absence is the worst failure mode"), and named cases are what let
/// `Display` say something a human can act on. Every variant's message
/// names the thing that went wrong and, where there is one, the fix.
#[derive(Debug)]
pub enum CaptureError {
    /// No Wayland compositor to talk to (`$WAYLAND_DISPLAY` unset, or the
    /// socket refused the connection).
    Connect(String),
    /// The compositor doesn't advertise a global this backend needs.
    MissingGlobal(&'static str),
    /// The Wayland connection itself broke, or the compositor sent
    /// something unusable.
    Protocol(String),
    /// The compositor advertised no usable outputs at all.
    NoOutputs,
    /// The named output isn't one of the compositor's.
    UnknownOutput(String),
    /// The compositor answered the capture request with `failed`.
    Refused,
    /// The compositor never finished the handshake within the budget.
    Timeout,
    /// The compositor offered no shm buffer format this code understands.
    UnsupportedFormat(u32),
    /// The requested region doesn't overlap any output (or any pixels).
    EmptyRegion,
    /// Allocating or reading the shared-memory buffer failed.
    Io(std::io::Error),
    /// A capture kind whose implementation lands in a later stage. Carries
    /// the actionable "which stage" note, same discipline as
    /// `dbus::not_yet_implemented`.
    Unsupported(&'static str),
    /// `shot --window` with no `--window-id` and no window currently
    /// focused (or the daemon can't reach niri's IPC socket to ask) — there
    /// is nothing to capture. Distinct from [`Self::Unsupported`]: this is
    /// not a missing feature, it is a `--window` invocation with nothing to
    /// point at, same class of error as [`Self::EmptyRegion`].
    NoFocusedWindow,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::Connect(err) => {
                write!(f, "could not connect to the Wayland compositor: {err}")
            }
            CaptureError::MissingGlobal(name) => write!(
                f,
                "the compositor does not support {name} — saola-capture needs it for screenshots"
            ),
            CaptureError::Protocol(err) => write!(f, "Wayland protocol error: {err}"),
            CaptureError::NoOutputs => write!(f, "the compositor reported no outputs to capture"),
            CaptureError::UnknownOutput(name) => write!(f, "no output named {name:?}"),
            CaptureError::Refused => {
                write!(f, "the compositor refused the screencopy request")
            }
            CaptureError::Timeout => write!(
                f,
                "the compositor did not deliver a frame in time — is it still responding?"
            ),
            CaptureError::UnsupportedFormat(format) => write!(
                f,
                "the compositor offered only shm buffer format {format}, which saola-capture \
                 cannot read"
            ),
            CaptureError::EmptyRegion => write!(
                f,
                "the requested region does not overlap any output — check --geometry"
            ),
            CaptureError::Io(err) => write!(f, "shared-memory buffer error: {err}"),
            CaptureError::Unsupported(note) => f.write_str(note),
            CaptureError::NoFocusedWindow => write!(
                f,
                "no window is currently focused — pass --window-id, or focus a window first"
            ),
        }
    }
}

impl std::error::Error for CaptureError {}

/// The seam. Everything above it deals in [`Frame`]s and [`OutputInfo`]s;
/// everything below it deals in Wayland objects, niri IPC and pixels in the
/// compositor's byte order.
///
/// # Deviation from PLAN.md's four-method sketch, and why
///
/// Architecture lists `outputs()`, `capture_output(id, cursor)`,
/// `capture_region(id, rect, cursor)` and `capture_window(ref)`. This trait
/// has all four with those exact roles (`id` is the output *name* —
/// `"eDP-1"` — which is what [`OutputInfo::name`] carries), plus a fifth:
/// [`Self::focused_output`]. It earns its place because *something* has to
/// answer "which output does a bare `shot --fullscreen` mean?", and the only
/// honest answers are compositor-specific (niri's IPC knows which output has
/// focus; `wl_output` alone does not). Putting it here keeps that knowledge
/// **behind** the boundary; the alternative — the caller reaching around the
/// trait into niri-ipc — is exactly what CLAUDE.md's "new paths go behind
/// them, never around them" rule forbids.
///
/// # Why `&self` and not `&mut self`
///
/// A backend is a stateless handle: [`screencopy::ScreencopyBackend`] opens
/// a fresh Wayland connection per call and closes it again. That keeps the
/// daemon (which holds a backend for its whole life alongside iced's own
/// Wayland connection) from having to serialize captures behind a lock, and
/// it means a fake in a test is a plain value.
pub trait CaptureBackend {
    /// Every output the compositor currently has mapped, in no particular
    /// order.
    fn outputs(&self) -> Result<Vec<OutputInfo>, CaptureError>;

    /// The output a bare `shot --fullscreen` should capture — the focused
    /// one, or a stable fallback if focus can't be determined.
    fn focused_output(&self) -> Result<OutputInfo, CaptureError>;

    /// The window currently focused on the desktop, if any — what a bare
    /// `shot --window` (no `--window-id`) captures, and what `modules::
    /// overlay`'s Window toolbar button confirms with (Stage 8;
    /// CAPTURE-RESEARCH D3: "Picking is by list ... or by 'the focused
    /// window'" — this is the second option, chosen because niri exposes no
    /// pixel position for tiled windows, so a hover-to-highlight list picker
    /// isn't implementable anyway, per D3's own "documented v0.1
    /// limitation").
    ///
    /// `Ok(None)` for "nothing is focused" (a layer-shell surface has focus,
    /// the desktop is empty, or the compositor's IPC is unreachable) — not
    /// an error, since the caller's own answer to "no focused window" is
    /// already a clean, actionable error
    /// ([`CaptureError::NoFocusedWindow`]) and this method has nothing more
    /// specific to say.
    fn focused_window(&self) -> Result<Option<WindowRef>, CaptureError>;

    /// A whole output, at its physical resolution.
    fn capture_output(&self, output: &str, cursor: bool) -> Result<Frame, CaptureError>;

    /// A logical rectangle of an output.
    ///
    /// Implemented as *capture the whole output, then crop in memory*, per
    /// CAPTURE-RESEARCH D2 — not via the protocol's own
    /// `capture_output_region`. Two reasons, both from the research: the
    /// overlay needs the full frozen frame as its background anyway (so one
    /// capture serves both), and screencopy composites layer-shell surfaces
    /// (§1.5), which is why the capture must happen *before* any overlay
    /// maps rather than being re-issued per drag.
    fn capture_region(
        &self,
        output: &str,
        region: LogicalRect,
        cursor: bool,
    ) -> Result<Frame, CaptureError>;

    /// A single window, rendered by the compositor itself.
    ///
    /// **Real as of Stage 8** (`niri-ipc` `Action::ScreenshotWindow`,
    /// CAPTURE-RESEARCH D3) — see
    /// [`ScreencopyBackend::capture_window`](screencopy::ScreencopyBackend)
    /// for the implementation. Not a screencopy path at all: niri renders
    /// the window's own elements offscreen, so occlusion by other windows,
    /// decorations and fractional scale are all handled on the compositor
    /// side rather than by any crop math here.
    fn capture_window(&self, window: WindowRef, cursor: bool) -> Result<Frame, CaptureError>;
}

/// Runs the capture half of one `shot` invocation: honour `--delay`, pick
/// the target, and hand back the pixels. Saving, encoding, the clipboard and
/// the history index are `storage.rs`'s half — see
/// [`crate::storage::save_capture`].
///
/// This is the **one** function both `shot` paths go through: the daemon's
/// `Screenshot` D-Bus method and the in-process `--no-daemon` path (PLAN.md
/// Stage 5, task 4: "the same library calls, no UI"). Keeping it here rather
/// than duplicating the dispatch in `dbus.rs` and `main.rs` is what makes
/// "both ways" actually mean the same thing rather than two implementations
/// that drift.
///
/// Taking `&dyn CaptureBackend` (rather than a generic parameter) is
/// deliberate: it costs one vtable dispatch per screenshot — utterly
/// irrelevant next to a 0.3 s capture — and in exchange the function is not
/// monomorphized per backend, which keeps the fake used in this module's own
/// tests honest about calling exactly the same code the real backend does.
pub fn take_screenshot(
    backend: &dyn CaptureBackend,
    options: &CaptureOptions,
) -> Result<Frame, CaptureError> {
    match options.kind {
        // `freeze_focused_output` owns `--delay`/`delay` for this arm — see
        // its own doc comment. It used to be honoured *again* here first
        // (Stage 5/6/7's code), which meant every delayed `--fullscreen`
        // slept twice: once in this function, once more inside
        // `freeze_focused_output`. Stage 8 fixed it by giving delay exactly
        // one owner per shot kind, this one included, rather than a
        // blanket sleep at the top that some kinds needed and others
        // (this one!) accidentally paid for twice.
        ShotKind::Fullscreen => {
            let (frame, _output) = freeze_focused_output(backend, options)?;
            Ok(frame)
        }
        ShotKind::Region => {
            let Some(geometry) = options.geometry else {
                // Reachable only from `--no-daemon` as of Stage 7: the
                // daemon intercepts an interactive `--region` *before* this
                // function (`dbus::CaptureService::interactive_region`) and
                // maps `modules::overlay` instead. A `--no-daemon` process
                // has no iced event loop and maps no surfaces at all, by
                // design — it is the scriptable path — so there is nothing
                // for a later stage to "finish" here; the actionable answer
                // is to name the two ways to get a region without an
                // overlay. Checked *before* sleeping, unlike Stage 5-7's
                // version of this function, which slept out the whole delay
                // and then reported "can't do this" anyway.
                return Err(CaptureError::Unsupported(
                    "--region without --geometry needs the daemon's selection overlay, and \
                     --no-daemon maps no surfaces — drop --no-daemon, or pass \
                     --geometry WxH+X+Y",
                ));
            };
            sleep_for_delay(options.delay);
            let region = LogicalRect {
                x: geometry.x,
                y: geometry.y,
                width: geometry.width,
                height: geometry.height,
            };
            let outputs = backend.outputs()?;
            let output = output_for_region(&outputs, region).ok_or(CaptureError::EmptyRegion)?;
            backend.capture_region(&output.name, region, options.cursor)
        }
        ShotKind::Window => {
            // `--window-id` (scriptable, mirrors `--geometry`) skips the
            // focused-window lookup entirely; otherwise CAPTURE-RESEARCH
            // D3's no-picker default applies — see
            // `CaptureBackend::focused_window`'s doc comment.
            let window = match options.window_id {
                Some(id) => WindowRef(id),
                None => backend
                    .focused_window()?
                    .ok_or(CaptureError::NoFocusedWindow)?,
            };
            sleep_for_delay(options.delay);
            backend.capture_window(window, options.cursor)
        }
    }
}

/// The one place `--delay`/`delay`'s whole-second sleep happens outside
/// [`freeze_focused_output`] (which owns it for [`ShotKind::Fullscreen`],
/// and for the interactive-region freeze `dbus.rs` calls directly). A shot
/// kind that doesn't go through `freeze_focused_output` still owes its
/// caller the delay it asked for — this is what [`take_screenshot`]'s
/// `Region`-with-`--geometry` and `Window` arms call instead of
/// reimplementing the same three lines, and it is called *after* each arm's
/// own validation (a nonsense `--geometry`, no focused window), so a request
/// that was always going to fail doesn't first make the caller wait out the
/// whole delay to find that out.
fn sleep_for_delay(delay: u32) {
    if delay > 0 {
        std::thread::sleep(std::time::Duration::from_secs(u64::from(delay)));
    }
}

/// Captures the **whole focused output**, and hands back the output it came
/// from alongside the pixels.
///
/// This is the "freeze" half of Architecture's region flow (PLAN.md: "the
/// daemon captures the target output **first** — frozen frame — no race with
/// the overlay's own pixels, exact crop source — then maps the overlay").
/// Screencopy composites layer-shell surfaces (CAPTURE-RESEARCH §1.5), so a
/// capture taken *after* the overlay maps would contain the overlay; the
/// only correct order is this one, and there is deliberately no second
/// capture once the user confirms — [`crop_frozen_frame`] crops these very
/// bytes.
///
/// [`take_screenshot`]'s own `Fullscreen` arm is the same call, which is why
/// it lives here rather than in `dbus.rs`: a fullscreen shot and a region
/// shot's freeze are literally the same operation, and keeping them one
/// function is what stops the two from drifting (the `--delay` handling in
/// particular).
pub fn freeze_focused_output(
    backend: &dyn CaptureBackend,
    options: &CaptureOptions,
) -> Result<(Frame, OutputInfo), CaptureError> {
    sleep_for_delay(options.delay);
    let output = backend.focused_output()?;
    let frame = backend.capture_output(&output.name, options.cursor)?;
    Ok((frame, output))
}

/// Crops a **desktop-logical** rectangle out of a frame already captured
/// from `output` — the second half of the region flow, run after the overlay
/// drops.
///
/// The conversion is [`logical_to_pixel_rect`]'s, unchanged: the interactive
/// overlay and an explicit `--geometry` land in exactly the same place, with
/// exactly the same rounding, because they go through exactly the same
/// function.
pub fn crop_frozen_frame(
    frame: &Frame,
    output: &OutputInfo,
    region: LogicalRect,
) -> Result<Frame, CaptureError> {
    let rect = logical_to_pixel_rect(region, output).ok_or(CaptureError::EmptyRegion)?;
    frame.crop(rect).ok_or(CaptureError::EmptyRegion)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Geometry, ShotArgs};
    use crate::config::CaptureConfig;

    /// `physical` is given explicitly (not derived) for the same reason
    /// [`OutputInfo::physical_width`]'s doc comment gives: on a
    /// fractional-scale output the two genuinely differ.
    fn output(
        name: &str,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        scale: f64,
        physical: (u32, u32),
    ) -> OutputInfo {
        OutputInfo {
            name: name.to_string(),
            logical: LogicalRect {
                x,
                y,
                width,
                height,
            },
            scale,
            physical_width: physical.0,
            physical_height: physical.1,
        }
    }

    /// A `width x height` frame whose every pixel encodes its own
    /// coordinates: R = x, G = y. Lets a crop assertion check *which*
    /// pixels came out, not merely how many.
    fn coordinate_frame(width: u32, height: u32, scale: f64) -> Frame {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[x as u8, y as u8, 0, 0xff]);
            }
        }
        match Frame::new(width, height, scale, pixels) {
            Some(frame) => frame,
            None => unreachable!("the test builder always produces width*height*4 bytes"),
        }
    }

    fn pixel_at(frame: &Frame, x: u32, y: u32) -> [u8; 4] {
        let start = (y as usize * frame.stride()) + x as usize * 4;
        match frame.pixels().get(start..start + 4) {
            Some([r, g, b, a]) => [*r, *g, *b, *a],
            _ => [0, 0, 0, 0],
        }
    }

    // -- Frame invariants ------------------------------------------------

    #[test]
    fn frame_rejects_a_buffer_of_the_wrong_length() {
        assert!(Frame::new(2, 2, 1.0, vec![0; 15]).is_none());
        assert!(Frame::new(2, 2, 1.0, vec![0; 17]).is_none());
        assert!(Frame::new(2, 2, 1.0, vec![0; 16]).is_some());
    }

    #[test]
    fn frame_rejects_zero_dimensions() {
        assert!(Frame::new(0, 4, 1.0, Vec::new()).is_none());
        assert!(Frame::new(4, 0, 1.0, Vec::new()).is_none());
    }

    #[test]
    fn frame_clamps_a_nonsense_scale_instead_of_losing_the_capture() {
        let frame = coordinate_frame(2, 2, f64::NAN);
        assert_eq!(frame.scale(), 1.0);
        let frame = coordinate_frame(2, 2, -3.0);
        assert_eq!(frame.scale(), 1.0);
    }

    // -- crop -------------------------------------------------------------

    #[test]
    fn crop_takes_exactly_the_requested_pixels() {
        let frame = coordinate_frame(8, 6, 1.5);
        let cropped = frame
            .crop(PixelRect {
                x: 2,
                y: 1,
                width: 3,
                height: 4,
            })
            .expect("the rect is fully inside the frame");

        assert_eq!(cropped.width(), 3);
        assert_eq!(cropped.height(), 4);
        assert_eq!(cropped.pixels().len(), 3 * 4 * 4);
        assert_eq!(cropped.scale(), 1.5, "a crop does not change the scale");
        // Top-left of the crop is source (2, 1); bottom-right is (4, 4).
        assert_eq!(pixel_at(&cropped, 0, 0), [2, 1, 0, 0xff]);
        assert_eq!(pixel_at(&cropped, 2, 3), [4, 4, 0, 0xff]);
    }

    #[test]
    fn crop_of_the_whole_frame_is_the_identity() {
        let frame = coordinate_frame(5, 3, 1.0);
        let cropped = frame.crop(frame.bounds()).expect("identity crop");
        assert_eq!(cropped, frame);
    }

    #[test]
    fn crop_clamps_a_rect_that_hangs_off_the_edge() {
        let frame = coordinate_frame(8, 6, 1.0);
        let cropped = frame
            .crop(PixelRect {
                x: 6,
                y: 5,
                width: 100,
                height: 100,
            })
            .expect("the rect overlaps the bottom-right corner");
        assert_eq!((cropped.width(), cropped.height()), (2, 1));
        assert_eq!(pixel_at(&cropped, 0, 0), [6, 5, 0, 0xff]);
    }

    #[test]
    fn crop_of_odd_sizes_is_exact() {
        // Odd width *and* an odd crop width: the case where a stride
        // assumption of "even bytes" would silently shear the image.
        let frame = coordinate_frame(7, 5, 1.0);
        let cropped = frame
            .crop(PixelRect {
                x: 1,
                y: 1,
                width: 5,
                height: 3,
            })
            .expect("inside the frame");
        assert_eq!((cropped.width(), cropped.height()), (5, 3));
        assert_eq!(pixel_at(&cropped, 4, 2), [5, 3, 0, 0xff]);
    }

    #[test]
    fn crop_entirely_outside_the_frame_is_none() {
        let frame = coordinate_frame(4, 4, 1.0);
        assert!(frame
            .crop(PixelRect {
                x: 10,
                y: 0,
                width: 4,
                height: 4
            })
            .is_none());
        assert!(frame
            .crop(PixelRect {
                x: 0,
                y: 0,
                width: 0,
                height: 4
            })
            .is_none());
    }

    #[test]
    fn clamp_rect_does_not_overflow_on_a_huge_rect() {
        let clamped = clamp_rect(
            PixelRect {
                x: u32::MAX - 1,
                y: 0,
                width: u32::MAX,
                height: u32::MAX,
            },
            PixelRect {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
        );
        assert_eq!(clamped, None, "the rect starts past the right edge");
    }

    // -- logical -> physical ----------------------------------------------

    #[test]
    fn logical_to_pixel_rect_matches_the_compositors_own_rounding() {
        // CAPTURE-RESEARCH §1.4, verified byte-exact against niri:
        // logical 100,100 400x300 on a scale-1.5 output is physical
        // 150,150 600x450.
        let out = output("eDP-1", 0, 0, 1706, 1066, 1.5, (2560, 1600));
        let rect = logical_to_pixel_rect(
            LogicalRect {
                x: 100,
                y: 100,
                width: 400,
                height: 300,
            },
            &out,
        )
        .expect("inside the output");
        assert_eq!(
            rect,
            PixelRect {
                x: 150,
                y: 150,
                width: 600,
                height: 450
            }
        );
    }

    #[test]
    fn logical_to_pixel_rect_rebases_onto_the_outputs_own_origin() {
        // A second monitor to the right: a region at logical x=1800 is 94
        // logical px into *that* output, not 1800.
        let out = output("DP-2", 1706, 0, 1920, 1080, 1.0, (1920, 1080));
        let rect = logical_to_pixel_rect(
            LogicalRect {
                x: 1806,
                y: 10,
                width: 100,
                height: 50,
            },
            &out,
        )
        .expect("inside the output");
        assert_eq!(
            rect,
            PixelRect {
                x: 100,
                y: 10,
                width: 100,
                height: 50
            }
        );
    }

    #[test]
    fn logical_to_pixel_rect_rounds_each_edge_independently() {
        // Two adjacent 1-logical-px columns at scale 1.5 must tile exactly:
        // edges at 0, 1.5 -> 2, 3.0 -> 3. Widths 2 and 1, no overlap, no
        // gap. `round(width * scale)` would have made both 2 (an overlap).
        let out = output("eDP-1", 0, 0, 100, 100, 1.5, (150, 150));
        let first = logical_to_pixel_rect(
            LogicalRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            &out,
        )
        .expect("inside");
        let second = logical_to_pixel_rect(
            LogicalRect {
                x: 1,
                y: 0,
                width: 1,
                height: 1,
            },
            &out,
        )
        .expect("inside");
        assert_eq!((first.x, first.width), (0, 2));
        assert_eq!((second.x, second.width), (2, 1));
    }

    #[test]
    fn logical_to_pixel_rect_clamps_a_region_hanging_off_the_left_edge() {
        let out = output("eDP-1", 0, 0, 100, 100, 2.0, (200, 200));
        let rect = logical_to_pixel_rect(
            LogicalRect {
                x: -20,
                y: -20,
                width: 40,
                height: 40,
            },
            &out,
        )
        .expect("half of it is on screen");
        assert_eq!(
            rect,
            PixelRect {
                x: 0,
                y: 0,
                width: 40,
                height: 40
            },
            "the on-screen half survives, in physical pixels"
        );
    }

    #[test]
    fn logical_to_pixel_rect_is_none_for_a_region_entirely_off_the_output() {
        let out = output("eDP-1", 0, 0, 100, 100, 1.0, (100, 100));
        assert!(logical_to_pixel_rect(
            LogicalRect {
                x: 500,
                y: 0,
                width: 10,
                height: 10
            },
            &out
        )
        .is_none());
    }

    // -- output_for_region ------------------------------------------------

    #[test]
    fn output_for_region_picks_the_output_with_the_most_overlap() {
        let outputs = vec![
            output("eDP-1", 0, 0, 1000, 1000, 1.0, (1000, 1000)),
            output("DP-2", 1000, 0, 1000, 1000, 1.0, (1000, 1000)),
        ];
        // 30 px on eDP-1, 70 px on DP-2.
        let region = LogicalRect {
            x: 970,
            y: 0,
            width: 100,
            height: 10,
        };
        let picked = output_for_region(&outputs, region).expect("overlaps both");
        assert_eq!(picked.name, "DP-2");
    }

    #[test]
    fn output_for_region_is_none_when_nothing_overlaps() {
        let outputs = vec![output("eDP-1", 0, 0, 100, 100, 1.0, (100, 100))];
        let region = LogicalRect {
            x: 500,
            y: 500,
            width: 10,
            height: 10,
        };
        assert!(output_for_region(&outputs, region).is_none());
    }

    // -- take_screenshot dispatch (against a fake backend) -----------------

    /// A [`CaptureBackend`] with no Wayland anywhere in it: it records what
    /// it was asked for and hands back a synthetic frame. This is the
    /// "compositors behind traits with fakes" half of CLAUDE.md's testing
    /// rule — the dispatch logic in [`take_screenshot`] is pure enough to
    /// test directly, and this is what makes that possible.
    struct FakeBackend {
        outputs: Vec<OutputInfo>,
        /// What [`CaptureBackend::focused_window`] answers — `None` by
        /// default (no window focused), settable per test via
        /// [`FakeBackend::with_focused_window`].
        focused_window: Option<WindowRef>,
        calls: std::cell::RefCell<Vec<String>>,
    }

    impl FakeBackend {
        fn single_output() -> Self {
            FakeBackend {
                outputs: vec![output("eDP-1", 0, 0, 1706, 1066, 1.5, (2560, 1600))],
                focused_window: None,
                calls: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn with_focused_window(mut self, window: WindowRef) -> Self {
            self.focused_window = Some(window);
            self
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl CaptureBackend for FakeBackend {
        fn outputs(&self) -> Result<Vec<OutputInfo>, CaptureError> {
            Ok(self.outputs.clone())
        }

        fn focused_output(&self) -> Result<OutputInfo, CaptureError> {
            self.outputs.first().cloned().ok_or(CaptureError::NoOutputs)
        }

        fn focused_window(&self) -> Result<Option<WindowRef>, CaptureError> {
            self.calls.borrow_mut().push("focused_window()".to_string());
            Ok(self.focused_window)
        }

        fn capture_output(&self, output: &str, cursor: bool) -> Result<Frame, CaptureError> {
            self.calls
                .borrow_mut()
                .push(format!("capture_output({output}, cursor={cursor})"));
            Ok(coordinate_frame(8, 8, 1.5))
        }

        fn capture_region(
            &self,
            output: &str,
            region: LogicalRect,
            cursor: bool,
        ) -> Result<Frame, CaptureError> {
            self.calls.borrow_mut().push(format!(
                "capture_region({output}, {}x{}+{}+{}, cursor={cursor})",
                region.width, region.height, region.x, region.y
            ));
            Ok(coordinate_frame(4, 4, 1.5))
        }

        fn capture_window(&self, window: WindowRef, _cursor: bool) -> Result<Frame, CaptureError> {
            self.calls
                .borrow_mut()
                .push(format!("capture_window({})", window.0));
            Ok(coordinate_frame(2, 2, 1.5))
        }
    }

    fn options(mutate: impl FnOnce(&mut ShotArgs)) -> CaptureOptions {
        let mut args = ShotArgs::default();
        mutate(&mut args);
        match CaptureOptions::resolve(&CaptureConfig::default(), &args) {
            Ok(options) => options,
            Err(err) => unreachable!("the test's own args always resolve: {err}"),
        }
    }

    #[test]
    fn fullscreen_captures_the_focused_output() {
        let backend = FakeBackend::single_output();
        let options = options(|a| a.fullscreen = true);
        let frame = take_screenshot(&backend, &options).expect("the fake never fails");
        assert_eq!((frame.width(), frame.height()), (8, 8));
        assert_eq!(
            backend.calls(),
            vec!["capture_output(eDP-1, cursor=true)".to_string()]
        );
    }

    #[test]
    fn region_with_geometry_captures_that_rectangle_on_the_right_output() {
        let backend = FakeBackend::single_output();
        let options = options(|a| {
            a.region = true;
            a.geometry = Some("400x300+100+100".to_string());
            a.no_cursor = true;
        });
        take_screenshot(&backend, &options).expect("the fake never fails");
        assert_eq!(
            backend.calls(),
            vec!["capture_region(eDP-1, 400x300+100+100, cursor=false)".to_string()]
        );
    }

    /// As of Stage 7 this path is `--no-daemon`-only: the daemon intercepts
    /// an interactive `--region` before [`take_screenshot`] and maps the
    /// overlay. The error must therefore name the *two real ways out*, not a
    /// future stage — there is no later stage that gives a surfaceless
    /// process an overlay.
    #[test]
    fn region_without_geometry_names_both_ways_out() {
        let backend = FakeBackend::single_output();
        let options = options(|a| a.region = true);
        let err = take_screenshot(&backend, &options).expect_err("no overlay without a daemon");
        assert!(
            matches!(err, CaptureError::Unsupported(note)
                if note.contains("--no-daemon") && note.contains("--geometry")),
            "got {err}"
        );
        assert!(
            backend.calls().is_empty(),
            "nothing should have been captured"
        );
    }

    // -- the region flow's two halves --------------------------------------

    #[test]
    fn freezing_captures_the_whole_focused_output_and_names_it() {
        let backend = FakeBackend::single_output();
        let options = options(|a| a.region = true);
        let (frame, output) =
            freeze_focused_output(&backend, &options).expect("the fake never fails");
        assert_eq!((frame.width(), frame.height()), (8, 8));
        assert_eq!(output.name, "eDP-1");
        assert_eq!(
            backend.calls(),
            vec!["capture_output(eDP-1, cursor=true)".to_string()],
            "the whole output, not a region"
        );
    }

    #[test]
    fn cropping_a_frozen_frame_matches_the_geometry_path_exactly() {
        // A 12x12 frame standing in for a scale-1.5 output whose logical
        // size is 8x8: a logical 2,2 4x4 selection is physical 3,3 6x6.
        let frame = coordinate_frame(12, 12, 1.5);
        let out = output("eDP-1", 0, 0, 8, 8, 1.5, (12, 12));
        let cropped = crop_frozen_frame(
            &frame,
            &out,
            LogicalRect {
                x: 2,
                y: 2,
                width: 4,
                height: 4,
            },
        )
        .expect("inside the output");
        assert_eq!((cropped.width(), cropped.height()), (6, 6));
        assert_eq!(pixel_at(&cropped, 0, 0), [3, 3, 0, 0xff]);
    }

    #[test]
    fn cropping_a_region_off_the_output_is_a_clean_error() {
        let frame = coordinate_frame(12, 12, 1.5);
        let out = output("eDP-1", 0, 0, 8, 8, 1.5, (12, 12));
        let err = crop_frozen_frame(
            &frame,
            &out,
            LogicalRect {
                x: 900,
                y: 900,
                width: 4,
                height: 4,
            },
        )
        .expect_err("off-screen");
        assert!(matches!(err, CaptureError::EmptyRegion), "got {err}");
    }

    // -- window capture (Stage 8) ------------------------------------------

    #[test]
    fn window_with_an_explicit_id_skips_the_focused_window_lookup() {
        let backend = FakeBackend::single_output().with_focused_window(WindowRef(1));
        let options = options(|a| {
            a.window = true;
            a.window_id = Some(42);
        });
        take_screenshot(&backend, &options).expect("the fake never fails");
        assert_eq!(
            backend.calls(),
            vec!["capture_window(42)".to_string()],
            "an explicit --window-id must never trigger a focused-window lookup"
        );
    }

    #[test]
    fn window_without_an_id_captures_the_focused_window() {
        let backend = FakeBackend::single_output().with_focused_window(WindowRef(7));
        let options = options(|a| a.window = true);
        take_screenshot(&backend, &options).expect("the fake never fails");
        assert_eq!(
            backend.calls(),
            vec![
                "focused_window()".to_string(),
                "capture_window(7)".to_string()
            ]
        );
    }

    #[test]
    fn window_without_an_id_or_a_focused_window_is_a_clean_error() {
        let backend = FakeBackend::single_output();
        let options = options(|a| a.window = true);
        let err = take_screenshot(&backend, &options).expect_err("nothing is focused");
        assert!(matches!(err, CaptureError::NoFocusedWindow), "got {err}");
        assert_eq!(
            backend.calls(),
            vec!["focused_window()".to_string()],
            "must not fall through to capturing anyway"
        );
    }

    #[test]
    fn a_region_off_every_output_is_a_clean_error() {
        let backend = FakeBackend::single_output();
        let options = options(|a| {
            a.region = true;
            a.geometry = Some("10x10+90000+90000".to_string());
        });
        let err = take_screenshot(&backend, &options).expect_err("off-screen");
        assert!(matches!(err, CaptureError::EmptyRegion), "got {err}");
    }

    /// Guards the one place `cli::Geometry` and [`LogicalRect`] have to
    /// agree: `take_screenshot` copies field for field, and a silent
    /// transposition (`x` into `y`) would be invisible in the fullscreen
    /// path and wrong in every region capture.
    #[test]
    fn geometry_maps_onto_logical_rect_field_for_field() {
        let geometry = Geometry {
            width: 4,
            height: 3,
            x: 2,
            y: 1,
        };
        let rect = LogicalRect {
            x: geometry.x,
            y: geometry.y,
            width: geometry.width,
            height: geometry.height,
        };
        assert_eq!(
            rect,
            LogicalRect {
                x: 2,
                y: 1,
                width: 4,
                height: 3
            }
        );
    }
}
