//! Screenshots via `zwlr_screencopy_v1` — the real [`CaptureBackend`] for
//! stills (PLAN.md Stage 5, task 2; CAPTURE-RESEARCH decision **D1**).
//!
//! # The handshake, in the order it actually happens
//!
//! This is the whole protocol dance, written out because it is the part a
//! reader new to Wayland has no way to guess (and because getting the order
//! wrong produces an `InvalidBuffer` protocol error that kills the whole
//! connection, not a friendly reply):
//!
//! 1. Bind three globals from the registry: `wl_shm` (shared-memory buffer
//!    factory), `wl_output` (one per monitor), and
//!    `zwlr_screencopy_manager_v1`.
//! 2. Ask the manager for a frame:
//!    `capture_output(overlay_cursor, output)`. **Nothing is captured yet.**
//! 3. The compositor answers with what it is *willing* to write into: one
//!    `buffer(format, width, height, stride)` event per supported shm
//!    format, one `linux_dmabuf(fourcc, w, h)` per dmabuf format, then
//!    `buffer_done` (protocol version ≥ 3 only).
//! 4. We allocate a `wl_shm` buffer matching one of those offers and call
//!    `copy(buffer)`.
//! 5. The compositor blits into it and sends `flags` (possibly `y_invert`)
//!    then `ready`. Or `failed`, and there is nothing to read.
//!
//! CAPTURE-RESEARCH §1.1 pinned down what niri actually offers here, by
//! reading its source *and* running the handshake live: **exactly one** shm
//! format (`Xrgb8888`, stride `width * 4`) and one dmabuf format
//! (`XR24`). It is hardcoded, not negotiated. This code still walks the
//! offer list properly and still reads `stride` from the event rather than
//! computing `width * 4` — D1 says so in as many words ("`stride` is the
//! authority, not `width * 4` (it happens to be equal on this output; do not
//! assume it)") — because the cost of doing it right is a few lines and the
//! cost of doing it wrong is a sheared image on the first machine whose
//! compositor pads its rows.
//!
//! # The three things that are easy to get wrong
//!
//! - **Byte order.** `wl_shm`'s `Xrgb8888` is a *32-bit little-endian word*
//!   `0xXXRRGGBB`, which in memory is the bytes **B, G, R, X** — the
//!   opposite of what the name suggests. [`to_rgba`] swizzles; §1.4 verified
//!   the resulting RGB values against `grim`'s own PNG output pixel for
//!   pixel.
//! - **`y_invert`.** The protocol lets a compositor say "row 0 is the
//!   bottom row". niri never does — §1.2 checked both call sites in its
//!   source and confirmed live twice, including on an output whose transform
//!   is *flipped vertically*. [`to_rgba`] handles the flag anyway (it is
//!   four lines), but the inverted branch is exercised only by this module's
//!   unit tests, never on this compositor.
//! - **Alpha.** There is none. niri offers no alpha format at all, and a
//!   screenshot of a composited desktop is opaque by definition, so
//!   [`to_rgba`] writes `0xff` unconditionally rather than propagating
//!   whatever happened to be in the X byte.
//!
//! # Where the fractional scale comes from (and why not `wl_output`)
//!
//! [`OutputInfo::scale`] must be the *fractional* scale (1.5 on Jordan's
//! laptop) for [`logical_to_pixel_rect`] to convert a `--geometry` correctly.
//! `wl_output`'s own `scale` event **cannot** provide it: it is an integer,
//! and a fractional-scale compositor reports the ceiling — niri sends `2` for
//! a 1.5-scale output (verified in this repo's own Stage 2 probe transcript,
//! `docs/research/2026-08-08-stage2/screencopy-01-output-nocursor.txt`:
//! `eDP-1: mode 2560x1600 scale 2`). So the fractional scale and the logical
//! position/size come from **niri's IPC** ([`niri_ipc`]), which reports them
//! exactly (`Logical position: 0, 0 / Logical size: 1706x1066 / Scale: 1.5`).
//!
//! That is a compositor-specific dependency, which is precisely why it lives
//! *here*, below the [`CaptureBackend`] boundary, and not in a caller. And
//! it degrades: if the niri socket is unreachable ([`niri_logical_outputs`]
//! returns `None`), this module warns once and falls back to `wl_output`'s
//! integer scale, which is exactly right on an integer-scale output and
//! merely approximate on a fractional one. A fullscreen capture — the only
//! path Stage 5 wires end to end — does not use the scale for anything at
//! all, so that degradation never costs a screenshot.
//!
//! # Threading and lifetime
//!
//! Every public method opens a **fresh Wayland connection**, does its work,
//! and drops it. That sounds wasteful and isn't: §1.5 measured a full
//! 2560×1600 capture including process start, connection, two roundtrips and
//! a 16 MB poison-fill at 0.278–0.281 s, of which the connection is
//! microseconds. In exchange, the daemon (which already holds iced's own
//! Wayland connection) never has to serialize captures behind a lock, and a
//! connection that breaks mid-capture cannot poison the next one.
//!
//! Everything in this module **blocks**. The daemon calls it from a
//! `tokio::task::spawn_blocking` (see `dbus.rs`); the CLI calls it straight
//! from `main`.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd};
use std::os::unix::fs::FileExt;
use std::time::{Duration, Instant};

use wayland_client::backend::WaylandError;
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

use super::{
    logical_to_pixel_rect, CaptureBackend, CaptureError, Frame, LogicalRect, OutputInfo, WindowRef,
};

/// How long to wait for the compositor to finish one handshake step.
///
/// §1.5 measured the whole capture at ~0.3 s, so ten seconds is not a
/// performance budget — it is a **liveness** budget: the alternative to
/// having one is `blocking_dispatch` parking forever on a wedged compositor,
/// which for a `Print` keybind means a screenshot that never happens and
/// never says why. CLAUDE.md: "silent absence is the worst failure mode."
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Refuse to allocate a shm buffer larger than this. An 8K output is
/// ~132 MB; a compositor asking for more than 1 GiB is either broken or
/// hostile, and either way "clean error" beats "OOM-killed daemon".
const MAX_BUFFER_BYTES: usize = 1 << 30;

/// The byte order of a `wl_shm` buffer, reduced to the only thing this
/// module cares about: where R, G and B sit inside each 4-byte pixel.
///
/// The alpha/`X` byte is deliberately absent from this enum — see the module
/// doc comment: screenshots are opaque and [`to_rgba`] always writes `0xff`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShmLayout {
    /// Bytes in memory are B, G, R, X. This is `Xrgb8888`/`Argb8888` on a
    /// little-endian machine — the only thing niri ever offers (D1).
    Bgrx,
    /// Bytes in memory are R, G, B, X. `Xbgr8888`/`Abgr8888`. Handled for
    /// portability to other wlroots compositors; never seen on niri.
    Rgbx,
}

