//! `EncoderSink` — the boundary between "we have raw frames" and "there is a
//! video file" (PLAN.md Architecture: one of this crate's **two trait
//! boundaries**, alongside `CaptureBackend`).
//!
//! `ffmpeg_cli.rs` is the only v0.1 implementation and it is an *external
//! CLI* boundary, never a library link (CLAUDE.md Boundaries: "Never link
//! ffmpeg/libav libraries"). The trait is what lets an in-process encoder —
//! or a "no encoder installed" degradation — replace it later without any
//! caller learning about it.
//!
//! # What lives here vs. in `ffmpeg_cli.rs`
//!
//! Everything in this file is **pure**: the preset tables (straight out of
//! `docs/CAPTURE-RESEARCH.md` §3), the specs describing one recording, and
//! [`select_encoder`] — the runtime VAAPI device choice, expressed as a
//! function over an injected capability oracle so it can be unit-tested
//! against fake probe results with no GPU in the room (PLAN.md Stage 11 task
//! 3: "Selection logic is pure and unit-tested with fake probe results").
//! `ffmpeg_cli.rs` owns everything that touches the world: enumerating
//! `/dev/dri`, spawning the trial encodes that answer the oracle, and running
//! the real child process.
//!
//! # Deliberate deviation from PLAN.md's trait sketch (read before "fixing" it)
//!
//! Architecture sketches the trait as `write_video(frame, pts)`. **There is
//! no `pts` argument here, on purpose.** CAPTURE-RESEARCH D6/§4.2 pins the
//! timestamp model at `-use_wallclock_as_timestamps 1`: ffmpeg stamps each
//! frame when its bytes arrive on stdin, and the Stage 10 handoff is explicit
//! that `VideoFrame::captured_at` "is NOT a PTS … Do not mux from it". A
//! `pts` parameter would therefore be a parameter every implementation must
//! ignore, which is a worse lie than not having it. The timing model this
//! crate actually implements is written out on [`VideoSpec`].
//!
//! Similarly, `start(video_spec, audio_spec, preset, path)` is **not** a trait
//! method: a constructor cannot be dispatched through a `dyn` object, and the
//! four arguments are exactly [`RecordSpec`]'s fields, so each implementation
//! offers its own `start(&RecordSpec)` inherent constructor
//! (`ffmpeg_cli::FfmpegSink::start`) and the trait covers only what a *live*
//! sink can be asked to do.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::config::VideoPreset;

pub mod export;
pub mod ffmpeg_cli;

/// The default video bitrate for both hardware presets — CAPTURE-RESEARCH
/// §3.7's `-b:v 20M`, measured at 2560×1600 (it produced ~23 Mbit/s of real
/// output on a synthetic worst case, §3.1). Not a config knob yet; Stage 12+
/// can promote it if anyone asks.
pub const VIDEO_BITRATE: &str = "20M";

/// The AV1 **software** cap, in frames per second — CAPTURE-RESEARCH §3.4:
/// `libsvtav1 -preset 10` runs at ~0.80× realtime at 2560×1600@60 but
/// **1.26–1.39×** when fed 30 fps. D6 says "cap the AV1 preset at 30 fps or
/// document the drops"; this crate caps, via an `fps` filter (see
/// [`filter_chain`]).
pub const AV1_SOFTWARE_FPS_CAP: u32 = 30;

/// The **time base** of the rawvideo input, in ticks per second, expressed
/// the only way ffmpeg's rawvideo demuxer lets you express it: as a nominal
/// `-framerate`.
///
/// # This is not a frame rate, and getting that wrong is the §4.2 bug
///
/// CAPTURE-RESEARCH §4.2 measured that feeding `-framerate 60` *alone*
/// fast-forwards a variable-rate cast 3×, and D6 concluded "no frame rate in
/// [`VideoSpec`]". That conclusion is about **where timestamps come from** —
/// which is still, exclusively, `-use_wallclock_as_timestamps 1`. What this
/// constant sets is the *resolution those timestamps are stored at*: the
/// rawvideo demuxer's stream time base is `1/framerate`, and ffmpeg rescales
/// each wallclock timestamp into it.
///
/// **Stage 13 found the consequence of leaving it at the default.** With no
/// `-framerate` the demuxer defaults to 25, so every frame's PTS was snapped
/// to a **40 ms grid** — and `-fps_mode:v vfr` then discards frames that land
/// in a slot already taken. Measured live (transcripts in the Stage 13
/// handoff): a 60 fps source through the exact Stage 11 command line came out
/// as **24.8 fps, 137 frames**; with `-framerate 1000` the same source came
/// out as **66.2 fps, 360 frames**, PTS on a 1 ms grid, duration and
/// keyframe interval unchanged. Every recording this project made before
/// Stage 13 was therefore effectively 25 fps with motion quantised to 40 ms.
///
/// 1000 rather than something finer: it is exactly Matroska's own default
/// time base (the output already reports `1k tbn`), so nothing is lost on the
/// way out, and 1 ms is an order of magnitude finer than a 120 Hz display's
/// 8.3 ms refresh. It also matters for A/V sync — a 40 ms video grid is
/// ±20 ms of unfixable error against an audio stream timestamped in
/// microseconds, which is the same order as the offset [`AudioSpec::itsoffset`]
/// exists to correct.
///
/// One cosmetic cost, seen live: ffmpeg logs
/// `Stream #0: not enough frames to estimate rate` once per recording at
/// `warning`, because a declared 1000 fps is not confirmable from the first
/// few frames. It is a note about the *nominal* rate this crate never uses.
pub const RAWVIDEO_TIMEBASE_HZ: &str = "1000";

// ---------------------------------------------------------------------
// Presets — CAPTURE-RESEARCH §3.7's table, as data
// ---------------------------------------------------------------------

/// Which of the three shipped encode recipes a recording uses.
///
/// A thin wrapper over [`VideoPreset`] (the `capture.toml`/`--preset`
/// vocabulary) rather than the same type, because the two answer different
/// questions: `VideoPreset` is *what the user asked for*, `EncodePreset` is
/// *what this module knows how to build a command line for*. They happen to
/// be isomorphic today; keeping the conversion explicit means a future
/// preset that the config accepts but an encoder can't serve has somewhere to
/// fail loudly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodePreset {
    /// The primary: `hevc_vaapi` → Matroska. Real hardware, comfortably
    /// realtime (§3.1).
    Hevc,
    /// AV1 → Matroska. **No AMD part tested so far has an AV1 encode
    /// entrypoint** (§3.3, re-confirmed live in Stage 11: the trial encode of
    /// `av1_vaapi` on `/dev/dri/renderD128` failed in 179 ms), so this
    /// normally resolves to software `libsvtav1` — see [`select_encoder`].
    Av1,
    /// `h264_vaapi` → MP4. The compatibility preset (§3.5).
    H264,
}

impl EncodePreset {
    pub fn from_config(preset: VideoPreset) -> Self {
        match preset {
            VideoPreset::Hevc => Self::Hevc,
            VideoPreset::Av1 => Self::Av1,
            VideoPreset::H264 => Self::H264,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
            Self::H264 => "h264",
        }
    }

    /// The file extension recordings under this preset get. Chosen by the
    /// container, not the codec: HEVC and AV1 both go in Matroska (which
    /// tolerates a truncated file — see [`RecordSpec::path`]'s note on why
    /// recordings are *not* written through `storage::write_atomically`),
    /// H.264 goes in MP4 because that is the point of having an H.264 preset
    /// at all.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Hevc | Self::Av1 => "mkv",
            Self::H264 => "mp4",
        }
    }

    /// ffmpeg's own name for that container (`-f <muxer>`), stated explicitly
    /// rather than inferred from the extension so the output path can be
    /// anything (including a `.part` name, should a later stage want one)
    /// without silently changing the muxer.
    pub fn muxer(self) -> &'static str {
        match self {
            Self::Hevc | Self::Av1 => "matroska",
            Self::H264 => "mp4",
        }
    }

    /// The encoder this preset wants when a VAAPI device supports it.
    fn preferred_encoder(self) -> VideoEncoder {
        match self {
            Self::Hevc => VideoEncoder::HevcVaapi,
            Self::Av1 => VideoEncoder::Av1Vaapi,
            Self::H264 => VideoEncoder::H264Vaapi,
        }
    }
}

