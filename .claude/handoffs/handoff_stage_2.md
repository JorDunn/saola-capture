# Stage 2 handoff — capture research

Deliverable: `docs/CAPTURE-RESEARCH.md` (evidence of record) plus raw transcripts,
probe sources and two overlay screenshots in `docs/research/2026-08-08-stage2/`
(45 files). Nothing was committed. No source files in `src/` were touched; the crate
still builds and clippies clean because Stage 2 added no code to it.

**Stage 3 (config + CLI + D-Bus dispatch + daemon scaffold) is barely affected by any
of this** — it is the one stage downstream of Stage 2 that touches no capture path.
The parts that do land in Stage 3: the `--audio`/`--preset` flag *values* below, and
the daemon-scaffold note in "What changes for Stage 3". Everything else is for
Stages 4, 6, 7 and 9–12; it is here so it survives the context boundary.

---

## What changed in CLAUDE.md (keep-current rule)

Three edits, no restructuring:

1. **Status block** — now "Stages 1–2 landed", plus a prominent note that
   **Jordan must run `sudo pacman -S clang` before Stage 9** (`pipewire` 0.10 cannot
   build here without libclang; verified, not assumed).
2. **Architecture** — the `docs/CAPTURE-RESEARCH.md` bullet lost its
   *(pending Stage 2)* marker and gained a four-item "decisions that most often get
   re-invented wrong" list (dmabuf-only recording; window screenshots via niri-ipc not
   geometry crop; the three mandatory ffmpeg flags; iced_layershell confirmed viable).
   It also states that CAPTURE-RESEARCH §8 is **binding on Stages 4, 6, 7 and 9–12**.
3. **Dependency surveys** — the pending-pipewire note now carries the libclang
   build-time fact (a real cost for the Stage 9 survey, a `makedepends`/CI entry for
   Stage 16), and a pointer that `wayland-protocols`' `staging` feature is only needed
   if Stage 7 uses `ext_foreign_toplevel_list_v1` — which §5.2 says it should not.

---

## Surprises worth carrying forward

Ranked by how much damage they'd do if rediscovered late.

1. **`pipewire` 0.10 does not build on this machine at all.** `pipewire-sys` runs
   bindgen; `libclang` is absent (`find / -name 'libclang.so*'` → nothing;
   `pacman -Q clang llvm` → neither installed). PipeWire dev headers *are* present, so
   only the bindgen toolchain was missing. **Since cleared** — Jordan installed clang
   22.1.8 on 2026-08-08 (see D5); Stage 9 is not blocked. The Stage 2 PipeWire probe was
   written in **C** against `libpipewire-0.3` directly — arguably better evidence anyway.
2. **The naive rawvideo-on-stdin setup silently produces a fast-forwarded video.**
   3.00 s of real capture came out as a **1.000 s** file. The cast negotiates
   `framerate 0/1` (variable), so this is the normal case, not a corner case.
   `-use_wallclock_as_timestamps 1 -fps_mode vfr` is mandatory.
3. **`hevc_vaapi` silently changes the resolution on odd inputs.** 2507x1457 in →
   **2508x1458** out, no warning. Window casts negotiate odd sizes routinely
   (2507x1457 was measured). Crop to even before the encoder.
4. **niri exposes no pixel position for tiled windows.** `tile_pos_in_workspace_view`
   is `null` for every scrolling-layout window — only the *floating* layout populates
   it (`src/layout/floating.rs:336` vs `src/layout/scrolling.rs:2428`). This kills the
   geometry-crop window-capture candidate outright and means the window picker cannot
   do hover-to-highlight. It is a documented v0.1 limitation, not a bug to fix.
5. **`-f pulse` audio silently loses the true A/V offset.** A video stream starting
   1.0 s after ffmpeg produced `v first_pts=0.000, a first_pts=-0.007` — the 1 s gap
   vanished. `-copyts -start_at_zero` did **not** rescue it. Also: `-f pulse` never
   EOFs, so the first combined-command attempt **hung until killed**; `-shortest` plus
   an explicit stop signal are both required.
6. **GPU colour conversion is 10× cheaper but wrong unless the matrix is pinned.**
   `format=nv12,hwupload` costs 3.50 s of CPU per 2 s of video; `hwupload,scale_vaapi`
   costs 0.34 s — but bare `scale_vaapi` renders pure red as (232,0,2). Adding
   `out_color_matrix=bt709:out_range=tv` fixes the colour and keeps the speed.
7. **The window id space is unified.** `niri-ipc Window.id` ==
   `ext_foreign_toplevel_handle_v1.identifier` (stringified) == ScreenCast
   `window-id` — one `MappedId(u64)` (`src/window/mapped.rs:209`). Verified live:
   `RecordWindow` with id 19 gave `niri msg casts → Target: window 19`.
8. **A bogus `RecordWindow` id "succeeds".** `Start` returns exit 0 with no error, then
   niri fires `Session.Closed` and destroys the session object. There is no error
   return to check — subscribe to `Session.Closed`, time out on a missing
   `PipeWireStreamAdded`, and never `Stop` a closed session.
9. **`iced_layershell` worked first try** — the overlay probe compiled and ran with no
   API fights. Exclusive keyboard, Escape, 8/8 pointer motions delivered within 0.25 px,
   frozen-frame background rendering: all green in nested niri. No smithay fallback
   needed. The only gap is multi-output, which is source-verified only (this machine
   has one output and niri's headless backend has no CLI surface).
10. **Two smaller ones.** `PW_STREAM_FLAG_MAP_BUFFERS` does *not* map dmabufs
    (`data = (nil)`), and on the dmabuf path `maxsize`/`chunk->size` are dummy `1`s
    while `chunk->stride` is the *output* pitch, not `width * 4`. Also, the pre-plan
    note that grim `-g "100,100 400x300"` yields 599x449 is a grim artifact — the
    protocol yields **600x450**, verified byte-exact against the full-output capture.

---

## What changes for Stage 3 specifically

- Nothing in Stage 3's four tasks is invalidated. Config knobs, CLI shape, D-Bus
  interface and the daemon scaffold are all as PLAN.md specifies.
- `--preset` values stay `hevc|av1|h264`, but note for the help text and README that
  **av1 is software-encoded and not realtime at 2560x1600@60** (measured 0.58–0.84×);
  it should be capped at 30 fps by Stage 10.
- `--audio` values stay `mic|system|both`. Device *names* must be resolved at record
  time from `pactl list short sources` — they are hardware-path-derived
  (`alsa_input.pci-0000_07_00.6.analog-stereo`,
  `alsa_output.pci-0000_07_00.6.analog-stereo.monitor`) and must never be hardcoded.
- The daemon scaffold should boot **surfaceless** (`StartMode::Background`, which
  creates a bare `wl_surface` with no shell role) rather than `StartMode::Active`:
  Stage 2 proved the on-demand path (`NewLayerShellSettings` with
  `OutputOption::OutputName`) works, and it is the shape Stages 5/6 need — the daemon
  is long-lived, the overlay exists only during a region shot.
- `--copy/--no-copy` gets a wrinkle from D3: the window-screenshot path clobbers the
  clipboard inside niri, so `storage.rs` must own the final clipboard state either way.

---

## The decision section, verbatim from `docs/CAPTURE-RESEARCH.md`

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

`pipewire` 0.10 could not build here because `libclang` was missing. Jordan has since
installed clang (22.1.8, verified live by the orchestrator) — Stage 9 does not need to
print-and-wait; the bindgen build is unblocked.

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
  item 3)**: the preset table's `-vaapi_device /dev/dri/renderD128` is the *measured
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