/// Maps a `wl_shm::Format` to the byte order it implies, or `None` for a
/// format this module cannot read (every non-32-bit-RGB format: YUV planes,
/// 10-bit, packed 16-bit, …).
fn layout_for_format(format: wl_shm::Format) -> Option<ShmLayout> {
    match format {
        wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => Some(ShmLayout::Bgrx),
        wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => Some(ShmLayout::Rgbx),
        _ => None,
    }
}

/// Turns a compositor shm buffer into the tightly-packed, top-row-first,
/// RGBA8 pixel data a [`Frame`] is made of.
///
/// This is the one genuinely fiddly function in the module, so it is written
/// to be *obviously* correct rather than clever, and it is the one covered
/// by the most unit tests (padded strides, odd sizes, both byte orders, both
/// y-invert states, and every truncated-input case):
///
/// - `stride` is the source's row pitch **in bytes** and may exceed
///   `width * 4` (padding at the end of each row). The destination never has
///   padding.
/// - `y_invert` mirrors rows: source row `y` becomes destination row
///   `height - 1 - y`.
/// - Anything inconsistent (`stride` too small, `src` too short, a zero
///   dimension, an arithmetic overflow) yields `None`. There is no partial
///   success and no panic — a screenshot that would have been half garbage
///   must fail loudly instead.
fn to_rgba(
    src: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    layout: ShmLayout,
    y_invert: bool,
) -> Option<Vec<u8>> {
    if width == 0 || height == 0 {
        return None;
    }

    let width = width as usize;
    let height = height as usize;
    let stride = stride as usize;

    let row_bytes = width.checked_mul(4)?;
    if stride < row_bytes {
        // A stride narrower than the pixels it is supposed to hold means
        // the compositor and this code disagree about the buffer's shape.
        return None;
    }
    // The last row only needs `row_bytes`, not a full `stride`, but every
    // compositor allocates `stride * height` and so does this code — the
    // stricter check is the safer one to make.
    let needed = stride.checked_mul(height)?;
    if src.len() < needed {
        return None;
    }

    let mut out = vec![0u8; row_bytes.checked_mul(height)?];

    for y in 0..height {
        let src_start = y.checked_mul(stride)?;
        let src_row = src.get(src_start..src_start.checked_add(row_bytes)?)?;

        let dst_y = if y_invert { height - 1 - y } else { y };
        let dst_start = dst_y.checked_mul(row_bytes)?;
        let dst_row = out.get_mut(dst_start..dst_start.checked_add(row_bytes)?)?;

        for (s, d) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(4)) {
            // Slice patterns rather than `s[0]`/`d[3]`: `chunks_exact(4)`
            // guarantees the length, but CLAUDE.md's no-panic rule is about
            // not *writing* indexing on runtime paths at all, so the
            // guarantee is expressed as a pattern the compiler checks.
            if let ([s0, s1, s2, _], [d0, d1, d2, d3]) = (s, d) {
                let (r, g, b) = match layout {
                    ShmLayout::Bgrx => (*s2, *s1, *s0),
                    ShmLayout::Rgbx => (*s0, *s1, *s2),
                };
                *d0 = r;
                *d1 = g;
                *d2 = b;
                *d3 = 0xff;
            }
        }
    }

    Some(out)
}

/// Re-orients a captured buffer from the output's **framebuffer** space into
/// the **displayed** space, undoing `wl_output`'s transform.
///
/// # Why this is needed at all (found live, Stage 5 — extends CAPTURE-RESEARCH §1.2)
///
/// A screencopy buffer is the output's framebuffer, and on an output with a
/// non-`Normal` `wl_output.geometry.transform` the framebuffer is **not**
/// what the user sees. The `y_invert` flag does not cover this: §1.2
/// established that niri always sends `flags = 0`, and that is still true —
/// the two are unrelated mechanisms.
///
/// Verified live in nested niri (Stage 5): the winit output reports
/// `Transform: flipped vertically` (`Flipped180`), and a raw capture of it
/// came out **vertically mirrored** relative to `grim` — the text in a
/// terminal read upside-down-mirrored, 51081 pixels different from grim's
/// PNG; after this correction the same comparison is byte-identical apart
/// from the cursor. `grim` does the same correction in its own renderer,
/// which is why it was right and the first draft of this module was wrong.
/// Jordan's real eDP-1 is `Normal`, so nothing on this machine's own session
/// exercises it — which is exactly why it had to be caught in the nested
/// compositor.
///
/// # The pixel mapping
///
/// Wayland's transforms rotate **counter-clockwise**, and the `Flipped`
/// family means "mirror horizontally, *then* rotate". Displayed content is
/// recovered by applying the transform's inverse, which is the identity for
/// every reflection (all four `Flipped*`) and for `Normal`/`_180`, and swaps
/// `_90` with `_270`.
///
/// **Verification status, honestly**: only `Normal` (the real session, and
/// §1.4's byte-exact grim cross-check) and `Flipped180` (nested niri, above)
/// are live-verified. The `_90`/`_270` pair is the one place the inverse
/// matters, and it was checked by rotating the nested output — see the Stage
/// 5 handoff. The four `Flipped*` cases are self-inverse, so no choice
/// exists there to get wrong.
fn undo_output_transform(
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    transform: wl_output::Transform,
) -> Option<(Vec<u8>, u32, u32)> {
    let correction = invert_transform(transform);
    if correction == wl_output::Transform::Normal {
        // The overwhelmingly common case (every non-rotated monitor):
        // hand the buffer straight back rather than copying 16 MB to do
        // nothing to it.
        return Some((pixels, width, height));
    }

    let w = width as usize;
    let h = height as usize;
    let (dest_width, dest_height) = match correction {
        wl_output::Transform::_90
        | wl_output::Transform::_270
        | wl_output::Transform::Flipped90
        | wl_output::Transform::Flipped270 => (height, width),
        _ => (width, height),
    };

    let dw = dest_width as usize;
    let dh = dest_height as usize;
    let mut out = vec![0u8; dw.checked_mul(dh)?.checked_mul(4)?];

    for dy in 0..dh {
        for dx in 0..dw {
            // Source coordinates for this destination pixel. Derived once,
            // written out per case rather than as a matrix, so each line can
            // be checked against a corner by hand (and by the unit tests
            // below, which do exactly that).
            let (sx, sy) = match correction {
                wl_output::Transform::Normal => (dx, dy),
                wl_output::Transform::_90 => (w.checked_sub(dy + 1)?, dx),
                wl_output::Transform::_180 => (w.checked_sub(dx + 1)?, h.checked_sub(dy + 1)?),
                wl_output::Transform::_270 => (dy, h.checked_sub(dx + 1)?),
                wl_output::Transform::Flipped => (w.checked_sub(dx + 1)?, dy),
                wl_output::Transform::Flipped90 => (dy, dx),
                wl_output::Transform::Flipped180 => (dx, h.checked_sub(dy + 1)?),
                wl_output::Transform::Flipped270 => {
                    (w.checked_sub(dy + 1)?, h.checked_sub(dx + 1)?)
                }
                // `wl_output::Transform` is `#[non_exhaustive]`; an unknown
                // transform is better left uncorrected than guessed at.
                _ => (dx, dy),
            };

            let src_start = sy.checked_mul(w)?.checked_add(sx)?.checked_mul(4)?;
            let dst_start = dy.checked_mul(dw)?.checked_add(dx)?.checked_mul(4)?;
            let src = pixels.get(src_start..src_start.checked_add(4)?)?;
            let dst = out.get_mut(dst_start..dst_start.checked_add(4)?)?;
            dst.copy_from_slice(src);
        }
    }

    Some((out, dest_width, dest_height))
}