impl fmt::Display for EncodePreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One concrete ffmpeg encoder. Separate from [`EncodePreset`] because the
/// preset is a *request* and this is the *answer* — `av1` resolves to either
/// [`Self::Av1Vaapi`] or [`Self::LibSvtAv1`] depending on what the machine
/// turns out to have (PLAN.md Stage 11 task 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoEncoder {
    HevcVaapi,
    H264Vaapi,
    Av1Vaapi,
    /// Software AV1. The only encoder here that needs no VAAPI device, and
    /// therefore the only reason `av1` still works on a machine with no
    /// usable render node at all (PLAN.md Stage 11 task 3's last sentence).
    LibSvtAv1,
}

impl VideoEncoder {
    /// ffmpeg's `-c:v` name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HevcVaapi => "hevc_vaapi",
            Self::H264Vaapi => "h264_vaapi",
            Self::Av1Vaapi => "av1_vaapi",
            Self::LibSvtAv1 => "libsvtav1",
        }
    }

    /// Whether this encoder consumes VAAPI surfaces — which decides both
    /// whether a `-vaapi_device` is needed and which filter chain
    /// ([`filter_chain`]) applies.
    pub fn is_vaapi(self) -> bool {
        !matches!(self, Self::LibSvtAv1)
    }

    /// The input frame rate cap this encoder needs to stay realtime, if any.
    /// Only software AV1 has one (§3.4).
    pub fn fps_cap(self) -> Option<u32> {
        match self {
            Self::LibSvtAv1 => Some(AV1_SOFTWARE_FPS_CAP),
            _ => None,
        }
    }

    /// The codec-specific tail of the output options, after `-c:v <name>`.
    /// CAPTURE-RESEARCH §3.7 verbatim.
    fn codec_args(self) -> Vec<String> {
        match self {
            Self::HevcVaapi | Self::H264Vaapi | Self::Av1Vaapi => {
                vec!["-b:v".to_string(), VIDEO_BITRATE.to_string()]
            }
            Self::LibSvtAv1 => vec![
                "-preset".to_string(),
                "10".to_string(),
                "-crf".to_string(),
                "35".to_string(),
                "-g".to_string(),
                "120".to_string(),
            ],
        }
    }
}

impl fmt::Display for VideoEncoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What [`select_encoder`] decided: an encoder, plus the render node it must
/// be pointed at (`None` for the software path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderChoice {
    pub encoder: VideoEncoder,
    pub vaapi_device: Option<PathBuf>,
}

impl fmt::Display for EncoderChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.vaapi_device {
            Some(device) => write!(f, "{} on {}", self.encoder, device.display()),
            None => write!(f, "{} (software)", self.encoder),
        }
    }
}

// ---------------------------------------------------------------------
// The runtime device choice (pure half)
// ---------------------------------------------------------------------

/// Pick the encoder and render node for `preset`, given `devices` (in
/// preference order) and a `supports` oracle that answers "can this device
/// open this encoder?".
///
/// # Why this takes an oracle instead of a `Vec<Capabilities>`
///
/// Every `supports` call in the real implementation is a **fork+exec of a
/// trial ffmpeg encode** (~250 ms measured, ×N devices ×M codecs — see
/// `ffmpeg_cli::probe_encoder`). Passing a pre-filled capability table would
/// force every probe to run before the first decision; passing a closure lets
/// the rules below short-circuit, so the common case (HEVC on a machine with
/// no AV1 anywhere) costs `devices.len()` AV1 probes plus one HEVC probe
/// rather than the full cross-product. It also makes the whole function
/// trivially testable with a fake table — PLAN.md Stage 11 task 3's
/// requirement.
///
/// # The rules (PLAN.md Stage 11 task 3, verbatim)
///
/// > enumerate `/dev/dri/renderD*` and pick the node: prefer one with a
/// > working AV1 encode entrypoint, else the first with a working encode
/// > entrypoint for the chosen preset.
///
/// so:
///
/// 1. **`av1` preset** — the first device that opens `av1_vaapi`; failing
///    that, software `libsvtav1`, which needs no device and therefore
///    **cannot fail here**. This is why "no usable VAAPI node" is only fatal
///    for the other two presets.
/// 2. **`hevc`/`h264`** — first pass: a device that opens *both* `av1_vaapi`
///    and the preset's own encoder (the "prefer an AV1-capable node" rule,
///    which exists so a future AV1-capable dGPU wins the tie rather than
///    whichever node happened to be enumerated first). Second pass: the first
///    device that opens the preset's own encoder at all. No device → an
///    actionable [`EncodeError::NoVaapiDevice`].
///
/// **Never hardcode `/dev/dri/renderD128`** (CAPTURE-RESEARCH D6's 2026-08-08
/// amendment). Stage 11's live verification (2026-08-09) enumerated exactly
/// one node — `/dev/dri/renderD128`, the 680M — and resolved `hevc` to it in
/// 330 ms of probing. That is the amendment's premise, not a refutation of
/// it: the dGPU is *normally* disabled, so the node count on this machine is
/// a runtime fact that changes, which is precisely why it is discovered here
/// and why the two-node cases below are unit-tested against fakes rather than
/// against whatever `/dev/dri` happens to hold today.
pub fn select_encoder(
    preset: EncodePreset,
    devices: &[PathBuf],
    mut supports: impl FnMut(&Path, VideoEncoder) -> bool,
) -> Result<EncoderChoice, EncodeError> {
    if preset == EncodePreset::Av1 {
        for device in devices {
            if supports(device, VideoEncoder::Av1Vaapi) {
                return Ok(EncoderChoice {
                    encoder: VideoEncoder::Av1Vaapi,
                    vaapi_device: Some(device.clone()),
                });
            }
        }
        // §3.3's answer on every AMD part measured so far, and the reason the
        // `av1` preset is documented as "not realtime at 2560×1600@60".
        return Ok(EncoderChoice {
            encoder: VideoEncoder::LibSvtAv1,
            vaapi_device: None,
        });
    }

    let wanted = preset.preferred_encoder();

    // Pass 1: an AV1-capable node that also serves this preset. The `&&`
    // short-circuits, so the (more expensive, always-fails-today) preset
    // probe is skipped on every node that has no AV1 entrypoint.
    for device in devices {
        if supports(device, VideoEncoder::Av1Vaapi) && supports(device, wanted) {
            return Ok(EncoderChoice {
                encoder: wanted,
                vaapi_device: Some(device.clone()),
            });
        }
    }

    // Pass 2: any node that serves this preset.
    for device in devices {
        if supports(device, wanted) {
            return Ok(EncoderChoice {
                encoder: wanted,
                vaapi_device: Some(device.clone()),
            });
        }
    }

    Err(EncodeError::NoVaapiDevice {
        preset,
        probed: devices.to_vec(),
    })
}

// ---------------------------------------------------------------------
// Specs — what one recording is
// ---------------------------------------------------------------------

