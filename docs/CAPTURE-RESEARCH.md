# Capture research: proving every capture path before building on it

Stage 2 output. Every claim below is either pasted command output from this machine, a
source-file citation against the exact commit the running niri binary was built from, or
a transcript from a throwaway probe written for this stage. Nothing here is inference
dressed up as fact. Where evidence is thin it says so, and says what would firm it up.

> **Renumbering note (2026-08-08, editorial — the evidence is untouched):**
> after this document was written, PLAN.md gained a new Stage 4 (config
> KDL → TOML migration) and its original Stages 4–18 became 5–19. Every
> "Stage N" reference below with N ≥ 4 therefore maps to PLAN.md stage
> N + 1 (e.g. §8's "Stage 9" decisions bind the current Stage 10).

**Probe environment**, 2026-08-08, `nt-14589`:

| thing | value |
| --- | --- |
| compositor | `niri 26.04 (8ed0da4)`, running as `niri --session` (PID 1314) |
| niri source | shallow clone of `v26.04`; `git log -1 --format=%H` → `8ed0da44d974c32c6877d2f4630c314da0717ecb`, whose short form `8ed0da4` is **byte-identical to the build id the running binary reports** — the clone is confirmed to be the source of the running binary |
| output | eDP-1, mode 2560x1600@60, **scale 1.5**, logical 1706x1066, transform normal |
| GPU | AMD Radeon 680M (RDNA2, `radeonsi`), Mesa 26.1.6, libva 1.24 |
| PipeWire | 1.6.8 (client and server) |
| ffmpeg | **n9.0** (Arch `2:9.0-5`) — installed since PLAN.md was written; Lavc63.1.100 |
| grim | 1.5.0 |

Probes were **built and run in a scratch directory, never in this crate** — no probe code
is compiled by `cargo build` here. Their sources and every raw transcript are archived
read-only in `docs/research/2026-08-08-stage2/` so the citations below resolve:

| probe | what it is |
| --- | --- |
| `pwprobe.c` | minimal libpipewire video consumer, C (see §2.0 for why C and not Rust) |
| `cast.sh`, `cast-window.sh` | Mutter.ScreenCast session drivers with unconditional teardown |
| `sccopy-main.rs` | `zwlr_screencopy_v1` handshake logger (Rust, wayland-client 0.31) |
| `ftl.rs` | `ext_foreign_toplevel_list_v1` listing logger |
| `inject.rs` | virtual pointer/keyboard injector — **nested compositor only** |
| `lstest-main.rs` | iced 0.14 + iced_layershell 0.19.1 overlay viability app |
| `*.txt` | raw command output, one file per probe |
| `overlay-*.png` | screenshots of the overlay rendering inside the nested niri |

The pre-plan probes in `docs/research/2026-08-08-probes/` remain valid; this document
folds their findings in and marks them **[pre-plan]** where they are not re-derived here.

---

## 1. Screencopy (`zwlr_screencopy_v1`) — stills

`zwlr_screencopy_manager_v1` is advertised at **version 3**. Full real-session global
list: `docs/research/2026-08-08-stage2/screencopy-06-globals-and-timing.txt`.

### 1.1 Formats offered per output

Live handshake, `capture_output` on eDP-1:

```
   [event] buffer format=1 2560x1600 stride=10240
   [event] linux_dmabuf fourcc=0x34325258 2560x1600
   [event] buffer_done
```

**Exactly one shm format and one dmabuf format are ever offered:**

- `wl_shm` `buffer(format=1)` = `Xrgb8888`, width×4 stride, no padding
- `linux_dmabuf(fourcc=0x34325258)` = `'XR24'` = `DRM_FORMAT_XRGB8888`

This is not a per-output negotiation — it is hardcoded. `niri-src/src/protocols/screencopy.rs:420-435`:

```rust
// Send desired SHM buffer parameters.
frame.buffer(
    Format::Xrgb8888,
    buffer_size.w as u32,
    buffer_size.h as u32,
    buffer_size.w as u32 * 4,
);

if frame.version() >= 3 {
    // Send desired DMA buffer parameters.
    frame.linux_dmabuf(
        Fourcc::Xrgb8888 as u32,
        buffer_size.w as u32,
        buffer_size.h as u32,
    );
    frame.buffer_done();
}
```

and the buffer niri will *accept* is validated to the same single format on both paths
(`screencopy.rs:566` for dmabuf, `:578-583` for shm) — anything else is an
`InvalidBuffer` protocol error. Note the compositor's global `wl_shm` advertises four
formats (`AB24`, `XB24`, `Xrgb8888`, `Argb8888`); **screencopy uses none of that
generality.** There is no alpha channel available and no request for one.

Byte order: `wl_shm` `Xrgb8888` on little-endian is **B, G, R, X in memory**. Verified
against pixel values — see §1.4.

### 1.2 y-invert

**niri never sets the `YInvert` flag.** Both call sites pass a literal `false`
(`niri-src/src/niri.rs:5312` and `:5374`):

```rust
queue.pop().submit_after_sync(false, sync, &self.event_loop);
...
let res = res.map(|sync| screencopy.submit_after_sync(false, sync, &self.event_loop));
```

and that value is the only thing that can set the flag (`screencopy.rs:703-710`).

Confirmed live twice, including the adversarial case: the nested niri's winit output
reports `Transform: flipped vertically` (transform 6), and screencopy on it *still*
reports `flags = 0` (`docs/research/2026-08-08-stage2/overlay-02-nested-screencopy-and-globals.txt`).

**Stage 4 must still read the flag** (the protocol permits it and a future niri could
change), but on this compositor row 0 is the top row and the untested branch is the
inverted one. Handle it, don't rely on exercising it.

### 1.3 Cursor compositing

`overlay_cursor` is a plain per-request boolean, honored by passing it as the
include-pointer flag to the render pass (`niri.rs:5277`, `:5355`). There is no
"cursor metadata" mode here — that is the ScreenCast interface (§2), not screencopy.

Verified by differencing three back-to-back full-output captures (nocursor, cursor,
nocursor) and keeping only pixels that were stable across both nocursor shots
(`docs/research/2026-08-08-stage2/screencopy-04/05`):

```
  cursor-attributable pixels: 500
  top 8 densest 64x64 cells:
    cell (20, 14) -> 455 px  (physical origin 1280,896)
  densest blob: 455 px, bbox x 1314..1334 y 912..946 (21x35)
  sample pixel there: nocursor BGR=(0, 10, 12)  cursor BGR=(6, 15, 17)
```

A 21×35 physical-pixel blob is a standard arrow cursor at scale 1.5. The residual ~45
pixels were scattered across the panel/terminal and are live-desktop churn, not cursor.
**The cursor is composited at physical resolution into the frame.**

### 1.4 Per-output vs region semantics

`capture_output_region(x=100, y=100, w=400, h=300)` on eDP-1 (scale 1.5):

```
   [event] buffer format=1 600x450 stride=2400
   [event] linux_dmabuf fourcc=0x34325258 600x450
```

**Region arguments are in logical coordinates; the delivered buffer is physical
pixels.** 400×1.5 = 600, 300×1.5 = 450. Source: `screencopy.rs:375-385` converts with
`rect.to_physical_precise_round(output_scale)` after clamping to the output, then
un-transforms.

Verified byte-exactly against the full-output capture:

```
  region buffer is 600x450 (= logical 400x300 * scale 1.5)
  pixels differing from full-output at physical offset (150,150): 0 / 270000
```

Zero differing pixels. The region buffer is exactly `full[150.., 150..]`, i.e. the
region's physical origin is `round(logical_origin * scale)`.

> **Discrepancy with the pre-plan probe, resolved.** The pre-plan note recorded grim
> `-g "100,100 400x300"` producing **599x449**, whereas the protocol produces
> **600x450**. grim captures the whole output and crops in its own code with its own
> rounding; the protocol path is the authority. Stage 4 should use
> `capture_output_region` (or crop the full frame itself), and must not treat grim's
> dimensions as the reference when comparing.

Cross-check against grim on the same screen content (`docs/research/2026-08-08-stage2/screencopy-01`,
`pipewire-05`):

| point (physical) | grim PNG (RGB) | screencopy shm (BGRX bytes) | ScreenCast dmabuf (BGRX) |
| --- | --- | --- | --- |
| (0,0) | 190, 221, 242 | `243 222 191 255` → RGB 191,222,243 | `f3 de bf ff` → RGB 191,222,243 |
| (1280,800) | 12, 10, 0 | `0 10 12 255` → RGB 12,10,0 | `00 0a 0c ff` → RGB 12,10,0 |
| (0,1599) | 25, 25, 29 | `29 25 25 255` → RGB 25,25,29 | — |

Two of three points match grim exactly and all three match screencopy↔ScreenCast
exactly; (0,0) differs from grim by ±1 on every channel, consistent with grim's PNG
encode path, not with a capture difference. **Row 0 is the top row on every path.**

### 1.5 Self-capture and timing

**[pre-plan, and load-bearing]** A grim taken while niri's own screenshot UI was mapped
captured the UI. Layer-shell surfaces are composited into screencopy. **Freeze-before-map
is mandatory**, exactly as the Architecture's region flow says.

Full-output shm capture, debug build, three runs, including process start, connection,
two roundtrips and a 16 MB buffer poison-fill: **0.278–0.281 s wall**. grim's release
build measured 0.398 s on the same output **[pre-plan]**. A ~0.3 s freeze before mapping
the overlay is the budget Stage 6 is working with.

No `damage` events arrive on a plain (non-`copy_with_damage`) capture — the event list
was empty in every run.

---

## 2. Mutter.ScreenCast v4 — video

The handshake, session interface, cursor-mode enum, absence of `RecordArea`, the
dmabuf-only `EnumFormat` pod, `niri msg casts` and the cast events are all
**[pre-plan]** and unchanged; see `docs/research/2026-08-08-probes/screencast-probes/SUMMARY.txt`.
This section answers the one question that was left open, and adds four new findings.

### 2.0 Why the probe is C, not pipewire-rs — and a Stage 9 blocker

**`pipewire` 0.10 cannot currently be built on this machine.** A scratch crate with
`pipewire = "0.10"` fails in `pipewire-sys`'s build script
(`docs/research/2026-08-08-stage2/pipewire-00-libclang-blocker.txt`):

```
thread 'main' panicked at bindgen-0.72.1/lib.rs:616:27:
Unable to find libclang: "couldn't find any valid shared libraries matching:
['libclang.so', 'libclang-*.so', 'libclang.so.*', 'libclang-*.so.*'], set the
`LIBCLANG_PATH` environment variable..."
```

`find / -name 'libclang.so*'` returns nothing; `pacman -Q clang llvm` says neither is
installed. The PipeWire *dev headers* are present (`pkg-config --modversion
libpipewire-0.3 libspa-0.2` → `1.6.8` / `0.2`), so only the bindgen toolchain is missing.

This is a **Stage 9 prerequisite**, and per the sudo rule no agent installs it. The exact
command for Jordan:

```sh
sudo pacman -S clang
```

It is also a **packaging fact for Stage 16**: `clang` (or `llvm-libs`+`clang`) belongs in
the PKGBUILD `makedepends`, and in CI's `APT_BUILD_DEPS` (`libclang-dev`).

The probe was therefore written in C against `libpipewire-0.3` directly (gcc 16.1.1 is
installed, headers are present). This is arguably *better* evidence than a Rust probe:
it is the same library, one layer closer to the wire, with no binding indirection.

### 2.1 Is shm negotiable? **No — refused at both layers.**

**Layer 1, format negotiation.** Offering `video/raw` `BGRx` with **no `modifier`
property at all** (the shape a shm-only consumer sends) and a `Buffers` `dataType` of
`MemFd|MemPtr` (`docs/research/2026-08-08-stage2/pipewire-01-shm-negotiation.txt`):

```
[state] unconnected -> connecting
== pw_stream_connect -> 0
[state] connecting -> paused
[param] id=4 -> NULL (cleared)
[state] paused -> error : no more input formats
== RESULT: got_format=0 frames_received=0
```

No format is ever produced. `no more input formats` is the negotiation dying.

**Layer 2, buffer allocation.** The sharper test: advertise the `modifier` property (so
the *format* negotiates fine) but then ask for shm buffers only
(`docs/research/2026-08-08-stage2/pipewire-04-modifier-but-shm-buffers.txt`):

```
[format] parsed: format=BGRx size=2560x1600 framerate=0/1 modifier=0x0
[buffers] replying with SPA_PARAM_Buffers dataType mask 0x6 (MemFd=1 MemPtr=1 DmaBuf=0)
[state] paused -> error : error alloc buffers: Invalid argument
== RESULT: got_format=1 frames_received=0
```

The format negotiates, then buffer allocation fails. **There is no path to shm.** The
pre-plan caveat ("whether niri renegotiates to shm for a modifier-less client is
untested") is now closed: it does not.

### 2.2 dmabuf with LINEAR works, and is mmap-able

Offering `BGRx` with a `MANDATORY|DONT_FIXATE` modifier enum containing only
`DRM_FORMAT_MOD_LINEAR` (0x0), and `dataType = DmaBuf`
(`docs/research/2026-08-08-stage2/pipewire-02`, `pipewire-03`):

```
[format] NEGOTIATED SPA_PARAM_Format:
     video/raw
               format : (Id) BGRx
             modifier : (Long) 0
                 size : (Rectangle) 2560x1600
            framerate : (Fraction) 0/1
         maxFramerate : (Fraction) 60000/1000
[state] paused -> streaming
[frame 0] n_datas=1
   data[0] type=3 (DmaBuf) fd=20 mapoffset=0 maxsize=1 data=(nil) chunk{offset=0 size=1 stride=10240}
   dmabuf fd size (lseek END) = 16384000, want 16384000, mmap -> OK
   DMA_BUF_IOCTL_SYNC(START|READ) -> 0 (ok)
   px[0..3] (B G R X): [f3 de bf ff] [f5 df c0 ff] [f7 df c1 ff] [f8 df c1 ff]
   px at row 800 col 1280: [00 0a 0c ff]
   non-black sample points: 4000 / 4000
== RESULT: got_format=1 frames_received=10
```

Ten frames in well under a second, the LINEAR dmabuf `mmap(PROT_READ, MAP_SHARED)`s
straight from the fd, `DMA_BUF_IOCTL_SYNC` succeeds, and the pixels match grim exactly at
(1280,800) (§1.4). **No GBM, no EGL, no GPU import is needed for LINEAR.** This is the
conservative path and it is real.

### 2.3 Four gotchas Stage 9 must not learn the hard way

1. **`PW_STREAM_FLAG_MAP_BUFFERS` does not map dmabufs.** `data = (nil)` in every frame
   above, despite the flag being set. The consumer must `mmap` the fd itself.
2. **`maxsize` and `chunk->size` are dummies on the dmabuf path** — both reported `1`.
   Do not size anything from them.
3. **`chunk->stride` is the authority, and it is not `width * 4`.** On the window cast
   (§5.3) the negotiated size was 2507×1457 but `chunk{stride=10240}` — 10240/4 = 2560,
   the *output* width, because the buffer is allocated at output pitch. Deriving the
   stride from the negotiated width would shear every frame. The fd's own size
   (14 921 728) is also not `stride × height` (14 919 680) — there is allocator padding
   beyond the last row. Read rows as `base + y * chunk->stride`, `width * 4` bytes each.
4. **`framerate` negotiates to `0/1` (variable) with `maxFramerate 60000/1000`.** Frames
   are damage-driven, not clocked. The window cast in §5.3 delivered **1 frame in 6
   seconds** from an idle terminal. This is the single biggest input to the A/V timing
   decision (§4).

### 2.4 Buffer-path decision and fallback chain

Verdict: **dmabuf-only. There is no shm fallback to write.**

```
1. dmabuf, modifier LINEAR (0x0), mmap the fd + DMA_BUF_IOCTL_SYNC     PRIMARY — proven
2. dmabuf, tiled AMD modifier, GBM/EGL import + GPU download           NOT IMPLEMENTED in v0.1
3. shm                                                                  IMPOSSIBLE — refused (§2.1)
```

Step 1 works because we *request* LINEAR specifically and niri honors it (`LINEAR` is one
of the 8 modifiers in its `EnumFormat` offer **[pre-plan]**). Step 2 exists only as a
note: if a future compositor or GPU refuses LINEAR, the stream will fail to negotiate
rather than silently degrade, which is the correct failure — a clean `Error` signal, not
a corrupt recording. **Stage 9 must treat "negotiation produced no format" as a
first-class, user-visible error path**, since that is the shape every unsupported case
takes.

---

## 3. ffmpeg encode chain

`ffmpeg -encoders` (`docs/research/2026-08-08-stage2/ffmpeg-01-encoders.txt`) shows `hevc_vaapi`, `h264_vaapi`,
`av1_vaapi`, `libsvtav1`, `libx264`, `libopus`. `/dev/dri/renderD128` exists and is
`crw-rw-rw-` (world-accessible; no group juggling needed).

### 3.1 `hevc_vaapi` is real hardware and comfortably realtime

The exact working command line, generated rawvideo on stdin at the real output size
(`docs/research/2026-08-08-stage2/ffmpeg-02-hevc-vaapi.txt`):

```sh
cat raw-2560x1600-bgr0.raw | ffmpeg -f rawvideo -pixel_format bgr0 -video_size 2560x1600 \
  -framerate 60 -i pipe:0 -vaapi_device /dev/dri/renderD128 \
  -vf "format=nv12,hwupload" -c:v hevc_vaapi -b:v 20M -f matroska out-hevc.mkv
```

```
frame=  120 fps=0.0 q=-0.0 Lsize=    5687KiB time=00:00:02.00 bitrate=23295.7kbits/s speed=2.11x
```

Hardware proof, from `-v verbose` (`docs/research/2026-08-08-stage2/ffmpeg-03`):

```
[VAAPI @ ...] Initialised VAAPI connection: version 1.24
[VAAPI @ ...] VAAPI driver: Mesa Gallium driver 26.1.6-arch1.1 for AMD Radeon 680M (radeonsi, rembrandt, ACO, ...)
[hevc_vaapi @ ...] Using VAAPI profile VAProfileHEVCMain (17).
[hevc_vaapi @ ...] Using VAAPI entrypoint VAEntrypointEncSlice (6).
[hevc_vaapi @ ...] Using VAAPI render target format YUV420 (0x1).
[hevc_vaapi @ ...] RC mode: VBR.
```

`VAEntrypointEncSlice` is the encode entrypoint. The output decodes clean
(`ffmpeg -i out-hevc.mkv -f null -` → no errors).

### 3.2 Do the BGRx→NV12 conversion on the GPU — 10× less CPU, *with the matrix pinned*

Two ways to get from `bgr0` to the NV12 VAAPI surface the encoder wants. Measured over
120 frames of 2560×1600 (2.00 s of video), same machine, page-cached input, 16 cores
(`docs/research/2026-08-08-stage2/ffmpeg-09-cpu-cost.txt`):

| filter chain | user CPU | wall | speed |
| --- | --- | --- | --- |
| `format=nv12,hwupload` (swscale on CPU) | **3.50 s** | 0.99 s | 2.11× |
| `hwupload,scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=tv` | **0.34 s** | 0.77 s | 2.84× |

3.50 s of CPU per 2 s of video is ~175 % of a core burned on colour conversion alone,
continuously, for the entire recording. The GPU path costs 0.34 s — a **10× reduction**.

But the GPU path is only correct **if the matrix and range are stated explicitly**. A
four-colour test card round-tripped at `-qp 1` (`docs/research/2026-08-08-stage2/ffmpeg-07`, `ffmpeg-08`):

| chain | red | green | blue | white |
| --- | --- | --- | --- | --- |
| expected | 255,0,0 | 0,255,0 | 0,0,255 | 255,255,255 |
| `format=nv12,hwupload` | 252,0,0 | 0,252,0 | 0,0,254 | 255,255,255 |
| `hwupload,scale_vaapi=format=nv12` | **232,0,2** | **20,255,8** | **1,0,243** | 255,255,255 |
| `hwupload,scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=tv` | 251,0,0 | 0,252,0 | 0,0,254 | 255,255,255 |

Bare `scale_vaapi` is visibly wrong (red off by 23). With `out_color_matrix=bt709
out_range=tv` it matches the CPU path to within YUV420 rounding. **The tagging is not
optional.**

Byte order sanity check, since this is the seam with PipeWire's `SPA_VIDEO_FORMAT_BGRx`:
a pure-red frame written as `pix_fmt bgr0` is `00 00 fd ff` — B, G, R, X. `bgr0` is the
right ffmpeg pix_fmt for `BGRx`, no swizzle needed on our side.

### 3.3 AV1 hardware encode does not exist on the 680M

`vainfo` lists exactly one AV1 line, and it is decode-only:

```
      VAProfileAV1Profile0            :	VAEntrypointVLD
```

`av1_vaapi` therefore exists in the ffmpeg build but fails at open
(`docs/research/2026-08-08-stage2/ffmpeg-04`):

```
[av1_vaapi @ ...] Using VAAPI profile VAProfileAV1Profile0 (32).
[av1_vaapi @ ...] No usable encoding entrypoint found for profile VAProfileAV1Profile0 (32).
[vost#0:0/av1_vaapi @ ...] Error while opening encoder
Conversion failed!
```

The AV1 preset is **software `libsvtav1`**, as PLAN.md assumed.

### 3.4 `libsvtav1` is not realtime at 2560×1600@60

Measured on `testsrc2` (a deliberately hard synthetic source — real desktop content is
much easier; treat these as a floor, not a typical case)
(`docs/research/2026-08-08-stage2/ffmpeg-05`, `ffmpeg-06`):

| preset | fps at 2560×1600 | realtime factor @60 fps | size (1 s) |
| --- | --- | --- | --- |
| 8 | 35 | 0.58× | 2.61 MB |
| 10 | 49 | 0.80× | 2.78 MB |
| 12 | 51 | 0.84× | 2.81 MB |
| 10, fed at **30 fps** | 42 | **1.26–1.39×** | — |

**At 60 fps the AV1 preset cannot keep up on this machine at full resolution; at 30 fps
it can.** Stage 10 should either cap the AV1 preset at 30 fps, or accept sustained frame
drops and say so in the UI. Do not silently ship a preset that drops a third of frames.

### 3.5 H.264

| preset | speed at 2560×1600 |
| --- | --- |
| `h264_vaapi` (hardware) → MP4 | 1.27–1.34× realtime |
| `libx264 -preset ultrafast` | 2.07× (1 s clip) |
| `libx264 -preset veryfast` | 1.44× (1 s clip) |

`h264_vaapi` is the right H.264 preset: realtime, near-zero CPU, and it produces a
correct MP4 (`Video: h264 (High) (avc1), yuv420p, 2560x1600, 60 fps`). `libx264` is a
fallback only if VAAPI is unavailable.

### 3.6 Odd dimensions: `hevc_vaapi` silently changes the resolution

Window casts negotiate odd sizes (§5.3 gave 2507×1457, both odd). Feeding that directly
to `hevc_vaapi` (`docs/research/2026-08-08-stage2/ffmpeg-10-odd-dimensions.txt`):

```
--- (A) straight through, no fixup:
  Stream #0:0: Video: hevc (Main), nv12(tv, bt709/...), 2507x1457, ...
ffprobe of the result:   hevc,2508,1458      <-- NOT what was asked for
--- (B) with crop=2506:1456:0:0 first:
ffprobe of the result:   hevc,2506,1456      <-- exactly as asked
--- (C) libsvtav1, same odd input:
ffprobe of the result:   av1,2507,1457       <-- odd sizes preserved
```

`hevc_vaapi` accepts the odd size without warning and emits a **2508×1458** stream,
padding the right column and bottom row with whatever the encoder felt like. `libsvtav1`
keeps the exact size. **Stage 10/11 must crop to even dimensions itself** (`crop=W&~1:H&~1`
or an equivalent crop applied to the frame before it reaches the encoder) rather than let
the encoder decide — otherwise a window recording is silently 1 px larger than the window,
with a garbage edge.

### 3.7 The preset table Stage 10 reuses verbatim

Common video input (values from the negotiated SPA format; `W`/`H` **rounded down to
even**):

```
-f rawvideo -pixel_format bgr0 -video_size {W}x{H} -framerate {fps} \
-use_wallclock_as_timestamps 1 -i pipe:0 -fps_mode vfr
```

| preset | container | encoder args |
| --- | --- | --- |
| `hevc` (primary) | MKV | `-vaapi_device /dev/dri/renderD128 -vf "hwupload,scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=tv" -c:v hevc_vaapi -b:v 20M -f matroska` |
| `h264` | MP4 | `-vaapi_device /dev/dri/renderD128 -vf "hwupload,scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=tv" -c:v h264_vaapi -b:v 20M -movflags +faststart -f mp4` |
| `av1` | MKV | `-vf "format=yuv420p" -c:v libsvtav1 -preset 10 -crf 35 -g 120 -f matroska` (cap at 30 fps — §3.4) |

Audio, when enabled: `-f pulse -i {source}` … `-c:a libopus -b:a 96k -shortest` (§4).

---

## 4. Audio transport

### 4.1 What is available

`pactl list short sources` — the PipeWire pulse shim, server string
`/run/user/1000/pulse/native` (`docs/research/2026-08-08-stage2/audio-01-pulse-shim.txt`):

```
56	alsa_output.pci-0000_07_00.6.analog-stereo.monitor	PipeWire	s32le 2ch 48000Hz
57	alsa_input.pci-0000_07_00.6.analog-stereo	        PipeWire	s32le 2ch 48000Hz
```

Exactly one system-audio monitor and one mic. ffmpeg has the `pulse` demuxer (`D  pulse`)
and both capture correctly to Opus/MKV: the mic produced 36 KiB of real audio in 2 s, the
monitor produced silence-sized output (nothing was playing). Opening a source moves it
`SUSPENDED → RUNNING` (`docs/research/2026-08-08-stage2/av-04`).

### 4.2 The PTS model matters more than the transport

Before comparing transports: **the naive rawvideo-on-stdin setup produces a video at the
wrong speed**, and this is not a subtle effect. A producer emitting 60 frames over 3.00 s
of real time, into an ffmpeg told `-framerate 60` (`docs/research/2026-08-08-stage2/av-01-pts-model.txt`):

| input handling | output duration (truth: 3.00 s) | frames |
| --- | --- | --- |
| `-framerate 60` (naive) | **1.000 s** — 3× fast-forward | 60 |
| `-use_wallclock_as_timestamps 1 -fps_mode vfr` | 2.900 s | 60 |
| `-use_wallclock_as_timestamps 1 -fps_mode cfr -r 60` | 2.934 s | 176 (duplicated) |

Since the cast negotiates `framerate 0/1` (variable, §2.3) and window casts can emit one
frame in six seconds, the naive path is not a corner case — it is the normal case.
**`-use_wallclock_as_timestamps 1` with `-fps_mode vfr` is mandatory on the video input.**

### 4.3 The hazard that decides the transport

With `-f pulse`, ffmpeg owns the audio clock and starts capturing the instant it is
spawned. If the video's first frame arrives later, does the offset survive?

Test: ffmpeg spawned, video producer sleeps 1.0 s, then feeds 90 frames at 30 fps
(`docs/research/2026-08-08-stage2/av-03-late-video-start.txt`):

| handling | video first pts | audio first pts | true offset preserved? |
| --- | --- | --- | --- |
| default normalisation | 0.000 | −0.007 | **no** |
| `-copyts -start_at_zero` | 0.000 | −0.007 | **no** |

**Both discard the 1 s offset silently.** ffmpeg normalises each input to its own first
packet. Whatever real gap exists between "ffmpeg opened the audio device" and "the first
video frame arrived" becomes A/V desync of exactly that size, with no warning. `-copyts`
does not rescue it.

Two further behaviours worth having in writing:

- **`-f pulse` is an infinite input.** The first attempt at a combined command **hung
  until killed** — ffmpeg does not exit when stdin EOFs, because the audio input never
  ends. `-shortest` plus an explicit stop signal from the recorder are both required.
- **`-itsoffset` survives normalisation** and is therefore the calibration knob. Asking
  for `-itsoffset 0.5` on the pulse input produced `a first=0.494` against `v first=0.000`
  (`docs/research/2026-08-08-stage2/av-04`).

PCM on `pipe:3` was also exercised end to end (`-f s16le -ar 48000 -ac 2 -i pipe:3`, fd
supplied via `3< pcm.raw`) and works — video and audio muxed, ends within ~30 ms of each
other (`docs/research/2026-08-08-stage2/av-02`). It gives us ownership of both clocks, which is the only way to
*guarantee* the offset. Its cost: we must write and own a second PipeWire capture stream
— and pipewire-rs currently does not build here at all (§2.0).

### 4.4 Decision

**Stage 12 implements `-f pulse`, with three specific mitigations, and keeps `pipe:3` as
the documented escalation.**

The reasoning turns on a structural fact: **ffmpeg cannot be spawned before the video
format is negotiated anyway**, because `-video_size` must match the negotiated size. So
the recorder's natural sequence is already `negotiate → first frame known → spawn ffmpeg`,
which collapses the §4.3 gap from "however long negotiation takes" to "however long
ffmpeg's own startup and the pulse device resume take". That residual is small, constant,
and — critically — *measurable and correctable* with `-itsoffset`.

Mitigations, all mandatory:

1. Spawn ffmpeg only after the first video frame has been received, and start writing
   frames to its stdin immediately, so neither input idles at the start.
2. `-shortest` on the output plus a graceful stop (write `q` to ffmpeg's stdin, or
   SIGINT, then wait) — never rely on stdin EOF to terminate the process.
3. Stage 12's clap test measures the residual offset; the measured constant goes into the
   preset as `-itsoffset` on the audio input, and the measurement is recorded here.

Escalate to `pipe:3` only if the clap test shows the residual is **not constant** across
runs (a varying offset cannot be corrected by a fixed `-itsoffset`, and only owning both
clocks fixes it). That escalation also unblocks per-application audio capture later,
which the pulse shim cannot express.

Device selection is by pulse source name from `pactl list short sources` — `mic` →
`alsa_input.*`, `system` → `*.monitor`, `both` → two `-f pulse` inputs plus `amix`.
Resolve names at record time, never hardcode: they are hardware-path-derived.

---

## 5. Window capture mechanism

### 5.1 The id space is unified — one number identifies a window everywhere

`ext_foreign_toplevel_handle_v1.identifier` is **the niri window id, stringified**
(`docs/research/2026-08-08-stage2/window-05-foreign-toplevel-list.txt`):

```
  [list] toplevel -> new handle id ext_foreign_toplevel_handle_v1@4278190080
     [handle ...] identifier = "19"
     [handle ...] title      = "jordan@nt-14589:~"
     [handle ...] app_id     = "Alacritty"
     ...
=== niri msg -j windows at the same moment ===
  niri id 19 | app_id Alacritty | title jordan@nt-14589:~
  niri id 16 | app_id Alacritty | title ⠂ System update completed
  niri id 18 | app_id microsoft-edge | title JorDunn/saola-capture...
```

Source: it is one `MappedId(u64)` (`niri-src/src/window/mapped.rs:209`) exposed three
ways — `niri-ipc` `Window.id`, `to_protocol_identifier()` → `format!("{}", self.0)` for
foreign-toplevel (`mapped.rs:234-236`), and `window-id: u64` on ScreenCast RecordWindow
(`src/dbus/mutter_screen_cast.rs:54-55` → `CastTarget::Window { id }`).

Confirmed live end to end: `RecordWindow` with `window-id = 19` (a niri-ipc id) produced
`niri msg casts` → `Target: window 19`. **The pre-plan open question is closed: the id
spaces match.**

### 5.2 `ext_foreign_toplevel_list_v1` gives identity only

Full event set per handle, verbatim: `identifier`, `title`, `app_id`, `done`, `closed`.
No geometry, no output, no state. The protocol's own doc comment says so
("intentionally minimalistic and expects additional functionality... to be implemented in
extension protocols"). **niri-ipc is strictly richer; there is no reason for this app to
use foreign-toplevel at all.**

Two Rust-side facts Stage 7 needs and would otherwise lose an hour to:

- the module is behind wayland-protocols' **`staging`** feature, which is not in the
  default set — `features = ["client", "staging"]`;
- the dispatcher **panics at runtime** without an `event_created_child!` specialization,
  placed *inside* the `impl Dispatch` block: `Missing event_created_child specialization
  for event opcode 0 of ext_foreign_toplevel_list_v1`.

### 5.3 The three candidates, judged

**(a) `niri-ipc` `Action::ScreenshotWindow`** — richer than the plan assumed. Its fields
(`niri-ipc/src/lib.rs:257-284`) are `id: Option<u64>`, `write_to_disk: bool`,
`show_pointer: bool`, `path: Option<String>` (absolute). So we can name our own output
path.

Crucially, `Niri::screenshot_window` (`niri.rs:5575-5640`) **renders the window's own
render elements to an offscreen buffer** — it is not a crop of the output:

```rust
mapped.render(ctx, mapped.window.geometry().loc.to_f64(), scale, alpha, ..., &mut |elem| elements.push(elem.into()));
let geo = encompassing_geo(scale, elements.iter().skip(pointer_count));
let pixels = render_to_vec(renderer, geo.size, scale, Transform::Normal, Fourcc::Abgr8888, elements)?;
```

Consequences: **occlusion is irrelevant** (overlapping floating windows cannot contaminate
it), decorations/shadows are included via `encompassing_geo`, the pixel format is
`Abgr8888` (RGBA byte order), and it is scale-correct by construction.

Two costs. It is **PNG-on-disk or nothing** — `save_screenshot` PNG-encodes on a niri
thread and either writes the file or not. And **the clipboard is clobbered
unconditionally**: `tx.send(buf)` runs regardless of `write_to_disk`
(`niri.rs:5665-5706`), setting a `image/png` selection every time. That is a real side
effect on a `--no-copy` capture.

**(b) Geometry-crop from an output screencopy — not viable.** `niri msg -j windows`
returns no absolute position for tiled windows:

```json
"layout": { "pos_in_scrolling_layout": [1,1], "tile_size": [1675.33,975.33],
            "window_size": [1671,971], "tile_pos_in_workspace_view": null,
            "window_offset_in_tile": [2.0,2.0] }
```

`tile_pos_in_workspace_view` is `null` for all three open windows, and the source shows
why: only the **floating** layout fills it (`src/layout/floating.rs:336`
`tile_pos_in_workspace_view: Some(pos.into())`), while the scrolling layout sets only
`pos_in_scrolling_layout` and inherits the template's `None`
(`src/layout/scrolling.rs:2428-2429`, `src/layout/tile.rs:866`). Foreign-toplevel has no
geometry either (§5.2). **There is no IPC or Wayland path to a tiled window's pixel
position on this compositor**, so the crop rectangle cannot be computed. This candidate is
dead for the common case, not merely awkward.

**(c) One-frame `RecordWindow` cast** — works, but is the wrong tool for a still. It
negotiated `2507x1457` for a window of logical 1671×971 (× 1.5 = 2506.5, 1456.5 → rounded
up), which is correct. But it delivered **1 frame in 6 seconds** from an idle window,
because window casts are damage-driven (§2.3). A screenshot that waits an unbounded time
for the user to jiggle the window is not a screenshot. It also carries the full
session/PipeWire/dmabuf apparatus and the odd-dimension problem (§3.6).

Also recorded, for Stage 11's error handling — the bogus-id failure path
(`docs/research/2026-08-08-stage2/window-04`):

```
RecordWindow 999999 ->  o "/org/gnome/Mutter/ScreenCast/Stream/u12"     <-- accepted
Start ->                (no output)  busctl exit=0                       <-- "succeeds"
--- session-interface signals seen:
   Path=/org/gnome/Mutter/ScreenCast/Session/u10  Member=Closed
--- is the session object still there?
Failed to introspect ...: Unknown object '/org/gnome/Mutter/ScreenCast/Session/u10'
```

`Start` returns success, then niri emits `Session.Closed` and destroys the session. There
is no error return to check. **Stage 11 must subscribe to `Session.Closed` and treat
"`Start` returned but no `PipeWireStreamAdded` within a timeout" as the failure signal —
and must not call `Stop` on an already-closed session.**

### 5.4 Decision

- **Window screenshot (`shot --window`) → (a) `Action::ScreenshotWindow`** with
  `path = <our own temp path>`, `write_to_disk = true`, `show_pointer` from the cursor
  option; then read and delete the PNG, and re-encode to the user's chosen format via
  `storage.rs`. It is the only candidate that gives correct, occlusion-free, scale-correct
  window pixels at all. The disk round-trip is ~one PNG encode+write+read of a
  window-sized image, which is acceptable next to the ~0.3 s screencopy already in the
  flow.
  - **Mitigate the clipboard side effect**: niri sets an `image/png` selection
    unconditionally. When the resolved options say `--no-copy`, `storage.rs` must restore
    or overwrite the selection afterwards; when they say copy (the default), overwrite it
    with the final encoded image so the clipboard matches the saved file rather than
    niri's intermediate PNG. Either way the app, not niri, owns the final clipboard state.
  - Window **picking** is ours: `niri msg windows` for the list (id, title, app_id,
    `window_size`, focused, floating), presented via the overlay toolbar or a list.
    Highlighting a window under the cursor is **not** implementable — see the limitation
    below.
- **Window recording (`record --window`) → (c) `RecordWindow`**, which is the only option;
  keyed by the same `niri-ipc` window id (§5.1), with the §5.3 failure handling and the
  §3.6 even-dimension crop.
- **Region recording** stays monitor-cast-plus-crop (there is no `RecordArea`
  **[pre-plan]**). Crop in the filter chain (`crop=W:H:X:Y` before `hwupload`) rather than
  in our own code: it keeps the memcpy on ffmpeg's side and composes with the
  even-dimension crop.

**Documented v0.1 limitation:** because niri exposes no pixel position for tiled windows
(§5.3b), the window picker cannot offer a hover-to-highlight-the-window-under-the-cursor
interaction. Picking is by list (title/app_id/thumbnail), or by "the focused window".
Firming this up would need a niri feature request to populate
`tile_pos_in_workspace_view` for scrolling-layout tiles — worth filing, out of scope here.

---

## 6. Overlay viability in `iced_layershell`

Tested in a **nested niri** per the binding rule: `touch /tmp/nested-niri.kdl`,
`niri -c /tmp/nested-niri.kdl &` (**no `--session`**), which announced
`listening on Wayland socket: wayland-2` and
`IPC listening on: /run/user/1000/niri.wayland-2.76302.sock`. `NIRI_SOCKET` was overridden
explicitly on every `niri msg` (the shell's points at the outer niri), the nested output
was forced to the fractional scale Jordan actually runs
(`niri msg output winit scale 1.5` → logical 825×971 from mode 1238×1457), everything ran
against `WAYLAND_DISPLAY=wayland-2` only, and the nested compositor was terminated at the
end (verified: no `nested-niri` process remains, `niri msg outputs` on the real session
still reports eDP-1).

Input was injected **inside the nest only**, via `zwlr_virtual_pointer_manager_v1` and
`zwp_virtual_keyboard_manager_v1` bound on `wayland-2` (a purpose-written `inject` client
that also re-uses the nested compositor's own keymap fd for the virtual keyboard). No
input was injected into Jordan's session at any point.

The probe app is iced 0.14 + iced_layershell 0.19.1, `build_pattern::daemon`, mirroring
saola-panel's shape. **It compiled and ran first try.**

### 6.1 `Layer::Overlay` + `KeyboardInteractivity::Exclusive` — works

```
=== nested niri layers ===
Output "winit":
  Overlay layer:
    Surface:
      Namespace: "saola-capture-lstest"
      Keyboard interactivity: exclusive
```

### 6.2 Escape reaches the app — works

```
[key] pressed Named(Escape)
[key] ESCAPE received on an Exclusive-keyboard layer surface
=== REPORT
  escape delivered to Exclusive surface: true
```

`iced::exit()` from the update arm unmapped the surface and ended the process cleanly.
Escape arrives as a plain `iced::Event::Keyboard(KeyPressed { key: Named(Escape) })` via
`iced::event::listen()` — no special layer-shell handling needed.

### 6.3 Pointer drag fidelity — pixel-exact, in logical coordinates

Injected a press at logical (206,242), eight motions, release at (577,631). What the app
received (`docs/research/2026-08-08-stage2/overlay-03`, `overlay-04`):

```
[mouse] press Left at Some(Point { x: 206.2461, y: 242.2461 })
[mouse] drag motion #1 -> Point { x: 252.30469, y: 290.29688 }
[mouse] drag motion #2 -> Point { x: 298.35938, y: 339.34766 }
[mouse] drag motion #3 -> Point { x: 345.41797, y: 387.39844 }
[mouse] drag motion #4 -> Point { x: 391.47266, y: 436.4453 }
[mouse] drag motion #5 -> Point { x: 437.52734, y: 485.4961 }
[mouse] release Left at Some(Point { x: 577.6992, y: 631.64844 }); drag had 8 motion events;
        rect = Some((Point { x: 206.2461, y: 242.2461 }, Point { x: 577.6992, y: 631.64844 }))
```

All eight motions delivered, in order, none coalesced away. Coordinates are **logical**
(the surface is 825×971 logical on a 1238×1457 physical output) and land within 0.25 px
of the injected values. Stage 6's geometry core converts logical→physical with the output
scale, exactly as §1.4 requires.

### 6.4 Frozen-frame background at output size — renders

A grim capture of the nested output was loaded as an `image::Handle::from_path` and drawn
full-bleed under a canvas scrim and the selection rect. Screenshotting the nested
compositor while the overlay was mapped shows the HUD text, the darkened scrim over the
frozen frame, and the terracotta selection rectangle at exactly the dragged coordinates
(physical (309,362)–(866,947) = logical (206,242)–(577,631) × 1.5). No tearing, no
scaling artifacts, no measurable frame-rate problem at this size.

### 6.5 Per-output surfaces — two mechanisms, one live-tested

**`StartMode::AllScreens`** creates one layer surface per `wl_output` at init. Source,
`layershellev-0.19.1/src/lib.rs:2303-2368`:

```rust
} else {
    let displays = self.outputs.clone();
    for output_display in displays.iter() {
        let wl_surface = wmcompositer.create_surface(&qh, ());
        let layer = layer_shell.get_layer_surface(&wl_surface, Some(output_display), self.layer, ...);
        ...
        self.push_window(WindowStateUnitBuilder::new(id::Id::unique(), ...).wl_output(Some(output_display.clone())).build());
    }
}
```

Each gets its own `window::Id`, which is what `view(&self, id)` dispatches on. Hotplug is
handled too — `DispatchMessageInner::NewDisplay` creates another surface when
`is_allscreens()` (`lib.rs:2501-2530`).

**`NewLayerShellSettings { output_option: OutputOption::OutputName(String), .. }`** is the
on-demand equivalent, and is the shape Stage 6 actually wants: the daemon is long-lived,
the overlay exists only during a region shot. Live-tested — booting with
`StartMode::Background` (no layer surface) and then sending one
`Message::layershell_open(settings)` per named output produced:

```
[boot] requesting on-demand overlay on output "winit"
[boot] -> surface Id Id(1)
=== nested niri layers ===
  Overlay layer:
    Surface:
      Namespace: "saola-capture-lstest-overlay"
      Keyboard interactivity: exclusive
```

and the same drag + Escape results as §6.2/§6.3. `OutputOption` also offers
`LastOutput`, `Output(wl_output)` and `Active`.

### 6.6 Verdict, and the one thing not live-tested

**iced_layershell is viable for the overlay. No smithay-client-toolkit fallback is
needed.** Every unproven item in PLAN.md's risk list came back green.

**Honest gap: multi-output was verified from source, not live.** The nested niri's winit
backend provides exactly one output, and niri's headless backend (which can add
`headless-N` outputs, `src/backend/headless.rs:61`) is selected by an internal flag with
no CLI surface (`src/niri.rs:719-724`) — it is for niri's own integration tests. Jordan's
machine has one physical output. So the per-output loop is read, not run.

Two specific risks follow, neither blocking:

1. **N surfaces each with `KeyboardInteractivity::Exclusive`.** Both code paths set the
   same interactivity on every surface. Which one receives Escape when several are
   exclusive is unspecified by the protocol and untested here. Stage 6 should handle
   Escape identically on every surface (cancel the whole selection regardless of which
   surface saw the key), which makes the question moot.
2. **Cross-output drags.** A drag that starts on one surface and ends on another needs
   the overlay state to be shared across surfaces (it already is — one `Overlay` struct,
   `view(&self, id)`) and coordinates translated into a global logical space using each
   output's logical position from `niri msg outputs`. Untested.

What would firm this up: a second physical or DP-MST output on Jordan's machine, or a
niri build with the headless backend exposed. Until then, **v0.1 may ship a
single-output-aware overlay with the limitation documented**, exactly as PLAN.md allows —
but the code should be written against the per-output shape (`view(&self, id)` +
`OutputOption::OutputName` per output from `niri msg outputs`) so enabling it later is
configuration, not a rewrite.

---

## 7. Enumeration

- **`niri msg casts` and the cast events** work as assumed **[pre-plan]** and were
  re-exercised repeatedly here: every session driver in this stage printed
  `niri msg casts` before and after, showing `Cast stream ID N / Session ID N /
  Kind: PipeWire / Target: output "eDP-1" | window 19 / PipeWire node ID: 70` while live
  and `No screencasts.` after teardown. Teardown is clean every time; node 70 was reused
  across sessions.
- **`is_active` is false until a consumer attaches**, as the pre-plan probe found. It is
  a consumer-attached flag, not a "cast exists" flag — do not use it to decide whether a
  recording is running.
- **`ext_foreign_toplevel_list_v1`** listing shape is §5.2: `identifier` / `title` /
  `app_id` / `done`, three handles for three windows, `identifier` == niri window id.
  **Not needed by this app** — `niri msg windows` returns a superset in one call.

---

## 8. Decisions

Every later stage keys off this section.

### D1 — Screenshots: `zwlr_screencopy_v1`, shm, `Xrgb8888` (Stage 4)

Request `capture_output`; read `buffer`/`linux_dmabuf`/`buffer_done`; attach a `wl_shm`
buffer in `Xrgb8888` at the advertised `width`/`height`/`stride`. Memory order is
**B, G, R, X** → swizzle to RGBA. `stride` is the authority, not `width * 4` (it happens
to be equal on this output; do not assume it). Read the `flags` event and honour
`YInvert`, but expect it to be 0 always on niri (§1.2). Alpha is absent; set A = 255.
Budget ~0.3 s for a full-output capture.

### D2 — Region: capture first, map second, crop in memory (Stage 4/6)

Screencopy composites layer surfaces (§1.5), so the frozen frame must be grabbed before
the overlay maps. Prefer capturing the whole output and cropping in memory over
`capture_output_region` — the overlay needs the full frozen frame as its background
anyway, and one capture serves both. `--geometry WxH+X+Y` is in **logical** coordinates
(matching slurp/grim convention); convert with `round(logical * scale)` — verified exact
against the protocol's own rounding (§1.4).

### D3 — Window screenshot: `niri-ipc` `Action::ScreenshotWindow` to our own path (Stage 7)

It renders the window's own elements offscreen, so occlusion, floating overlap and
fractional scale are all handled by niri. Pass an absolute `path` we control,
`write_to_disk = true`, `show_pointer` from the cursor option; read the PNG (RGBA), delete
it, re-encode via `storage.rs`. **Take ownership of the clipboard afterwards** — niri sets
an `image/png` selection unconditionally (§5.3a). Geometry-crop is impossible (no pixel
position for tiled windows) and a one-frame `RecordWindow` is damage-gated. Window picking
is by list, not by hover-highlight; record that limitation in the README.

### D4 — Recording buffers: dmabuf, LINEAR, mmap. There is no shm path (Stage 9)

Offer `BGRx` with a `MANDATORY|DONT_FIXATE` modifier enum containing `LINEAR` (0x0), and
`SPA_PARAM_Buffers dataType = 1 << SPA_DATA_DmaBuf`. `mmap(PROT_READ, MAP_SHARED)` the fd,
bracket reads with `DMA_BUF_IOCTL_SYNC` START/END. **Use `chunk->stride` for the row
pitch** (it is the output pitch, not `width * 4`, on window casts) and never trust
`maxsize`/`chunk->size` on the dmabuf path (§2.3). `PW_STREAM_FLAG_MAP_BUFFERS` does not
map dmabufs. Negotiation failure is a user-visible `Error`, not a silent degrade.

### D5 — Stage 9 install prerequisite — **cleared 2026-08-08**

`pipewire` 0.10 could not build here because `libclang` was missing (§2.0). Jordan has
since installed clang (22.1.8, verified live) — Stage 9 does not need to print-and-wait;
the bindgen build is unblocked.

Still true for Stage 16: `clang` in PKGBUILD `makedepends`, `libclang-dev` in CI build
deps.

### D6 — Encode: `hevc_vaapi` primary, GPU colour conversion with the matrix pinned (Stage 10)

Preset table in §3.7. The non-obvious parts:

- `hwupload,scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=tv`, **not**
  `format=nv12,hwupload` — 10× less CPU (0.34 s vs 3.50 s per 2 s of video), and the
  explicit matrix/range is what keeps the colours correct.
- **Crop to even dimensions before the encoder.** `hevc_vaapi` silently emits a
  2508×1458 stream for a 2507×1457 input (§3.6).
- **`-use_wallclock_as_timestamps 1 -fps_mode vfr`** on the rawvideo input. Without it a
  variable-rate cast becomes a fast-forwarded video — 3.00 s of capture rendered as 1.00 s
  in the measurement (§4.2).
- AV1 is software `libsvtav1` (no AV1 encode entrypoint on the 680M, §3.3) and is **not
  realtime at 2560×1600@60** — cap the AV1 preset at 30 fps or document the drops (§3.4).
- H.264 uses `h264_vaapi` → MP4, not `libx264`.
- ffmpeg presence is checked up front; its absence names `sudo pacman -S ffmpeg`.
- **Amended 2026-08-08 (agreed with Jordan; binding on Stage 10 — see PLAN.md Stage 10
  item 3)**: `-vaapi_device /dev/dri/renderD128` in §3.7's table is the *measured
  example on the iGPU-only machine*, not a constant to ship. Jordan's dGPU is normally
  disabled but can appear, adding a second render node and potentially reshuffling which
  device owns `renderD128` — and a future node may have AV1 encode. Stage 10 enumerates
  `/dev/dri/renderD*` at encoder start, probes each candidate with a tiny ffmpeg
  null-sink trial encode (ffmpeg stays the sole external CLI; no runtime `vainfo`),
  prefers a node with a working AV1 encode entrypoint (then the AV1 preset uses
  `av1_vaapi`), falls back to `libsvtav1` otherwise, caches per daemon run, and adds an
  optional `vaapi-device` override knob to `capture.kdl`.

### D7 — Audio: `-f pulse` into the same ffmpeg, with three mandatory mitigations (Stage 12)

Reasoning and evidence in §4.4. Spawn ffmpeg only after the first video frame (which is
forced anyway, since `-video_size` needs the negotiated size); `-shortest` plus a graceful
stop signal (`-f pulse` never EOFs and *will* hang the process otherwise); measure the
residual A/V offset with the clap test and bake it in as `-itsoffset`. Escalate to PCM on
`pipe:3` only if the measured offset is not constant across runs. Device names come from
`pactl list short sources` at record time.

### D8 — Window/region recording (Stage 11)

`RecordWindow` keyed by the `niri-ipc` window id (id spaces are unified, §5.1). Region
recording is a monitor cast cropped in ffmpeg's filter chain (`crop=W:H:X:Y` before
`hwupload`), because there is no `RecordArea`. Failure handling: `Start` returns success
for a bogus window id and then the session self-destructs — subscribe to `Session.Closed`,
time out on a missing `PipeWireStreamAdded`, and never `Stop` a closed session (§5.3).
Window casts are damage-driven and can go seconds between frames; the recorder must not
interpret frame silence as failure.

### D9 — Overlay: `iced_layershell`, on-demand, output-targeted (Stage 6)

No smithay-client-toolkit fallback. Boot the daemon with no overlay surface and spawn one
per output on demand via `NewLayerShellSettings { output_option:
OutputOption::OutputName(name), layer: Layer::Overlay, keyboard_interactivity: Exclusive,
anchor: all four, exclusive_zone: -1, size: (0,0) }`, one per output from
`niri msg outputs`. Escape arrives as an ordinary iced keyboard event; handle it
identically on every surface. Pointer coordinates are logical and pixel-faithful. The
frozen frame renders full-bleed under the scrim with no trouble.

### D10 — Multi-output is the one thin spot

Per-output surface creation is **source-verified, not live-verified** (§6.6): this machine
has one output and niri's headless backend is not reachable from the CLI. Cross-output
drag and multi-exclusive-keyboard arbitration are untested. Write the code against the
per-output shape, ship v0.1 single-output-aware if anything bites, and document it.
Firming it up needs a second output on Jordan's machine.

### D11 — Things that turned out not to matter

- `ext_foreign_toplevel_list_v1` is strictly weaker than `niri msg windows`; do not use it
  (§5.2). (If some future need arises: `staging` feature + `event_created_child!` inside
  the `impl Dispatch`.)
- `ext_image_copy_capture` is absent, so grim-style `-T` toplevel capture was never a
  candidate **[pre-plan]**.
- `is_active` on `niri msg casts` means "a consumer is attached", not "a cast exists".