/// The inverse of a `wl_output` transform: rotations swap 90 with 270; every
/// reflection (and `Normal`/`_180`) is its own inverse. Same rule as
/// wlroots' `wlr_output_transform_invert`.
fn invert_transform(transform: wl_output::Transform) -> wl_output::Transform {
    match transform {
        wl_output::Transform::_90 => wl_output::Transform::_270,
        wl_output::Transform::_270 => wl_output::Transform::_90,
        other => other,
    }
}

/// The `zwlr_screencopy_v1` implementation of [`CaptureBackend`].
///
/// Stateless: see the module doc comment on why each call opens its own
/// Wayland connection. Construct one with [`ScreencopyBackend::new`] and
/// hold it for as long as convenient (the daemon holds one forever; a CLI
/// verb builds one, uses it once and exits).
#[derive(Debug, Default, Clone, Copy)]
pub struct ScreencopyBackend;

impl ScreencopyBackend {
    pub fn new() -> Self {
        ScreencopyBackend
    }
}

impl CaptureBackend for ScreencopyBackend {
    fn outputs(&self) -> Result<Vec<OutputInfo>, CaptureError> {
        Session::open()?.output_infos()
    }

    fn focused_output(&self) -> Result<OutputInfo, CaptureError> {
        let session = Session::open()?;
        let outputs = session.output_infos()?;

        // niri knows which output has focus; ask it. If it can't be reached
        // (or reports nothing focused), fall back to the first output by
        // name — arbitrary, but *deterministic*, which is what matters for a
        // fallback: the same machine picks the same monitor every time
        // rather than whichever one the registry happened to advertise
        // first this run.
        if let Some(name) = niri_focused_output_name() {
            if let Some(found) = outputs.iter().find(|output| output.name == name) {
                return Ok(found.clone());
            }
        }

        let mut sorted: Vec<&OutputInfo> = outputs.iter().collect();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        sorted
            .first()
            .map(|output| (*output).clone())
            .ok_or(CaptureError::NoOutputs)
    }

    fn capture_output(&self, output: &str, cursor: bool) -> Result<Frame, CaptureError> {
        let mut session = Session::open()?;
        let info = session.find_output(output)?;
        session.capture(&info, cursor)
    }

    fn capture_region(
        &self,
        output: &str,
        region: LogicalRect,
        cursor: bool,
    ) -> Result<Frame, CaptureError> {
        let mut session = Session::open()?;
        let info = session.find_output(output)?;

        // Resolve the crop rectangle *before* capturing, so a nonsense
        // `--geometry` costs nothing and reports immediately rather than
        // after a 0.3 s capture.
        let rect = logical_to_pixel_rect(region, &info).ok_or(CaptureError::EmptyRegion)?;

        // Capture the whole output and crop in memory, per D2 — not
        // `capture_output_region`. The overlay (Stage 7) needs the full
        // frozen frame as its background anyway, so one capture will serve
        // both, and screencopy composites layer surfaces (§1.5), which is
        // why the capture has to precede any overlay rather than be
        // re-issued per drag.
        let frame = session.capture(&info, cursor)?;
        frame.crop(rect).ok_or(CaptureError::EmptyRegion)
    }

    fn capture_window(&self, _window: WindowRef, _cursor: bool) -> Result<Frame, CaptureError> {
        // CAPTURE-RESEARCH D3: this is *not* a screencopy path at all. niri
        // exposes no pixel position for tiled windows, so a geometry crop is
        // impossible; the mechanism is niri-ipc's own `Action::ScreenshotWindow`,
        // which renders the window's elements offscreen. Stage 8's job.
        Err(CaptureError::Unsupported(
            "--window capture lands in Stage 8 (niri's own ScreenshotWindow action)",
        ))
    }
}

// ---------------------------------------------------------------------
// Wayland plumbing
// ---------------------------------------------------------------------

/// One shm buffer shape the compositor said it is willing to write into.
#[derive(Debug, Clone, Copy)]
struct ShmOffer {
    format: WEnum<wl_shm::Format>,
    width: u32,
    height: u32,
    stride: u32,
}

/// A `wl_output` we've bound, plus whatever its own events told us.
struct BoundOutput {
    proxy: wl_output::WlOutput,
    /// From the `name` event (`wl_output` version ≥ 4). Empty if the
    /// compositor is older — see [`State::output_name`].
    name: String,
    /// The current mode's size, in **physical** pixels.
    mode: Option<(u32, u32)>,
    /// `wl_output`'s integer scale — the ceiling of the real one on a
    /// fractional-scale compositor, hence only ever a fallback.
    integer_scale: i32,
    /// The output's `geometry.transform`. `Normal` on every non-rotated
    /// monitor; see `undo_output_transform` for what a non-`Normal` value
    /// costs if it is ignored (it was, in this stage's first draft, and the
    /// nested-niri live check caught it).
    transform: wl_output::Transform,
}

/// Everything one `capture` handshake accumulates.
#[derive(Debug, Default)]
struct FrameHandshake {
    offers: Vec<ShmOffer>,
    buffer_done: bool,
    /// The `flags` event's raw bits. Bit 0 is `y_invert`.
    flags: u32,
    ready: bool,
    failed: bool,
}