/// The raw video stream this crate feeds ffmpeg's stdin.
///
/// # The timing model (binding — CAPTURE-RESEARCH §4.2/D6)
///
/// There is **no frame rate in this struct**, and that is the whole point. A
/// niri cast negotiates `framerate 0/1` (variable) and a window cast can go
/// seconds between frames, so the naive `-framerate N` input produced a video
/// **3× fast-forwarded** in Stage 2's measurement. The rate is carried
/// entirely by *when the bytes reach ffmpeg's stdin*, via
/// `-use_wallclock_as_timestamps 1` plus `-fps_mode:v vfr` on the output.
/// Consequences worth knowing before Stage 13 adds audio:
///
/// - The recorder must write each frame to stdin **promptly** — a frame
///   buffered for 200 ms is a frame timestamped 200 ms late. This is why the
///   pump loop (`modules::recorder::pump_frames`) does nothing between
///   `recv` and `write_all`.
/// - ffmpeg's clock starts when ffmpeg starts, so it is spawned **after the
///   first frame is already in hand** (§4.4's mitigation 1) — otherwise the
///   gap between "ffmpeg opened the audio device" and "the first video frame"
///   becomes silent A/V desync that `-copyts` cannot rescue (§4.3).
/// - `captured_at` on a `VideoFrame` is a **diagnostic**, never a PTS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoSpec {
    /// The width of the frames actually written, in pixels.
    ///
    /// **This is the negotiated width, not the even one.** `-video_size` has
    /// to describe the bytes on the pipe or ffmpeg mis-frames every row from
    /// the second frame onward; the *even* size is reached with a `crop`
    /// filter instead (see [`filter_chain`] and §3.6). Getting these two
    /// backwards produces a progressively sheared video rather than an error,
    /// which is why they are separate concepts here.
    pub width: u32,
    pub height: u32,
    /// **Stage 12.** A sub-rectangle of the negotiated frame to encode,
    /// physical pixels, for `record start --region` — CAPTURE-RESEARCH D8:
    /// there is no `RecordArea`, so a region recording is a full
    /// [`CastTarget::Monitor`](crate::capture::screencast::CastTarget::Monitor)
    /// cast cropped in this same `-vf crop` filter that already exists for
    /// the even-dimension rule, just pointed at a caller-chosen rectangle
    /// instead of `(0, 0)`. `None` means "the whole (even-cropped) frame" —
    /// every fullscreen recording, and every window recording (a window
    /// cast's own negotiated frame already *is* just that window, so it
    /// never needs a second crop on top).
    ///
    /// The rectangle is [`crate::capture::PixelRect`] — the same type
    /// `capture::logical_to_pixel_rect` produces for a screenshot region —
    /// reused rather than duplicated, since `encode` already depends on
    /// `capture::screencast` for [`Self::from_negotiated`].
    pub crop: Option<crate::capture::PixelRect>,
}

impl VideoSpec {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            crop: None,
        }
    }

    /// Builder for the region-recording case — see [`Self::crop`].
    pub fn with_crop(mut self, crop: crate::capture::PixelRect) -> Self {
        self.crop = Some(crop);
        self
    }

    /// Straight from Stage 10's negotiated SPA format.
    pub fn from_negotiated(format: &crate::capture::screencast::NegotiatedFormat) -> Self {
        Self::new(format.width, format.height)
    }

    /// `-pixel_format`. Fixed: PipeWire negotiates `SPA_VIDEO_FORMAT_BGRx`
    /// and `capture::screencast` hands over packed B G R X rows, which is
    /// exactly ffmpeg's `bgr0` (§3.2's byte-order sanity check). No swizzle
    /// happens anywhere in this crate.
    pub const PIXEL_FORMAT: &'static str = "bgr0";

    /// Bytes in one frame as written to the pipe.
    pub fn frame_len(&self) -> usize {
        (self.width as usize)
            .saturating_mul(self.height as usize)
            .saturating_mul(4)
    }

    /// The dimensions the *encoder* sees, both rounded down to even.
    ///
    /// §3.6, live-measured: `hevc_vaapi` accepts a 2507×1457 input without a
    /// warning and emits a **2508×1458** stream, padding the extra row and
    /// column with whatever it felt like. `libsvtav1` keeps odd sizes. Rather
    /// than have two behaviours, every preset crops to even here.
    pub fn even_dimensions(&self) -> (u32, u32) {
        (self.width & !1, self.height & !1)
    }
}

/// Guards a running recording against the cast changing size underneath it.
///
/// # Why this exists (a failure mode with no error of its own)
///
/// `-video_size` is fixed for an ffmpeg process's whole life, and the raw
/// pipe carries no framing — ffmpeg simply reads `width * height * 4` bytes
/// and calls that a frame. PipeWire's `param_changed` callback, meanwhile,
/// can legitimately fire **more than once**: a compositor may renegotiate
/// (an output mode change, a window resize on a window cast). If it does,
/// every subsequent frame is a different length, and the encoder keeps
/// happily slicing the stream at the old size — producing a video that
/// progressively shears rather than an error anybody could act on.
///
/// So a renegotiation to a *different* size ends the recording cleanly (a
/// stated error, a flushed file, a toast) instead. A renegotiation to the
/// *same* size is ignored, because that is a no-op the compositor is entitled
/// to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedGuard {
    spec: VideoSpec,
}

impl NegotiatedGuard {
    pub fn new(spec: VideoSpec) -> Self {
        Self { spec }
    }

    /// `Ok(())` if `format` still describes the frames this recording was
    /// started for; otherwise the message the recording dies with.
    pub fn check(
        &self,
        format: &crate::capture::screencast::NegotiatedFormat,
    ) -> Result<(), String> {
        if format.width == self.spec.width && format.height == self.spec.height {
            return Ok(());
        }
        Err(format!(
            "the screencast renegotiated from {}x{} to {}x{} mid-recording — ffmpeg's input size \
             is fixed once it starts, so the recording has been stopped rather than continued \
             with mis-framed video",
            self.spec.width, self.spec.height, format.width, format.height
        ))
    }
}

/// The audio half of one recording — **real as of Stage 13** (Stage 11 wrote
/// the shape; nothing constructed it until now).
///
/// The three mandatory mitigations CAPTURE-RESEARCH §4.4 attaches to the
/// `-f pulse` transport, and where each one lands:
///
/// 1. *Spawn ffmpeg only after the first video frame* — already true since
///    Stage 11 (`dbus::CaptureService`'s start sequence), for an unrelated
///    reason: `-video_size` needs the negotiated size. This is also why the
///    residual offset Stage 13 measured is small enough to ship uncorrected
///    (see [`crate::audio::DEFAULT_SYNC_OFFSET`]).
/// 2. *`-shortest` plus a graceful stop* — `-shortest` is emitted by
///    [`ffmpeg_args`] whenever audio is present (`-f pulse` is an infinite
///    input and **will** hang the process otherwise); the graceful stop is
///    `ffmpeg_cli::FfmpegSink::finish`'s SIGINT escalation. Measured live in
///    Stage 13: with `-shortest`, closing stdin is enough — ffmpeg finalises
///    and exits without ever reaching the SIGINT.
/// 3. *Measure the residual offset with the clap test and bake it in* —
///    [`Self::itsoffset`].
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSpec {
    /// One or two PulseAudio (PipeWire pulse shim) source names, resolved at
    /// record time by [`crate::audio::plan_audio`] — **never hardcoded**,
    /// they are hardware-path-derived (§4.4).
    ///
    /// Two names means `--audio both`, which is **one mixed track, not two
    /// tracks** — see [`mix_arguments`].
    pub sources: Vec<String>,
    /// `-itsoffset` on every audio input, in seconds. §4.3 measured that both
    /// ffmpeg's default normalisation and `-copyts -start_at_zero` silently
    /// discard the real offset between the two inputs, and that `-itsoffset`
    /// is the one knob that survives normalisation. `None` (the shipped
    /// default) emits no argument at all.
    pub itsoffset: Option<f64>,
}

