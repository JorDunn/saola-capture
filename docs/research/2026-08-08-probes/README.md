# Pre-plan capture probes — 2026-08-08

Live probes run on Jordan's real session (niri 26.04, PipeWire 1.6.8,
eDP-1 2560x1600@60 scale 1.5 / logical 1706x1066) during the planning
session, ahead of PLAN.md Stage 2. Input injection used **ydotool** 1.0.4
(socket `/run/user/1000/.ydotool_socket`). Raw transcripts sit next to this
file; `input-tests/NOTES.md` and `screencast-probes/SUMMARY.txt` are the
per-suite write-ups. Stage 2 should treat these as verified evidence and
fill the remaining gaps (listed at the bottom).

## Verified findings

### Interactive (input-tests/)

- **Drag through a full-screen grab overlay is pixel-exact** (slurp:
  press → 4 moves → release, geometry perfect). The Stage 6 overlay input
  model is viable.
- **Screencopy composites overlay surfaces**: a grim taken while niri's
  screenshot UI was open captured the UI. Capture-the-frozen-frame
  **before** mapping the overlay is mandatory, not just tidy.
- **PickColor works**: `busctl --user call org.gnome.Shell.Screenshot
  /org/gnome/Shell/Screenshot org.gnome.Shell.Screenshot PickColor` blocks
  until click, returns `(ddd)` doubles 0–1, verified pixel-exact.
- niri's built-in `screenshot-screen` writes
  `~/Pictures/Screenshots/Screenshot from YYYY-MM-DD HH-MM-SS.png`
  (physical-res PNG) + clipboard, and emits a `ScreenshotCaptured` event
  with the path. The built-in UI freezes+dims with a hint bar; Escape
  cancels; UI open/cancel emits **no** event.
- **ydotool absolute coords map 2.0x onto niri logical coords** here (not
  the 1.5 output scale) — calibrate with a probe drag per session, never
  hardcode. slurp reports logical coords, size-inclusive (+1). grim `-g`
  takes logical, outputs physical (fractional-scale rounding: 400x300
  logical → 599x449 px).

### ScreenCast / protocols (screencast-probes/)

- Protocol globals: `zwlr_screencopy_manager_v1` **v3**, layer-shell v5,
  `ext_foreign_toplevel_list_v1` v1, wlr-foreign-toplevel v3, dmabuf v5.
  **No `ext_image_copy_capture_manager_v1`** → grim `-T` window capture is
  dead on this niri.
- Full `org.gnome.Mutter.ScreenCast` v4 handshake verified:
  CreateSession → session `/…/Session/u2` → RecordMonitor("eDP-1") →
  Start → `PipeWireStreamAdded` → pw node 70 → Stop (clean teardown,
  `niri msg casts` empty after).
- **No `RecordArea`.** The session interface has only RecordMonitor
  (`sa{sv}→o`) and RecordWindow (`a{sv}→o`). Region recording must be
  full-monitor cast + crop.
- **Cursor-mode is the Mutter enum** (0=hidden, 1=embedded, 2=metadata);
  3/4 rejected — it is not the portal bitmask.
- **The cast node offers dmabuf only** — a single EnumFormat pod: BGRx,
  2560x1600 (physical), variable framerate up to 60, with a **MANDATORY**
  modifier property (8 AMD modifiers incl. LINEAR `0x0` and
  `MOD_INVALID`). No modifier-less (shm) pod advertised.
- RecordWindow requires `window-id (t)`; bogus ids are accepted at
  RecordWindow time (validation deferred to Start). RecordMonitor with a
  bad connector fails cleanly ("no such monitor").
- The stream object's `Parameters` property reports position/size in
  **logical** pixels while the video is physical-res.
- `niri msg casts` + `CastsChanged`/`CastStartedOrChanged`/`CastStopped`
  events work as PLAN.md assumes (incl. `pw_node_id`, `is_active`).
- grim timing: full output 0.40 s, small region 0.04 s. grim encodes
  png/ppm/jpeg only.
- Audio sources (pulse shim): exactly one monitor
  (`alsa_output.pci-0000_07_00.6.analog-stereo.monitor`) and one mic
  (`alsa_input.pci-0000_07_00.6.analog-stereo`), both s32le 2ch 48 kHz.

## Gaps Stage 2 still owns

1. **Is shm negotiable?** The node advertises dmabuf-mandatory, but
   negotiation is bidirectional and no consumer existed to test with.
   Minimal pipewire-rs client offering BGRx without the modifier prop;
   inspect the negotiated Buffers `dataType`. If shm is refused, Stage 9
   goes dmabuf-first (LINEAR `0x0` is in the offer — mmap-able fallback).
2. ffmpeg still not installed — the VA-API encode smoke test is pending
   Jordan's `sudo pacman -S ffmpeg`.
3. Does RecordWindow's `window-id` match niri-ipc window ids? Needs a
   consumer attached (validation happens at Start).
4. iced_layershell-specific overlay checks (per-output spawn, Exclusive
   keyboard from iced) in nested niri.