/// The `&mut D` every `Dispatch` impl in this module writes into.
#[derive(Default)]
struct State {
    shm: Option<wl_shm::WlShm>,
    manager: Option<ZwlrScreencopyManagerV1>,
    manager_version: u32,
    outputs: Vec<BoundOutput>,
    frame: FrameHandshake,
}

impl State {
    /// A stable name for an output whose compositor never sent a `name`
    /// event (`wl_output` < 4). Not expected on niri; a fallback rather than
    /// an empty string so the output is still addressable.
    fn output_name(index: usize, bound: &BoundOutput) -> String {
        if bound.name.is_empty() {
            format!("wl_output-{index}")
        } else {
            bound.name.clone()
        }
    }
}

/// One connection's worth of Wayland state, opened per public method call.
struct Session {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
}

impl Session {
    /// Connects, binds the globals, and waits for the outputs to describe
    /// themselves.
    ///
    /// Two roundtrips, not one, and the reason is worth spelling out: the
    /// first delivers the registry's `global` events (which is when
    /// `wl_output`s get bound at all), and the *second* delivers the events
    /// those freshly-bound `wl_output`s then send about themselves (`name`,
    /// `mode`, `scale`). One roundtrip would leave every output nameless.
    fn open() -> Result<Self, CaptureError> {
        let conn =
            Connection::connect_to_env().map_err(|err| CaptureError::Connect(err.to_string()))?;
        let mut queue = conn.new_event_queue::<State>();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());

        let mut state = State::default();
        queue
            .roundtrip(&mut state)
            .map_err(|err| CaptureError::Protocol(err.to_string()))?;
        queue
            .roundtrip(&mut state)
            .map_err(|err| CaptureError::Protocol(err.to_string()))?;

        if state.shm.is_none() {
            return Err(CaptureError::MissingGlobal("wl_shm"));
        }
        if state.manager.is_none() {
            return Err(CaptureError::MissingGlobal("zwlr_screencopy_manager_v1"));
        }
        if state.outputs.is_empty() {
            return Err(CaptureError::NoOutputs);
        }