/// Everything one recording needs — PLAN.md Architecture's
/// `start(video_spec, audio_spec, preset, path)`, as a struct (see this
/// module's doc comment for why it is not a trait method).
#[derive(Debug, Clone, PartialEq)]
pub struct RecordSpec {
    pub video: VideoSpec,
    pub audio: Option<AudioSpec>,
    pub preset: EncodePreset,
    /// Where the finished recording goes.
    ///
    /// **ffmpeg writes here directly** — deliberately *not* through
    /// `storage::write_atomically`'s `.part`+`rename`, which every still
    /// image uses. A screenshot is encoded in memory and written in one shot,
    /// so there is an instant where "complete" is knowable; a recording is
    /// written incrementally by an external process over minutes, and the
    /// only thing a rename would add is that a recording interrupted by a
    /// crash (or a full disk) disappears instead of being recoverable.
    /// Matroska is explicitly designed to survive truncation, which is part
    /// of why it is the primary container.
    pub path: PathBuf,
}

// ---------------------------------------------------------------------
// The ffmpeg command line (pure)
// ---------------------------------------------------------------------

/// The `-vf` chain for one encoder, at one frame size.
///
/// Two shapes, and the split is by *encoder*, not by preset (an
/// `av1_vaapi` that some future machine actually has takes the hardware
/// chain, not the software one):
///
/// - **VAAPI:** `crop,hwupload,scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=tv`.
///   The colour conversion happens on the GPU — §3.2 measured **10× less
///   CPU** than `format=nv12,hwupload` (0.34 s vs 3.50 s per 2 s of video) —
///   and the matrix/range tagging is **not optional**: bare `scale_vaapi`
///   shifted red by 23/255 in the four-colour round trip.
/// - **Software:** `crop,fps=30,format=yuv420p` — the §3.4 rate cap plus the
///   pixel format `libsvtav1` wants.
///
/// `crop` is first in both, and is a *pointer-adjusting* filter (it changes
/// width/height and the data offset; it copies nothing), so an already-even
/// frame pays essentially nothing for a crop that removes zero pixels.
///
/// **Stage 12:** when [`VideoSpec::crop`] is `Some`, that rectangle (rounded
/// down to even width/height, same rule as the no-crop case — `hevc_vaapi`
/// cares just as much about a region recording's dimensions being even as it
/// does about a fullscreen one's) replaces the `(0, 0, even_width,
/// even_height)` default. The offset itself is **not** rounded — `crop`'s `x`/
/// `y` are a pointer offset, not a dimension libva reasons about chroma
/// subsampling over, and rounding it would drift the recording off the
/// selected rectangle by up to a pixel for no benefit.
pub fn filter_chain(encoder: VideoEncoder, video: &VideoSpec) -> String {
    let (width, height, x, y) = match video.crop {
        Some(rect) => (rect.width & !1, rect.height & !1, rect.x, rect.y),
        None => {
            let (width, height) = video.even_dimensions();
            (width, height, 0, 0)
        }
    };
    let mut chain = format!("crop={width}:{height}:{x}:{y}");
    if let Some(cap) = encoder.fps_cap() {
        chain.push_str(&format!(",fps={cap}"));
    }
    if encoder.is_vaapi() {
        chain.push_str(",hwupload,scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=tv");
    } else {
        chain.push_str(",format=yuv420p");
    }
    chain
}

/// The `amix` parameters `--audio both` mixes its two inputs with.
///
/// **One mixed track, not two tracks** — CAPTURE-RESEARCH §4.4's own answer
/// ("`both` → two `-f pulse` inputs plus `amix`"), and the reasons hold up:
/// Matroska could carry two tracks, but the `h264` preset's MP4 container is
/// the *compatibility* preset and track-switching is exactly what its
/// audience will not do, and every consumer of a screen recording so far
/// (upload it, send it, scrub it) wants one audible thing. A future stage
/// that wants separate tracks should add a *third* `--audio` spelling rather
/// than change what `both` means.
///
/// `normalize=0` is the parameter worth arguing about. `amix` defaults to
/// `normalize=1`, which divides every input by the input count — so adding a
/// microphone would make the system audio 6 dB quieter than the same
/// recording without one, which is a surprising thing for adding a microphone
/// to do. With `normalize=0` each source keeps its own level; the risk is
/// clipping when both are loud at once, which the user can fix by turning a
/// source down, while a 6 dB loss is not fixable by anything they can see.
const AMIX_PARAMS: &str = "duration=longest:normalize=0";

/// The `-filter_complex` graph a two-source recording needs, plus the `-map`
/// pair that goes with it — `None` for the one-source case, which needs
/// neither (ffmpeg's automatic stream selection picks the single video and
/// single audio stream correctly, which is what Stage 11's own tests already
/// describe).
///
/// **The video keeps its plain `-vf` chain even here.** A complex filtergraph
/// switches ffmpeg's automatic stream selection off, so both output streams
/// have to be named — but the video is still mapped straight from input 0 and
/// `-vf` still applies to it, so the hardware chain
/// ([`filter_chain`]) is byte-identical between a mixed recording and a
/// video-only one. Verified live in Stage 13 (CAPTURE-RESEARCH §4.5) rather
/// than assumed, because the failure mode — `-vf` silently ignored — would
/// send un-cropped, un-uploaded frames at the VAAPI encoder.
pub fn mix_arguments(audio: &AudioSpec) -> Option<Vec<String>> {
    if audio.sources.len() < 2 {
        return None;
    }
    // Input 0 is the raw video on stdin, so the audio inputs are 1..=n.
    let labels: String = (1..=audio.sources.len())
        .map(|index| format!("[{index}:a]"))
        .collect();
    Some(vec![
        "-filter_complex".to_string(),
        format!(
            "{labels}amix=inputs={}:{AMIX_PARAMS}[aout]",
            audio.sources.len()
        ),
        "-map".to_string(),
        "0:v:0".to_string(),
        "-map".to_string(),
        "[aout]".to_string(),
    ])
}

