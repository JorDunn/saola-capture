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
//! # What Stage 5 implements and what it doesn't
//!
//! [`ScreencopyBackend`](screencopy::ScreencopyBackend) implements
//! [`CaptureBackend::outputs`], [`CaptureBackend::focused_output`],
//! [`CaptureBackend::capture_output`] and [`CaptureBackend::capture_region`]
//! for real. [`CaptureBackend::capture_window`] returns
//! [`CaptureError::Unsupported`] until Stage 8 wires it to niri-ipc's
//! `Action::ScreenshotWindow` (CAPTURE-RESEARCH D3 — *not* a geometry crop;
//! niri exposes no pixel position for tiled windows). Interactive region
//! selection (a `--region` with no `--geometry`) needs the Stage 7 overlay
//! and likewise reports a clean, actionable error today rather than guessing
//! a rectangle.

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
/// per-mechanism handle type is needed. Stage 8 fills the implementation in.
///
/// `#[allow(dead_code)]` because Stage 5 has no window *picker* to produce
/// an id — nothing constructs one yet. The type and the trait method exist
/// now so that Stage 8 adds a picker rather than also having to design this
/// boundary, and so the trait's shape matches Architecture's four-method
/// sketch from the start.
#[allow(dead_code)]
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
    /// Stage 8 (`niri-ipc` `Action::ScreenshotWindow`, CAPTURE-RESEARCH D3).
    /// `#[allow(dead_code)]` for the same reason [`WindowRef`] carries it:
    /// implemented by every backend, called by nobody until a window picker
    /// exists.
    #[allow(dead_code)]
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
    // `--delay`/`delay` is a plain sleep for now. Stage 8 ("window capture
    // + delayed capture") replaces this with a real countdown surface; the
    // knob is honoured here rather than ignored because a silently-ignored
    // `--delay 5` is precisely the "looks like success, isn't" failure
    // CLAUDE.md's no-panic rule is aimed at. In the daemon this runs inside
    // a `spawn_blocking` task (see `dbus.rs`), so it never stalls the event
    // loop.
    if options.delay > 0 {
        std::thread::sleep(std::time::Duration::from_secs(u64::from(options.delay)));
    }

    match options.kind {
        ShotKind::Fullscreen => {
            let output = backend.focused_output()?;
            backend.capture_output(&output.name, options.cursor)
        }
        ShotKind::Region => {
            let Some(geometry) = options.geometry else {
                return Err(CaptureError::Unsupported(
                    "--region without --geometry needs the interactive selection overlay, \
                     which lands in Stage 7 — pass --geometry WxH+X+Y for now",
                ));
            };
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
        ShotKind::Window => Err(CaptureError::Unsupported(
            "--window capture lands in Stage 8 (niri's own ScreenshotWindow action)",
        )),
    }
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
        calls: std::cell::RefCell<Vec<String>>,
    }

    impl FakeBackend {
        fn single_output() -> Self {
            FakeBackend {
                outputs: vec![output("eDP-1", 0, 0, 1706, 1066, 1.5, (2560, 1600))],
                calls: std::cell::RefCell::new(Vec::new()),
            }
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

    #[test]
    fn region_without_geometry_reports_the_stage_that_lands_it() {
        let backend = FakeBackend::single_output();
        let options = options(|a| a.region = true);
        let err = take_screenshot(&backend, &options).expect_err("no overlay yet");
        assert!(
            matches!(err, CaptureError::Unsupported(note) if note.contains("Stage 7")),
            "got {err}"
        );
        assert!(
            backend.calls().is_empty(),
            "nothing should have been captured"
        );
    }

    #[test]
    fn window_capture_reports_the_stage_that_lands_it() {
        let backend = FakeBackend::single_output();
        let options = options(|a| a.window = true);
        let err = take_screenshot(&backend, &options).expect_err("Stage 8");
        assert!(
            matches!(err, CaptureError::Unsupported(note) if note.contains("Stage 8")),
            "got {err}"
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