        Ok(Session { conn, queue, state })
    }

    /// Every output, with niri's fractional scale and logical geometry
    /// folded in where available.
    fn output_infos(&self) -> Result<Vec<OutputInfo>, CaptureError> {
        let niri = niri_logical_outputs();
        if niri.is_none() {
            eprintln!(
                "saola-capture: could not read output geometry from niri \
                 (is $NIRI_SOCKET set?) — falling back to wl_output's integer scale, \
                 which is approximate on a fractionally-scaled output"
            );
        }
        let niri = niri.unwrap_or_default();

        let mut infos = Vec::with_capacity(self.state.outputs.len());
        for (index, bound) in self.state.outputs.iter().enumerate() {
            let name = State::output_name(index, bound);
            // A mode event is guaranteed by the protocol for any enabled
            // output; an output that somehow never sent one is skipped
            // rather than reported at a made-up size.
            let Some((mode_width, mode_height)) = bound.mode else {
                continue;
            };

            let info = match niri.get(&name) {
                Some(logical) => OutputInfo {
                    name,
                    logical: logical.rect,
                    scale: logical.scale,
                    physical_width: mode_width,
                    physical_height: mode_height,
                },
                None => {
                    let scale = if bound.integer_scale > 0 {
                        f64::from(bound.integer_scale)
                    } else {
                        1.0
                    };
                    OutputInfo {
                        name,
                        // Without niri's answer there is no way to know
                        // where this output sits in a multi-output layout,
                        // so it is placed at the origin. Single-output
                        // machines (and every fullscreen capture) are
                        // unaffected; a multi-output `--geometry` would be
                        // wrong, which is what the warning above is for.
                        logical: LogicalRect {
                            x: 0,
                            y: 0,
                            width: (f64::from(mode_width) / scale).round() as u32,
                            height: (f64::from(mode_height) / scale).round() as u32,
                        },
                        scale,
                        physical_width: mode_width,
                        physical_height: mode_height,
                    }
                }
            };
            infos.push(info);
        }

        if infos.is_empty() {
            return Err(CaptureError::NoOutputs);
        }
        Ok(infos)
    }

    fn find_output(&self, name: &str) -> Result<OutputInfo, CaptureError> {
        self.output_infos()?
            .into_iter()
            .find(|output| output.name == name)
            .ok_or_else(|| CaptureError::UnknownOutput(name.to_string()))
    }

    /// The proxy for a named output (needed to issue the capture request)
    /// and its `wl_output` transform (needed to re-orient the result).
    fn output_proxy(
        &self,
        name: &str,
    ) -> Result<(wl_output::WlOutput, wl_output::Transform), CaptureError> {
        self.state
            .outputs
            .iter()
            .enumerate()
            .find(|(index, bound)| State::output_name(*index, bound) == name)
            .map(|(_, bound)| (bound.proxy.clone(), bound.transform))
            .ok_or_else(|| CaptureError::UnknownOutput(name.to_string()))
    }

    /// The whole of D1's flow: request, allocate, copy, swizzle.
    fn capture(&mut self, output: &OutputInfo, cursor: bool) -> Result<Frame, CaptureError> {
        let (proxy, transform) = self.output_proxy(&output.name)?;
        let manager = self
            .state
            .manager
            .clone()
            .ok_or(CaptureError::MissingGlobal("zwlr_screencopy_manager_v1"))?;
        let shm = self
            .state
            .shm
            .clone()
            .ok_or(CaptureError::MissingGlobal("wl_shm"))?;
        let manager_version = self.state.manager_version;

        self.state.frame = FrameHandshake::default();
        let qh = self.queue.handle();
        // `overlay_cursor` is a plain per-request boolean (§1.3, verified by
        // differencing three back-to-back captures): the cursor is
        // composited into the frame at physical resolution, and there is no
        // "cursor metadata" mode on this interface.
        let frame = manager.capture_output(i32::from(cursor), &proxy, &qh, ());

        // Step 3: wait for the format offers. `buffer_done` only exists from
        // version 3; on an older compositor the first `buffer` event is all
        // we are going to get a signal from.
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        if manager_version >= 3 {
            self.pump(deadline, |state| {
                state.frame.buffer_done || state.frame.failed
            })?;
        } else {
            self.pump(deadline, |state| {
                !state.frame.offers.is_empty() || state.frame.failed
            })?;
        }
        if self.state.frame.failed {
            frame.destroy();
            return Err(CaptureError::Refused);
        }

        let (offer, format, layout) = pick_offer(&self.state.frame.offers)?;

        // Step 4: allocate a matching shm buffer and hand it over.
        let len = (offer.stride as usize)
            .checked_mul(offer.height as usize)
            .filter(|len| *len > 0 && *len <= MAX_BUFFER_BYTES)
            .ok_or_else(|| {
                CaptureError::Io(io::Error::other(format!(
                    "refusing a {}x{} stride-{} screencopy buffer",
                    offer.width, offer.height, offer.stride
                )))
            })?;
        let file = shm_file(len).map_err(CaptureError::Io)?;
        let pool_size = i32::try_from(len).map_err(|_| {
            CaptureError::Io(io::Error::other(
                "screencopy buffer is larger than wl_shm can address",
            ))
        })?;
        let pool = shm.create_pool(file.as_fd(), pool_size, &qh, ());
        let buffer = pool.create_buffer(
            0,
            offer.width as i32,
            offer.height as i32,
            offer.stride as i32,
            // Already resolved by `pick_offer` — the `WEnum` was unwrapped
            // there, which is also where an unusable format is rejected.
            format,
            &qh,
            (),
        );

        frame.copy(&buffer);

        // Step 5: wait for the blit.
        let result = self.pump(deadline, |state| state.frame.ready || state.frame.failed);

        let outcome = result.and_then(|()| {
            if self.state.frame.failed {
                return Err(CaptureError::Refused);
            }
            if !self.state.frame.ready {
                return Err(CaptureError::Timeout);
            }

            let mut bytes = vec![0u8; len];
            file.read_exact_at(&mut bytes, 0)
                .map_err(CaptureError::Io)?;

            // Bit 0 of the `flags` event is `y_invert`. niri never sets it
            // (§1.2); the branch exists because the protocol allows it and a
            // future niri could change.
            let y_invert =
                self.state.frame.flags & u32::from(zwlr_screencopy_frame_v1::Flags::YInvert) != 0;

            let pixels = to_rgba(
                &bytes,
                offer.width,
                offer.height,
                offer.stride,
                layout,
                y_invert,
            )
            .ok_or_else(|| {
                CaptureError::Protocol(format!(
                    "the compositor's {}x{} stride-{} buffer is not self-consistent",
                    offer.width, offer.height, offer.stride
                ))
            })?;

            // The buffer is the output's *framebuffer*; on a rotated or
            // mirrored output that is not what the user sees. See
            // `undo_output_transform`.
            let (pixels, width, height) =
                undo_output_transform(pixels, offer.width, offer.height, transform).ok_or_else(
                    || {
                        CaptureError::Protocol(format!(
                            "could not apply the {transform:?} output transform to a {}x{} frame",
                            offer.width, offer.height
                        ))
                    },
                )?;

            Frame::new(width, height, output.scale, pixels)
                .ok_or_else(|| CaptureError::Protocol("empty screencopy frame".to_string()))
        });

        // Release the compositor-side objects either way. (Dropping the
        // whole connection right after would do it too, but being explicit
        // keeps the buffer's lifetime obviously *after* `ready`, which is
        // the part the protocol actually requires.)
        buffer.destroy();
        pool.destroy();
        frame.destroy();
        let _ = self.queue.flush();

        outcome
    }

    /// Drives the event loop until `done` says so, or the deadline passes.
    ///
    /// Teaching note (why not `EventQueue::blocking_dispatch`): that helper
    /// polls the socket with **no timeout**, so a compositor that stops
    /// answering mid-handshake parks this thread forever. This loop is the
    /// same shape with a deadline threaded through: flush our requests, ask
    /// the connection for a read guard, wait for the socket to become
    /// readable *or* the deadline to pass, read, dispatch. `prepare_read`
    /// returning `None` means events are already queued locally — the next
    /// `dispatch_pending` will consume them, so the loop simply continues
    /// (and re-checks the deadline, which is what keeps that case bounded
    /// too).
    fn pump<F>(&mut self, deadline: Instant, done: F) -> Result<(), CaptureError>
    where
        F: Fn(&State) -> bool,
    {
        loop {
            self.queue
                .dispatch_pending(&mut self.state)
                .map_err(|err| CaptureError::Protocol(err.to_string()))?;
            if done(&self.state) {
                return Ok(());
            }

            self.queue
                .flush()
                .map_err(|err| CaptureError::Protocol(err.to_string()))?;

            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(CaptureError::Timeout);
            };

            let Some(guard) = self.conn.prepare_read() else {
                continue;
            };
            if !wait_readable(guard.connection_fd(), remaining).map_err(CaptureError::Io)? {
                return Err(CaptureError::Timeout);
            }
            match guard.read() {
                Ok(_) => {}
                // "Nothing to read after all" — another thread got there
                // first, or a spurious wakeup. Loop and try again; the
                // deadline check keeps it bounded.
                Err(WaylandError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => {}
                Err(err) => return Err(CaptureError::Protocol(err.to_string())),
            }
        }
    }
}

/// Picks the first offer this module knows how to read, and the byte order
/// it implies. On niri there is exactly one offer and it is `Xrgb8888`
/// (§1.1).
fn pick_offer(offers: &[ShmOffer]) -> Result<(ShmOffer, wl_shm::Format, ShmLayout), CaptureError> {
    for offer in offers {
        // `WEnum::into_result` resolves the wire value to the generated
        // enum, or hands back the raw number for a format this build of
        // wayland-client doesn't know a name for.
        if let Ok(format) = offer.format.into_result() {
            if let Some(layout) = layout_for_format(format) {
                if offer.width > 0 && offer.height > 0 {
                    return Ok((*offer, format, layout));
                }
            }
        }
    }

    // Nothing usable: name the first format the compositor did offer, so
    // the error message says *what* it wanted rather than just "no".
    let offered = offers
        .first()
        .map(|offer| match offer.format {
            WEnum::Value(value) => value as u32,
            WEnum::Unknown(raw) => raw,
        })
        .unwrap_or_default();
    Err(CaptureError::UnsupportedFormat(offered))
}