/// The complete ffmpeg argv (everything after the program name) for one
/// recording — CAPTURE-RESEARCH §3.7's table plus §4.2's timing model and
/// §3.6's even-dimension crop, assembled in ffmpeg's own grammar:
/// *global options, then per-input options and inputs, then output options,
/// then the output*.
///
/// `loglevel` is `-loglevel`'s value. `ffmpeg_cli` defaults it to `warning`
/// and lets `$SAOLA_CAPTURE_FFMPEG_LOGLEVEL` override it, because "is this
/// really hardware?" is answered by lines ffmpeg only prints at `verbose`
/// (§3.1's `VAEntrypointEncSlice`).
pub fn ffmpeg_args(spec: &RecordSpec, choice: &EncoderChoice, loglevel: &str) -> Vec<String> {
    /// Appends a run of literal flags. A free function rather than a closure
    /// capturing `args`, so it does not hold a borrow across the
    /// `args.push(..)` calls interleaved with it.
    fn push(args: &mut Vec<String>, values: &[&str]) {
        args.extend(values.iter().map(|value| (*value).to_string()));
    }

    let mut args: Vec<String> = Vec::new();

    // -- global -------------------------------------------------------
    push(
        &mut args,
        &["-hide_banner", "-nostats", "-loglevel", loglevel],
    );
    // Overwrite: the path was just allocated by `storage`, so an existing
    // file means a collision this process created; `-n` would leave ffmpeg
    // waiting on a prompt it can never receive (its stdin is our video pipe).
    push(&mut args, &["-y"]);

    // -- input 0: raw video on stdin ----------------------------------
    push(
        &mut args,
        &["-f", "rawvideo", "-pixel_format", VideoSpec::PIXEL_FORMAT],
    );
    args.push("-video_size".to_string());
    args.push(format!("{}x{}", spec.video.width, spec.video.height));
    // **Stage 13.** The input *time base*, not a frame rate — read
    // [`RAWVIDEO_TIMEBASE_HZ`] before touching this pair; the two lines are
    // only safe together.
    push(&mut args, &["-framerate", RAWVIDEO_TIMEBASE_HZ]);
    // §4.2: mandatory. Without it a variable-rate cast becomes a
    // fast-forwarded video, silently.
    push(&mut args, &["-use_wallclock_as_timestamps", "1"]);
    push(&mut args, &["-i", "pipe:0"]);

    // -- inputs 1..n: audio (Stage 13) --------------------------------
    // One `-f pulse` input per resolved source — one for `mic`/`system`, two
    // for `both`. `-itsoffset` is a *per-input* option and so is repeated,
    // not stated once: both inputs are opened by the same ffmpeg at the same
    // moment, so the same correction applies to each.
    if let Some(audio) = &spec.audio {
        for source in &audio.sources {
            if let Some(offset) = audio.itsoffset {
                args.push("-itsoffset".to_string());
                args.push(format!("{offset}"));
            }
            push(&mut args, &["-f", "pulse"]);
            args.push("-i".to_string());
            args.push(source.clone());
        }
    }

    // -- output -------------------------------------------------------
    if let Some(device) = &choice.vaapi_device {
        args.push("-vaapi_device".to_string());
        args.push(device.to_string_lossy().into_owned());
    }
    // The mix graph, if any — before `-vf`/`-map`-less output options, since
    // it introduces the labels the `-map`s in it refer to.
    if let Some(mix) = spec.audio.as_ref().and_then(mix_arguments) {
        args.extend(mix);
    }
    // Stream-qualified (`:v`) rather than §3.7's bare `-fps_mode vfr`: with an
    // audio input present the bare form would also apply to the audio stream,
    // where it means nothing useful. Identical to the measured command line
    // for the video-only case Stage 11 ships.
    push(&mut args, &["-fps_mode:v", "vfr"]);
    args.push("-vf".to_string());
    args.push(filter_chain(choice.encoder, &spec.video));
    args.push("-c:v".to_string());
    args.push(choice.encoder.as_str().to_string());
    args.extend(choice.encoder.codec_args());

    if spec.audio.is_some() {
        push(&mut args, &["-c:a", "libopus", "-b:a", "96k"]);
        // §4.3: `-f pulse` never EOFs. Without `-shortest` the process
        // outlives its video input and has to be killed.
        push(&mut args, &["-shortest"]);
    }

    if spec.preset == EncodePreset::H264 {
        // MP4 only: moves the index to the front so the file is seekable
        // before it is fully downloaded/copied. Meaningless for Matroska.
        push(&mut args, &["-movflags", "+faststart"]);
    }

    args.push("-f".to_string());
    args.push(spec.preset.muxer().to_string());
    args.push(spec.path.to_string_lossy().into_owned());

    args
}

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

/// Everything that can go wrong between "we have frames" and "there is a
/// file". Every variant's `Display` names something the human can *do* —
/// CLAUDE.md's "absent services … produce actionable errors" rule, which for
/// this module is load-bearing: these strings end up in a toast and in the
/// `io.saola.Capture1` `Error` signal, where nobody is watching a terminal.
#[derive(Debug)]
pub enum EncodeError {
    /// No `ffmpeg` on `$PATH`. The one error this crate is contractually
    /// required to phrase well (CLAUDE.md Boundaries: "Its absence is a clean
    /// runtime error naming the install command").
    FfmpegMissing,
    /// `ffmpeg` is present but could not be started (permissions, a broken
    /// `$PATH` entry, fork failure).
    Spawn(std::io::Error),
    /// No render node could open the encoder this preset needs. Only
    /// reachable for `hevc`/`h264` — `av1` always has the software path.
    NoVaapiDevice {
        preset: EncodePreset,
        probed: Vec<PathBuf>,
    },
    /// The child exited (or was killed) when it should have been encoding.
    /// `tail` is the last few stderr lines, which is where "No space left on
    /// device" actually appears.
    Died {
        code: Option<i32>,
        tail: Vec<String>,
    },
    /// Writing a frame to the child's stdin failed for a reason other than
    /// the child being gone.
    Write(std::io::Error),
    /// The child exited cleanly but produced nothing usable.
    EmptyOutput(PathBuf),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::FfmpegMissing => write!(
                f,
                "ffmpeg is not installed (or not on $PATH) — recording needs it: \
                 sudo pacman -S ffmpeg"
            ),
            EncodeError::Spawn(err) => write!(f, "could not start ffmpeg: {err}"),
            EncodeError::NoVaapiDevice { preset, probed } => {
                if probed.is_empty() {
                    write!(
                        f,
                        "the {preset} preset needs a VAAPI render node and there are none \
                         (no /dev/dri/renderD* on this machine) — try `--preset av1`, which \
                         encodes in software"
                    )
                } else {
                    let names: Vec<String> = probed
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect();
                    write!(
                        f,
                        "no render node can encode {preset} (tried {}) — check `vainfo` for an \
                         encode entrypoint, set `vaapi-device` in capture.toml if the right node \
                         is being skipped, or use `--preset av1`, which encodes in software",
                        names.join(", ")
                    )
                }
            }
            // **The stderr tail is only quoted for a coded exit** (found the
            // hard way in Stage 11's live run). ffmpeg prints its reason and
            // *then* exits, so on a status-1 death the last line really is
            // the cause ("No space left on device"). A signal death has no
            // ffmpeg-authored explanation at all — the process was cut off
            // mid-sentence — so whatever line happened to be last is noise,
            // and quoting it after a colon presents it as a cause. The live
            // run produced exactly that: `ffmpeg was killed by a signal:
            // [out#0/matroska @ 0x…] Starting thread...`.
            EncodeError::Died { code, tail } => match code {
                Some(code) => {
                    write!(f, "ffmpeg exited with status {code}")?;
                    if let Some(last) = tail.last() {
                        write!(f, ": {last}")?;
                    }
                    Ok(())
                }
                None => write!(
                    f,
                    "ffmpeg was killed by a signal — whatever it had already written is on disk, \
                     but the file was never finalized"
                ),
            },
            EncodeError::Write(err) => write!(f, "could not feed a frame to ffmpeg: {err}"),
            EncodeError::EmptyOutput(path) => write!(
                f,
                "ffmpeg finished but {} is empty — nothing was encoded",
                path.display()
            ),
        }
    }
}

impl std::error::Error for EncodeError {}

// ---------------------------------------------------------------------
// The trait
// ---------------------------------------------------------------------

/// A live encoder, from the first frame to the finished file.
///
/// `Send` is part of the contract, not an accident: the pump that drives a
/// sink runs on tokio's blocking pool
/// (`modules::recorder::pump_frames`, spawned from `dbus.rs`), so the sink
/// crosses a thread boundary exactly once at the start of a recording and
/// once at the end.
///
/// **Every method must be non-panicking**, including on a sink whose child
/// process died three minutes ago — the whole point of this trait's error
/// type is that a dead encoder becomes a toast, not a dead daemon.
///
/// # There is no `write_audio` (a deliberate omission, PLAN.md sketches one)
///
/// CAPTURE-RESEARCH D7 settled the audio *transport* before this trait
/// existed: audio reaches the file as `-f pulse`, an input **ffmpeg opens
/// itself** from a source name we resolve ([`AudioSpec`]). On that design no
/// PCM ever passes through this crate, so a `write_audio` would be a method
/// no implementation could ever be asked to serve. §4.4 does document one
/// escalation that would change that — PCM on `pipe:3`, taken only if the
/// clap test shows a *non-constant* A/V offset — and that is the stage which
/// should add the method, with a real implementation behind it rather than a
/// `todo!()`-shaped default.
pub trait EncoderSink: Send {
    /// Hand one frame's packed bytes to the encoder. `bytes.len()` must equal
    /// [`VideoSpec::frame_len`]; a caller that cannot guarantee that must
    /// check first (the pump does), because a short write silently desyncs
    /// every subsequent frame rather than failing.
    fn write_video(&mut self, bytes: &[u8]) -> Result<(), EncodeError>;

    /// Cheap liveness check — has the child died since the last write? Called
    /// periodically by the pump so a recording of a mostly-idle window (which
    /// can legitimately go seconds without a frame, CAPTURE-RESEARCH D8)
    /// still notices a dead encoder promptly instead of at stop time.
    fn poll_health(&mut self) -> Result<(), EncodeError>;

    /// Close the input, wait for the encoder to flush, and return the file.
    ///
    /// Consuming `self: Box<Self>` rather than `&mut self` is what makes
    /// "finished" a type-level fact: there is no way to write another frame
    /// to a sink that has been finished.
    fn finish(self: Box<Self>) -> Result<PathBuf, EncodeError>;

    /// Give up: kill the encoder and clean up anything it left that is not
    /// worth keeping. Infallible by design — this is the path taken when
    /// something *else* already failed, and a second error there helps
    /// nobody.
    fn abort(self: Box<Self>);

    /// Where the output is going. Used to name a partial file in an error
    /// message, so a recording that died mid-stream is never silently lost.
    fn output_path(&self) -> &Path;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(width: u32, height: u32, preset: EncodePreset) -> RecordSpec {
        RecordSpec {
            video: VideoSpec::new(width, height),
            audio: None,
            preset,
            path: PathBuf::from("/tmp/out.mkv"),
        }
    }

    fn joined(args: &[String]) -> String {
        args.join(" ")
    }

    // -- presets ------------------------------------------------------

    #[test]
    fn presets_map_to_the_containers_the_research_fixed() {
        assert_eq!(EncodePreset::Hevc.extension(), "mkv");
        assert_eq!(EncodePreset::Hevc.muxer(), "matroska");
        assert_eq!(EncodePreset::Av1.extension(), "mkv");
        assert_eq!(EncodePreset::Av1.muxer(), "matroska");
        assert_eq!(EncodePreset::H264.extension(), "mp4");
        assert_eq!(EncodePreset::H264.muxer(), "mp4");
    }

    #[test]
    fn preset_conversion_round_trips_the_config_vocabulary() {
        for (config, preset) in [
            (VideoPreset::Hevc, EncodePreset::Hevc),
            (VideoPreset::Av1, EncodePreset::Av1),
            (VideoPreset::H264, EncodePreset::H264),
        ] {
            assert_eq!(EncodePreset::from_config(config), preset);
            assert_eq!(preset.as_str(), config.as_str());
        }
    }

    #[test]
    fn only_software_av1_carries_a_frame_rate_cap() {
        assert_eq!(
            VideoEncoder::LibSvtAv1.fps_cap(),
            Some(AV1_SOFTWARE_FPS_CAP)
        );
        assert_eq!(VideoEncoder::HevcVaapi.fps_cap(), None);
        assert_eq!(VideoEncoder::Av1Vaapi.fps_cap(), None);
        assert!(!VideoEncoder::LibSvtAv1.is_vaapi());
        assert!(VideoEncoder::Av1Vaapi.is_vaapi());
    }

    // -- VideoSpec ----------------------------------------------------

    /// The real live case from Stage 10: a window cast negotiated
    /// **2507×1457**, both odd (§3.6's exact scenario).
    #[test]
    fn even_dimensions_rounds_the_live_odd_window_cast_down() {
        let video = VideoSpec::new(2507, 1457);
        assert_eq!(video.even_dimensions(), (2506, 1456));
        // …but the frame on the wire is still the full odd size.
        assert_eq!(video.frame_len(), 2507 * 1457 * 4);
    }

    #[test]
    fn even_dimensions_leaves_an_already_even_frame_alone() {
        assert_eq!(VideoSpec::new(2560, 1600).even_dimensions(), (2560, 1600));
    }

    #[test]
    fn frame_len_saturates_instead_of_overflowing() {
        // Nothing can negotiate this; the point is that a bad value produces
        // a number, not a panic (CLAUDE.md's no-panic rule).
        assert!(VideoSpec::new(u32::MAX, u32::MAX).frame_len() > 0);
    }

    // -- select_encoder -----------------------------------------------

    fn dev(name: &str) -> PathBuf {
        PathBuf::from(format!("/dev/dri/{name}"))
    }

    /// The machine D6's amendment is written for: the iGPU that is always
    /// there plus the dGPU that sometimes is, **neither** with an AV1 encode
    /// entrypoint (§3.3, and Stage 11's live probe of `renderD128`). The
    /// second node is the hypothetical half — only `renderD128` was present
    /// during Stage 11's live run — which is exactly why this is a fake.
    fn jordans_machine(device: &Path, encoder: VideoEncoder) -> bool {
        let known = device == dev("renderD128") || device == dev("renderD129");
        known && encoder != VideoEncoder::Av1Vaapi
    }

    #[test]
    fn hevc_takes_the_first_node_when_no_node_does_av1() {
        let devices = [dev("renderD128"), dev("renderD129")];
        let choice = select_encoder(EncodePreset::Hevc, &devices, jordans_machine).unwrap();
        assert_eq!(
            choice,
            EncoderChoice {
                encoder: VideoEncoder::HevcVaapi,
                vaapi_device: Some(dev("renderD128")),
            }
        );
    }

    #[test]
    fn hevc_prefers_an_av1_capable_node_even_when_it_is_second() {
        let devices = [dev("renderD128"), dev("renderD129")];
        // A hypothetical future dGPU with an AV1 encode entrypoint, enumerated
        // second — the exact case D6's amendment was written for.
        let choice = select_encoder(EncodePreset::Hevc, &devices, |device, _| {
            device == dev("renderD129")
        })
        .unwrap();
        assert_eq!(choice.vaapi_device, Some(dev("renderD129")));
        assert_eq!(choice.encoder, VideoEncoder::HevcVaapi);
    }

    #[test]
    fn hevc_skips_a_node_that_only_does_av1() {
        let devices = [dev("renderD128"), dev("renderD129")];
        let choice = select_encoder(EncodePreset::Hevc, &devices, |device, encoder| {
            if device == dev("renderD128") {
                encoder == VideoEncoder::Av1Vaapi
            } else {
                encoder == VideoEncoder::HevcVaapi
            }
        })
        .unwrap();
        assert_eq!(choice.vaapi_device, Some(dev("renderD129")));
    }

    #[test]
    fn av1_uses_hardware_when_a_node_has_the_entrypoint() {
        let devices = [dev("renderD128")];
        let choice = select_encoder(EncodePreset::Av1, &devices, |_, _| true).unwrap();
        assert_eq!(
            choice,
            EncoderChoice {
                encoder: VideoEncoder::Av1Vaapi,
                vaapi_device: Some(dev("renderD128")),
            }
        );
    }

    #[test]
    fn av1_falls_back_to_software_on_this_machine() {
        let devices = [dev("renderD128"), dev("renderD129")];
        let choice = select_encoder(EncodePreset::Av1, &devices, jordans_machine).unwrap();
        assert_eq!(
            choice,
            EncoderChoice {
                encoder: VideoEncoder::LibSvtAv1,
                vaapi_device: None,
            }
        );
    }

    /// PLAN.md Stage 11 task 3's last sentence, both halves.
    #[test]
    fn with_no_usable_node_av1_still_works_and_hevc_does_not() {
        let none = |_: &Path, _: VideoEncoder| false;
        assert_eq!(
            select_encoder(EncodePreset::Av1, &[dev("renderD128")], none)
                .unwrap()
                .encoder,
            VideoEncoder::LibSvtAv1
        );
        let err = select_encoder(EncodePreset::Hevc, &[dev("renderD128")], none).unwrap_err();
        assert!(matches!(err, EncodeError::NoVaapiDevice { .. }));
        // Actionable: names the software escape hatch and the override knob.
        let message = err.to_string();
        assert!(message.contains("--preset av1"), "{message}");
        assert!(message.contains("vaapi-device"), "{message}");
    }