/// An anonymous, in-memory file to back a `wl_shm` pool.
///
/// Teaching note (why `memfd_create` and not a temp file): `wl_shm` needs a
/// file descriptor both processes can `mmap`. A file in `/tmp` would work
/// but has to be created, unlinked and cleaned up, and leaves a window where
/// it is visible on disk. `memfd_create` hands back an fd to a nameless
/// chunk of memory that disappears when the last descriptor closes —
/// exactly the lifetime we want. `MFD_CLOEXEC` keeps it from leaking into
/// any process this one later spawns (the detached clipboard helper, say).
///
/// This is the module's only `unsafe`, and each call is a plain libc call
/// whose contract is checked immediately: a negative return means "consult
/// `errno`", which is what `io::Error::last_os_error` does.
fn shm_file(len: usize) -> Result<File, io::Error> {
    // SAFETY: `memfd_create` reads a NUL-terminated name (a `c"..."`
    // literal is exactly that, with a lifetime covering the call) and
    // returns a new fd or -1. No pointer we own is retained by the kernel.
    let raw = unsafe { libc::memfd_create(c"saola-capture".as_ptr(), libc::MFD_CLOEXEC) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a fresh, exclusively-owned fd returned by
    // `memfd_create` above; `File` takes ownership and closes it on drop.
    let file = unsafe { File::from_raw_fd(raw) };

    let length = libc::off_t::try_from(len)
        .map_err(|_| io::Error::other("shared-memory buffer is too large for off_t"))?;
    // SAFETY: `raw` is the fd `file` owns and is still open for the whole
    // call; `ftruncate` mutates only the file's length.
    if unsafe { libc::ftruncate(raw, length) } != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(file)
}

/// `poll(2)` on one fd with a timeout. `Ok(true)` means readable,
/// `Ok(false)` means the timeout expired first.
fn wait_readable(fd: BorrowedFd<'_>, timeout: Duration) -> Result<bool, io::Error> {
    // `poll`'s timeout is milliseconds in an `i32`; saturate rather than
    // wrap (a wrapped negative value means "block forever", which is the
    // exact failure this function exists to prevent).
    let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);

    loop {
        let mut pollfd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pollfd` is a live, correctly-initialized struct on this
        // stack frame and the count matches; `fd` is borrowed for the
        // duration of the call.
        let ready = unsafe { libc::poll(&mut pollfd, 1, millis) };
        if ready < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                // A signal arrived. Re-poll; the outer deadline check in
                // `Session::pump` is what stops this from looping forever.
                continue;
            }
            return Err(err);
        }
        return Ok(ready > 0);
    }
}

// ---------------------------------------------------------------------
// niri IPC: the fractional scale and the logical layout
// ---------------------------------------------------------------------

/// One output's logical placement, as niri reports it.
struct NiriLogical {
    rect: LogicalRect,
    scale: f64,
}

/// Asks niri for every output's logical geometry and fractional scale.
///
/// `None` on any failure at all — socket unset, socket refused, niri
/// answered an error, niri answered something else. There is nothing
/// actionable to distinguish between those from here, and the caller's
/// fallback is the same in every case.
fn niri_logical_outputs() -> Option<HashMap<String, NiriLogical>> {
    let mut socket = niri_ipc::socket::Socket::connect().ok()?;
    let response = socket.send(niri_ipc::Request::Outputs).ok()?.ok()?;
    let niri_ipc::Response::Outputs(outputs) = response else {
        return None;
    };

    let mut map = HashMap::new();
    for (name, output) in outputs {
        let Some(logical) = output.logical else {
            // A disabled output has no logical placement. Skipping it here
            // means it falls back to the wl_output path, which is right:
            // there is nothing to capture on it anyway.
            continue;
        };
        map.insert(
            name,
            NiriLogical {
                rect: LogicalRect {
                    x: logical.x,
                    y: logical.y,
                    width: logical.width,
                    height: logical.height,
                },
                scale: logical.scale,
            },
        );
    }
    Some(map)
}

/// The focused output's name, or `None` if niri can't be reached or nothing
/// is focused.
fn niri_focused_output_name() -> Option<String> {
    let mut socket = niri_ipc::socket::Socket::connect().ok()?;
    let response = socket.send(niri_ipc::Request::FocusedOutput).ok()?.ok()?;
    match response {
        niri_ipc::Response::FocusedOutput(Some(output)) => Some(output.name),
        _ => None,
    }
}

// ---------------------------------------------------------------------
// Dispatch impls — one per Wayland object this module touches.
// ---------------------------------------------------------------------

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            // `GlobalRemove` during the ~1 ms this connection is open would
            // mean a monitor was unplugged mid-handshake; the capture will
            // fail on its own with a protocol error, which is more
            // informative than anything this handler could do.
            return;
        };

        match interface.as_str() {
            "wl_shm" => {
                state.shm = Some(registry.bind(name, version.min(1), qh, ()));
            }
            "zwlr_screencopy_manager_v1" => {
                // Cap at 3: that is the version whose events this module
                // knows (`linux_dmabuf`/`buffer_done` arrived in 3), and
                // binding a higher version than you can handle is how you
                // get events you silently ignore.
                let bound = version.min(3);
                state.manager_version = bound;
                state.manager = Some(registry.bind(name, bound, qh, ()));
            }
            "wl_output" => {
                // Version 4 is where the `name` event lives — the whole
                // reason this module can address outputs by "eDP-1".
                let proxy: wl_output::WlOutput = registry.bind(name, version.min(4), qh, ());
                state.outputs.push(BoundOutput {
                    proxy,
                    name: String::new(),
                    mode: None,
                    integer_scale: 1,
                    transform: wl_output::Transform::Normal,
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_shm::WlShm, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_shm::WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // `wl_shm` only ever sends `format` events advertising the
        // compositor's global format list. Screencopy uses none of that
        // generality (§1.1: the frame's own `buffer` events are the
        // authority), so there is nothing to record.
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(bound) = state.outputs.iter_mut().find(|bound| &bound.proxy == proxy) else {
            return;
        };

        match event {
            wl_output::Event::Name { name } => bound.name = name,
            wl_output::Event::Scale { factor } => bound.integer_scale = factor,
            wl_output::Event::Geometry { transform, .. } => {
                if let Ok(transform) = transform.into_result() {
                    bound.transform = transform;
                }
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                // Only the *current* mode describes the buffer a capture
                // will produce; the others are just what the monitor could
                // do. `flags` is a bitfield, so an unknown value is not an
                // error, only uninteresting.
                let is_current = matches!(
                    flags.into_result(),
                    Ok(mode) if mode.contains(wl_output::Mode::Current)
                );
                if is_current {
                    bound.mode = Some((width.max(0) as u32, height.max(0) as u32));
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrScreencopyManagerV1,
        _: <ZwlrScreencopyManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The manager has no events.
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => state.frame.offers.push(ShmOffer {
                format,
                width,
                height,
                stride,
            }),
            zwlr_screencopy_frame_v1::Event::BufferDone => state.frame.buffer_done = true,
            zwlr_screencopy_frame_v1::Event::Flags { flags } => {
                state.frame.flags = match flags {
                    WEnum::Value(value) => value.bits(),
                    WEnum::Unknown(raw) => raw,
                };
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => state.frame.ready = true,
            zwlr_screencopy_frame_v1::Event::Failed => state.frame.failed = true,
            // `linux_dmabuf` (this code takes the shm path — D1) and
            // `damage` (§1.5: never arrives on a plain, non-`copy_with_damage`
            // capture) are both deliberately ignored.
            _ => {}
        }
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // No events.
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // `release` only matters to a client that reuses buffers across
        // frames; this one allocates per capture and destroys after `ready`.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a synthetic compositor buffer: `width x height` pixels at
    /// `stride` bytes per row, with the padding bytes poisoned to `0xAA` so
    /// a stride bug shows up as garbage in the output rather than as a
    /// coincidentally-correct zero.
    fn shm_buffer(width: u32, height: u32, stride: u32, layout: ShmLayout) -> Vec<u8> {
        let mut buffer = vec![0xAA_u8; (stride * height) as usize];
        for y in 0..height {
            for x in 0..width {
                let offset = (y * stride + x * 4) as usize;
                // Each pixel encodes its coordinates: R = x, G = y, B = 7.
                let (r, g, b) = (x as u8, y as u8, 7);
                let bytes = match layout {
                    ShmLayout::Bgrx => [b, g, r, 0x00],
                    ShmLayout::Rgbx => [r, g, b, 0x00],
                };
                if let Some(slot) = buffer.get_mut(offset..offset + 4) {
                    slot.copy_from_slice(&bytes);
                }
            }
        }
        buffer
    }

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * width + x) * 4) as usize;
        match rgba.get(offset..offset + 4) {
            Some([r, g, b, a]) => [*r, *g, *b, *a],
            _ => [0, 0, 0, 0],
        }
    }

    #[test]
    fn swizzles_bgrx_to_rgba_and_forces_opaque_alpha() {
        let src = shm_buffer(3, 2, 12, ShmLayout::Bgrx);
        let out = to_rgba(&src, 3, 2, 12, ShmLayout::Bgrx, false).expect("consistent buffer");

        assert_eq!(out.len(), 3 * 2 * 4);
        assert_eq!(pixel(&out, 3, 0, 0), [0, 0, 7, 0xff]);
        assert_eq!(pixel(&out, 3, 2, 1), [2, 1, 7, 0xff]);
        assert!(
            out.chunks_exact(4).all(|p| p.last() == Some(&0xff)),
            "alpha must be forced opaque — the X byte is not an alpha channel"
        );
    }

    #[test]
    fn swizzles_rgbx_without_swapping_channels() {
        let src = shm_buffer(2, 2, 8, ShmLayout::Rgbx);
        let out = to_rgba(&src, 2, 2, 8, ShmLayout::Rgbx, false).expect("consistent buffer");
        assert_eq!(pixel(&out, 2, 1, 1), [1, 1, 7, 0xff]);
    }

    /// The case D1 warns about: `stride != width * 4`. niri's own buffers
    /// happen to be unpadded, so this is the branch that would never be
    /// exercised in the real session and would break on the first machine
    /// that pads.
    #[test]
    fn honours_a_stride_wider_than_the_row() {
        let width = 3;
        let height = 4;
        let stride = 20; // 8 bytes of padding per row.
        let src = shm_buffer(width, height, stride, ShmLayout::Bgrx);
        let out = to_rgba(&src, width, height, stride, ShmLayout::Bgrx, false)
            .expect("consistent buffer");

        assert_eq!(out.len(), (width * height * 4) as usize);
        // If the padding leaked in, row 1 would start with 0xAA bytes.
        assert_eq!(pixel(&out, width, 0, 1), [0, 1, 7, 0xff]);
        assert_eq!(pixel(&out, width, 2, 3), [2, 3, 7, 0xff]);
        assert!(
            !out.contains(&0xAA),
            "no padding byte may survive into the frame"
        );
    }

    #[test]
    fn y_invert_mirrors_rows_and_nothing_else() {
        let width = 3;
        let height = 4;
        let stride = 16;
        let src = shm_buffer(width, height, stride, ShmLayout::Bgrx);

        let upright = to_rgba(&src, width, height, stride, ShmLayout::Bgrx, false).expect("ok");
        let flipped = to_rgba(&src, width, height, stride, ShmLayout::Bgrx, true).expect("ok");

        assert_eq!(pixel(&upright, width, 1, 0), [1, 0, 7, 0xff]);
        // Source row 0 must land on destination row height-1.
        assert_eq!(pixel(&flipped, width, 1, height - 1), [1, 0, 7, 0xff]);
        assert_eq!(pixel(&flipped, width, 1, 0), [1, height as u8 - 1, 7, 0xff]);
        assert_eq!(upright.len(), flipped.len());
    }

    #[test]
    fn odd_dimensions_survive_the_swizzle() {
        // 7x5 with a stride that is not a multiple of anything convenient.
        let src = shm_buffer(7, 5, 30, ShmLayout::Bgrx);
        let out = to_rgba(&src, 7, 5, 30, ShmLayout::Bgrx, false).expect("consistent buffer");
        assert_eq!(out.len(), 7 * 5 * 4);
        assert_eq!(pixel(&out, 7, 6, 4), [6, 4, 7, 0xff]);
    }

    #[test]
    fn a_stride_narrower_than_the_row_is_rejected() {
        let src = vec![0u8; 64];
        assert!(to_rgba(&src, 4, 4, 8, ShmLayout::Bgrx, false).is_none());
    }

    #[test]
    fn a_short_buffer_is_rejected_rather_than_read_out_of_bounds() {
        let src = vec![0u8; 4 * 4 * 4 - 1];
        assert!(to_rgba(&src, 4, 4, 16, ShmLayout::Bgrx, false).is_none());
    }

    #[test]
    fn zero_dimensions_are_rejected() {
        let src = vec![0u8; 16];
        assert!(to_rgba(&src, 0, 4, 16, ShmLayout::Bgrx, false).is_none());
        assert!(to_rgba(&src, 4, 0, 16, ShmLayout::Bgrx, false).is_none());
    }

    #[test]
    fn absurd_dimensions_do_not_overflow() {
        let src = vec![0u8; 16];
        assert!(to_rgba(&src, u32::MAX, u32::MAX, u32::MAX, ShmLayout::Bgrx, false).is_none());
    }

    // -- output transform --------------------------------------------------

    /// A 3x2 image whose pixels encode their own coordinates (R = x, G = y),
    /// so a transform can be checked corner by corner rather than by
    /// counting bytes.
    fn coordinate_rgba(width: u32, height: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[x as u8, y as u8, 0, 0xff]);
            }
        }
        pixels
    }

    fn at(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * width + x) * 4) as usize;
        match pixels.get(offset..offset + 4) {
            Some([r, g, b, a]) => [*r, *g, *b, *a],
            _ => [0, 0, 0, 0],
        }
    }

    #[test]
    fn a_normal_transform_returns_the_buffer_untouched() {
        let src = coordinate_rgba(3, 2);
        let (out, w, h) =
            undo_output_transform(src.clone(), 3, 2, wl_output::Transform::Normal).expect("ok");
        assert_eq!((w, h), (3, 2));
        assert_eq!(out, src);
    }

    /// The case caught live in nested niri: the winit output reports
    /// `flipped vertically` (`Flipped180`), and the raw capture came out
    /// upside down. The correction is a vertical mirror.
    #[test]
    fn flipped180_is_a_vertical_mirror() {
        let src = coordinate_rgba(3, 2);
        let (out, w, h) =
            undo_output_transform(src, 3, 2, wl_output::Transform::Flipped180).expect("ok");
        assert_eq!((w, h), (3, 2), "a vertical mirror keeps the dimensions");
        // Source row 0 must come out as row h-1.
        assert_eq!(at(&out, w, 0, 1), [0, 0, 0, 0xff]);
        assert_eq!(at(&out, w, 2, 0), [2, 1, 0, 0xff]);
    }

    #[test]
    fn flipped_is_a_horizontal_mirror() {
        let src = coordinate_rgba(3, 2);
        let (out, w, h) =
            undo_output_transform(src, 3, 2, wl_output::Transform::Flipped).expect("ok");
        assert_eq!((w, h), (3, 2));
        assert_eq!(at(&out, w, 0, 0), [2, 0, 0, 0xff]);
        assert_eq!(at(&out, w, 2, 1), [0, 1, 0, 0xff]);
    }

    #[test]
    fn rotations_swap_the_dimensions_and_round_trip() {
        let src = coordinate_rgba(4, 3);

        let (rotated, w, h) =
            undo_output_transform(src.clone(), 4, 3, wl_output::Transform::_90).expect("ok");
        assert_eq!((w, h), (3, 4), "a quarter turn swaps width and height");

        // `_90`'s correction is `_270` and vice versa, so applying the other
        // one to the result must give the original image back. That is the
        // strongest check available without a rotated monitor: it pins the
        // two mappings as exact inverses of each other even though which one
        // the compositor means cannot be verified here (see
        // `undo_output_transform`'s doc comment).
        let (back, bw, bh) =
            undo_output_transform(rotated, w, h, wl_output::Transform::_270).expect("ok");
        assert_eq!((bw, bh), (4, 3));
        assert_eq!(back, src);
    }

    #[test]
    fn a_half_turn_is_its_own_inverse() {
        let src = coordinate_rgba(4, 3);
        let (once, w, h) =
            undo_output_transform(src.clone(), 4, 3, wl_output::Transform::_180).expect("ok");
        let (twice, ..) =
            undo_output_transform(once, w, h, wl_output::Transform::_180).expect("ok");
        assert_eq!(twice, src);
    }

    #[test]
    fn flipped90_is_a_transpose() {
        let src = coordinate_rgba(4, 3);
        let (out, w, h) =
            undo_output_transform(src, 4, 3, wl_output::Transform::Flipped90).expect("ok");
        assert_eq!((w, h), (3, 4));
        // Transpose: destination (x, y) is source (y, x).
        assert_eq!(at(&out, w, 2, 3), [3, 2, 0, 0xff]);
        assert_eq!(at(&out, w, 0, 3), [3, 0, 0, 0xff]);
    }

    #[test]
    fn every_reflection_is_its_own_inverse_and_rotations_swap() {
        use wl_output::Transform as T;
        assert_eq!(invert_transform(T::Normal), T::Normal);
        assert_eq!(invert_transform(T::_90), T::_270);
        assert_eq!(invert_transform(T::_180), T::_180);
        assert_eq!(invert_transform(T::_270), T::_90);
        for flipped in [T::Flipped, T::Flipped90, T::Flipped180, T::Flipped270] {
            assert_eq!(invert_transform(flipped), flipped);
        }
    }

    // -- format/offer selection -------------------------------------------

    #[test]
    fn known_formats_map_to_the_right_byte_order() {
        assert_eq!(
            layout_for_format(wl_shm::Format::Xrgb8888),
            Some(ShmLayout::Bgrx)
        );
        assert_eq!(
            layout_for_format(wl_shm::Format::Argb8888),
            Some(ShmLayout::Bgrx)
        );
        assert_eq!(
            layout_for_format(wl_shm::Format::Xbgr8888),
            Some(ShmLayout::Rgbx)
        );
        assert_eq!(layout_for_format(wl_shm::Format::Yuv420), None);
    }

    #[test]
    fn pick_offer_takes_the_first_readable_offer() {
        let offers = vec![
            ShmOffer {
                format: WEnum::Value(wl_shm::Format::Yuv420),
                width: 8,
                height: 8,
                stride: 8,
            },
            ShmOffer {
                format: WEnum::Value(wl_shm::Format::Xrgb8888),
                width: 2560,
                height: 1600,
                stride: 10240,
            },
        ];
        let (offer, format, layout) = pick_offer(&offers).expect("one offer is readable");
        assert_eq!(format, wl_shm::Format::Xrgb8888);
        assert_eq!(offer.width, 2560);
        assert_eq!(offer.stride, 10240);
        assert_eq!(layout, ShmLayout::Bgrx);
    }

    #[test]
    fn pick_offer_reports_the_format_it_could_not_use() {
        let offers = vec![ShmOffer {
            format: WEnum::Unknown(0x3231564e),
            width: 8,
            height: 8,
            stride: 32,
        }];
        let err = pick_offer(&offers).expect_err("nothing readable");
        assert!(
            matches!(err, CaptureError::UnsupportedFormat(0x3231564e)),
            "got {err}"
        );
    }

    #[test]
    fn pick_offer_rejects_an_empty_offer_list() {
        assert!(pick_offer(&[]).is_err());
    }

    // -- the shm file -----------------------------------------------------

    /// The one piece of `unsafe` in this module, exercised directly: a
    /// memfd of the requested size that can be read back through the same
    /// `File`.
    #[test]
    fn shm_file_is_allocated_at_the_requested_size_and_readable() {
        let file = shm_file(4096).expect("memfd_create is available on Linux");
        let metadata = file.metadata().expect("fstat on a memfd");
        assert_eq!(metadata.len(), 4096);

        let mut bytes = vec![0xFF_u8; 4096];
        file.read_exact_at(&mut bytes, 0).expect("read back");
        assert!(
            bytes.iter().all(|byte| *byte == 0),
            "a fresh memfd reads as zeroes"
        );
    }
}