    #[test]
    fn no_render_nodes_at_all_is_its_own_message() {
        let err = select_encoder(EncodePreset::H264, &[], |_, _| false).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("no /dev/dri/renderD*"), "{message}");
    }

    /// The oracle is expensive (a process spawn each). Passing a node with no
    /// AV1 entrypoint must not cost a second probe for the same pair.
    #[test]
    fn selection_short_circuits_the_expensive_probe() {
        let devices = [dev("renderD128"), dev("renderD129")];
        let mut asked: Vec<(PathBuf, VideoEncoder)> = Vec::new();
        let _ = select_encoder(EncodePreset::Hevc, &devices, |device, encoder| {
            asked.push((device.to_path_buf(), encoder));
            jordans_machine(device, encoder)
        });
        // Pass 1 asks each node about AV1 and stops there (the `&&` never
        // reaches the HEVC probe); pass 2 asks the first node about HEVC.
        assert_eq!(
            asked,
            vec![
                (dev("renderD128"), VideoEncoder::Av1Vaapi),
                (dev("renderD129"), VideoEncoder::Av1Vaapi),
                (dev("renderD128"), VideoEncoder::HevcVaapi),
            ]
        );
    }

    // -- filter chains ------------------------------------------------

    #[test]
    fn the_vaapi_chain_pins_the_matrix_and_range() {
        let chain = filter_chain(VideoEncoder::HevcVaapi, &VideoSpec::new(2560, 1600));
        assert_eq!(
            chain,
            "crop=2560:1600:0:0,hwupload,\
             scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=tv"
        );
    }

    #[test]
    fn the_vaapi_chain_crops_an_odd_frame_to_even() {
        let chain = filter_chain(VideoEncoder::HevcVaapi, &VideoSpec::new(2507, 1457));
        assert!(chain.starts_with("crop=2506:1456:0:0,"), "{chain}");
    }

    #[test]
    fn the_software_av1_chain_caps_the_rate_and_converts_on_the_cpu() {
        let chain = filter_chain(VideoEncoder::LibSvtAv1, &VideoSpec::new(2560, 1600));
        assert_eq!(chain, "crop=2560:1600:0:0,fps=30,format=yuv420p");
        assert!(!chain.contains("hwupload"));
    }

    // -- Stage 12: region-recording crop -------------------------------

    #[test]
    fn a_region_crop_replaces_the_full_frame_crop() {
        let video = VideoSpec::new(2560, 1600).with_crop(crate::capture::PixelRect {
            x: 100,
            y: 200,
            width: 640,
            height: 480,
        });
        let chain = filter_chain(VideoEncoder::HevcVaapi, &video);
        assert!(chain.starts_with("crop=640:480:100:200,"), "{chain}");
    }

    #[test]
    fn a_region_crop_rounds_its_own_size_to_even_but_not_its_offset() {
        let video = VideoSpec::new(2560, 1600).with_crop(crate::capture::PixelRect {
            x: 101,
            y: 201,
            width: 641,
            height: 481,
        });
        let chain = filter_chain(VideoEncoder::HevcVaapi, &video);
        // Size rounds down to even (640x480); the offset is untouched.
        assert!(chain.starts_with("crop=640:480:101:201,"), "{chain}");
    }

    #[test]
    fn with_crop_is_a_no_op_on_frame_len_and_even_dimensions() {
        // The crop only ever changes the `-vf` filter — `-video_size` still
        // describes the full negotiated frame on the wire (the whole monitor
        // is still cast; only the encoder's own view of it narrows).
        let full = VideoSpec::new(2560, 1600);
        let cropped = full.clone().with_crop(crate::capture::PixelRect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        });
        assert_eq!(full.frame_len(), cropped.frame_len());
        assert_eq!(full.even_dimensions(), cropped.even_dimensions());
    }

    // -- the argv -----------------------------------------------------

    fn hevc_choice() -> EncoderChoice {
        EncoderChoice {
            encoder: VideoEncoder::HevcVaapi,
            vaapi_device: Some(dev("renderD128")),
        }
    }

    #[test]
    fn the_video_input_uses_the_raw_size_not_the_even_one() {
        // The bug this guards against shears the video instead of failing:
        // `-video_size` describes the bytes on the pipe, the `crop` filter
        // describes what the encoder sees.
        let args = ffmpeg_args(
            &spec(2507, 1457, EncodePreset::Hevc),
            &hevc_choice(),
            "warning",
        );
        let line = joined(&args);
        assert!(line.contains("-video_size 2507x1457"), "{line}");
        assert!(line.contains("crop=2506:1456:0:0"), "{line}");
    }

    #[test]
    fn a_region_recording_crops_the_full_negotiated_frame() {
        // `-video_size` is still the whole monitor (the cast never narrows);
        // only the `-vf crop` rectangle picks out the selected region.
        let mut recording = spec(2560, 1600, EncodePreset::Hevc);
        recording.video = recording.video.with_crop(crate::capture::PixelRect {
            x: 300,
            y: 400,
            width: 800,
            height: 600,
        });
        let line = joined(&ffmpeg_args(&recording, &hevc_choice(), "warning"));
        assert!(line.contains("-video_size 2560x1600"), "{line}");
        assert!(line.contains("crop=800:600:300:400"), "{line}");
    }

    #[test]
    fn the_timing_model_is_on_every_command_line() {
        let args = ffmpeg_args(
            &spec(2560, 1600, EncodePreset::Hevc),
            &hevc_choice(),
            "warning",
        );
        let line = joined(&args);
        assert!(line.contains("-use_wallclock_as_timestamps 1"), "{line}");
        assert!(line.contains("-fps_mode:v vfr"), "{line}");
        // **Stage 13.** `-framerate` is present, but only ever as the input
        // *time base*, and only ever immediately before the wallclock flag
        // that overrides the timestamps it would otherwise generate — §4.2's
        // 3× fast-forward is what `-framerate` *without* that flag produces.
        assert!(
            line.contains("-framerate 1000 -use_wallclock_as_timestamps 1"),
            "{line}"
        );
        assert_eq!(
            args.iter().filter(|arg| *arg == "-framerate").count(),
            1,
            "the nominal rate belongs to the input and nothing else: {line}"
        );
    }

    /// **Stage 13.** The regression this guards is invisible in the file's
    /// duration and shows up only as jerky motion: with the demuxer's default
    /// 25 fps time base, every PTS snaps to a 40 ms grid and `-fps_mode vfr`
    /// throws away whatever lands in a taken slot (measured live: a 60 fps
    /// source became 24.8 fps).
    #[test]
    fn the_rawvideo_input_declares_a_millisecond_time_base() {
        assert_eq!(RAWVIDEO_TIMEBASE_HZ, "1000");
        let args = ffmpeg_args(
            &spec(2560, 1600, EncodePreset::Hevc),
            &hevc_choice(),
            "warning",
        );
        // …and it applies to the *input*, so it must come before `-i pipe:0`.
        let rate = args.iter().position(|arg| arg == "-framerate").unwrap();
        let input = args.iter().position(|arg| arg == "pipe:0").unwrap();
        assert!(rate < input, "{args:?}");
    }

    #[test]
    fn the_hevc_preset_matches_the_research_table() {
        let args = ffmpeg_args(
            &spec(2560, 1600, EncodePreset::Hevc),
            &hevc_choice(),
            "warning",
        );
        let line = joined(&args);
        assert!(line.contains("-f rawvideo -pixel_format bgr0"), "{line}");
        assert!(line.contains("-i pipe:0"), "{line}");
        assert!(line.contains("-vaapi_device /dev/dri/renderD128"), "{line}");
        assert!(line.contains("-c:v hevc_vaapi -b:v 20M"), "{line}");
        assert!(line.contains("-f matroska /tmp/out.mkv"), "{line}");
        assert!(!line.contains("faststart"), "{line}");
    }

    #[test]
    fn the_h264_preset_is_mp4_with_faststart() {
        let choice = EncoderChoice {
            encoder: VideoEncoder::H264Vaapi,
            vaapi_device: Some(dev("renderD129")),
        };
        let mut recording = spec(1920, 1080, EncodePreset::H264);
        recording.path = PathBuf::from("/tmp/out.mp4");
        let line = joined(&ffmpeg_args(&recording, &choice, "warning"));
        assert!(line.contains("-c:v h264_vaapi -b:v 20M"), "{line}");
        assert!(line.contains("-movflags +faststart"), "{line}");
        assert!(line.contains("-f mp4 /tmp/out.mp4"), "{line}");
    }

    #[test]
    fn the_software_av1_preset_names_no_vaapi_device() {
        let choice = EncoderChoice {
            encoder: VideoEncoder::LibSvtAv1,
            vaapi_device: None,
        };
        let line = joined(&ffmpeg_args(
            &spec(2560, 1600, EncodePreset::Av1),
            &choice,
            "warning",
        ));
        assert!(!line.contains("-vaapi_device"), "{line}");
        assert!(
            line.contains("-c:v libsvtav1 -preset 10 -crf 35 -g 120"),
            "{line}"
        );
        assert!(line.contains("fps=30"), "{line}");
        assert!(line.contains("-f matroska"), "{line}");
    }

    /// The single-source shape (`--audio mic` or `--audio system`): one pulse
    /// input, its calibration offset, the Opus codec, and the `-shortest`
    /// that keeps an infinite input from hanging the process (§4.3).
    #[test]
    fn an_audio_input_lands_in_the_order_ffmpeg_expects() {
        let mut recording = spec(2560, 1600, EncodePreset::Hevc);
        recording.audio = Some(AudioSpec {
            sources: vec!["alsa_output.pci-0000_07_00.6.analog-stereo.monitor".to_string()],
            itsoffset: Some(0.12),
        });
        let args = ffmpeg_args(&recording, &hevc_choice(), "warning");
        let line = joined(&args);
        assert!(
            line.contains("-itsoffset 0.12 -f pulse -i alsa_output"),
            "{line}"
        );
        assert!(line.contains("-c:a libopus -b:a 96k"), "{line}");
        assert!(line.contains("-shortest"), "{line}");

        // Ordering matters: both inputs come before any output option.
        let last_input = args.iter().rposition(|a| a == "-i").unwrap();
        let vf = args.iter().position(|a| a == "-vf").unwrap();
        assert!(last_input < vf, "{line}");

        // One source needs no filtergraph and no explicit mapping at all.
        assert!(!line.contains("-filter_complex"), "{line}");
        assert!(!line.contains("-map"), "{line}");
    }

    /// **Stage 13.** `--audio both`: two pulse inputs, mixed into one track.
    #[test]
    fn two_audio_inputs_are_mixed_into_one_track() {
        let mut recording = spec(2560, 1600, EncodePreset::Hevc);
        recording.audio = Some(AudioSpec {
            sources: vec!["mic.source".to_string(), "sink.monitor".to_string()],
            itsoffset: None,
        });
        let args = ffmpeg_args(&recording, &hevc_choice(), "warning");
        let line = joined(&args);

        assert!(line.contains("-f pulse -i mic.source"), "{line}");
        assert!(line.contains("-f pulse -i sink.monitor"), "{line}");
        assert!(
            line.contains(
                "-filter_complex [1:a][2:a]amix=inputs=2:duration=longest:normalize=0[aout]"
            ),
            "{line}"
        );
        // Automatic stream selection is off once a complex graph exists, so
        // both output streams are named — and the video is still the *input*
        // stream with its own `-vf` chain, not a graph output.
        assert!(line.contains("-map 0:v:0"), "{line}");
        assert!(line.contains("-map [aout]"), "{line}");
        assert!(
            line.contains("-vf crop=2560:1600:0:0,hwupload"),
            "the hardware chain is unchanged by mixing: {line}"
        );
        assert!(line.contains("-c:a libopus"), "{line}");
        assert!(line.contains("-shortest"), "{line}");
    }

    /// A per-input option has to be repeated per input — stated once it would
    /// apply only to the first pulse source and silently desync the other.
    #[test]
    fn the_offset_is_repeated_for_every_audio_input() {
        let mut recording = spec(640, 480, EncodePreset::Hevc);
        recording.audio = Some(AudioSpec {
            sources: vec!["a".to_string(), "b".to_string()],
            itsoffset: Some(-0.05),
        });
        let args = ffmpeg_args(&recording, &hevc_choice(), "warning");
        assert_eq!(
            args.iter().filter(|arg| *arg == "-itsoffset").count(),
            2,
            "{:?}",
            args
        );
        let line = joined(&args);
        assert!(line.contains("-itsoffset -0.05 -f pulse -i a"), "{line}");
        assert!(line.contains("-itsoffset -0.05 -f pulse -i b"), "{line}");
    }

    #[test]
    fn a_single_source_never_grows_a_mix_graph() {
        let one = AudioSpec {
            sources: vec!["only".to_string()],
            itsoffset: None,
        };
        assert_eq!(mix_arguments(&one), None);
        // Nor does an empty one — unreachable through `audio::plan_audio`
        // (which returns no spec at all rather than an empty source list),
        // but the argv builder must not produce `amix=inputs=0` if it ever is.
        let none = AudioSpec {
            sources: Vec::new(),
            itsoffset: None,
        };
        assert_eq!(mix_arguments(&none), None);
    }

    #[test]
    fn no_audio_means_no_shortest() {
        let line = joined(&ffmpeg_args(
            &spec(2560, 1600, EncodePreset::Hevc),
            &hevc_choice(),
            "warning",
        ));
        assert!(!line.contains("-shortest"), "{line}");
        assert!(!line.contains("pulse"), "{line}");
    }

    #[test]
    fn the_loglevel_is_threaded_through() {
        let line = joined(&ffmpeg_args(
            &spec(640, 480, EncodePreset::Hevc),
            &hevc_choice(),
            "verbose",
        ));
        assert!(line.contains("-loglevel verbose"), "{line}");
    }

    // -- errors -------------------------------------------------------

    #[test]
    fn a_missing_ffmpeg_names_the_install_command() {
        let message = EncodeError::FfmpegMissing.to_string();
        assert!(message.contains("sudo pacman -S ffmpeg"), "{message}");
    }

    /// Live-caught in Stage 11: a SIGKILLed ffmpeg has no last words worth
    /// quoting, and the line it happened to be printing (`Starting
    /// thread...`, at `verbose`) read as though it were the cause.
    #[test]
    fn a_signal_death_does_not_quote_whatever_line_was_last() {
        let err = EncodeError::Died {
            code: None,
            tail: vec!["[out#0/matroska @ 0x1] Starting thread...".to_string()],
        };
        let message = err.to_string();
        assert!(!message.contains("Starting thread"), "{message}");
        assert!(message.contains("killed by a signal"), "{message}");
        // …and it says what that means for the file on disk.
        assert!(message.contains("never finalized"), "{message}");
    }

    #[test]
    fn a_dead_encoder_reports_its_last_stderr_line() {
        let err = EncodeError::Died {
            code: Some(1),
            tail: vec![
                "[out#0] Error muxing a packet".to_string(),
                "av_interleaved_write_frame(): No space left on device".to_string(),
            ],
        };
        let message = err.to_string();
        assert!(message.contains("No space left on device"), "{message}");
    }
}
