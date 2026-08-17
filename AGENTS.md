# AGENTS.md — saola-capture

Screenshot and screen-recording app for the Saola desktop environment,
targeting the **niri** compositor. One binary, three run modes: **daemon**
(iced_layershell multi-surface daemon owning the selection overlay, camera
flash, toast stack, tray item, capture engine, recording pipeline, and the
`io.saola.Capture1` bus name), **window** (a separate-process iced app: main
window, history library, annotation editor), and **CLI verbs** (`shot`,
`record`, `pick-color`, `open` — thin D-Bus clients; what keybinds call;
`--no-daemon` captures in-process and headless for scripts).

**Keep this file current.** Every PLAN.md stage that changes commands,
architecture, dependencies, or conventions updates this file in the same
stage and says so in its handoff. A stale AGENTS.md is a bug.

> Status: Stages 1–16 landed (repo skeleton, dependency survey; every capture
> path proven with live evidence in `docs/CAPTURE-RESEARCH.md`; full CLI
> parsing, `capture.toml` config, the `io.saola.Capture1` bus, a surfaceless
> daemon boot; Stage 5's **real screenshot pipeline**; Stage 6's **PrintScr
> MVP**; Stage 7's **region selection overlay**; Stage 8's **window capture
> and a visible delayed-capture countdown**; Stage 9's **main app window
> process**; and — new in Stage 10 — the **ScreenCast session + PipeWire
> frame pipeline**).
> `shot --fullscreen`, `shot --region [--geometry WxH+X+Y]` and (as of Stage
> 8) `shot --window [--window-id ID]` all genuinely capture, encode, save,
> copy and print a path. Fullscreen, `--geometry` region and `--window` all
> work **both ways** — through the daemon's `Screenshot` D-Bus method (which
> also emits `CaptureTaken`) and in-process via `--no-daemon` — over the same
> two library calls, `capture::take_screenshot` → `storage::save_capture`. As
> of Stage 6, the daemon path additionally **flashes and toasts**:
> `src/modules/flash.rs` (a full-output ivory fade, live-verified in nested
> niri) and `src/modules/toast.rs` (the §6 notification card, stack of 3,
> click opens the — still-stub — editor); see Conventions for two binding
> gotchas Stage 6 found the hard way.
> **Stage 7's `src/modules/overlay.rs`** is the daemon-only interactive
> region path: freeze the focused output, map an `Exclusive`-keyboard
> layer-shell surface showing that frozen frame, let the user drag/move/
> resize a rectangle (scrim + dashed terracotta edge + 8 handles + size
> readout + floating toolbar), then crop **that same frozen frame** in memory
> and hand it to the same `storage::save_capture` tail. Live-verified end to
> end in nested niri with injected input. `--no-daemon --region` with no
> `--geometry` stays an error by design: a surfaceless process has nowhere to
> draw.
> **Stage 8 makes `--window` real** two ways: a bare `shot --window` (or
> `--window-id ID` for the scriptable path) captures via niri-ipc's
> `Action::ScreenshotWindow` — no crop math, niri renders the window's own
> elements offscreen — with no picker (niri exposes no pixel position for a
> tiled window, so hover-highlight isn't implementable; CAPTURE-RESEARCH D3's
> answer is "or by the focused window", which is what a missing
> `--window-id` resolves to); and the region overlay's **Window toolbar
> button is now wired** — it confirms the same focused-window resolution
> instead of a dragged rectangle, without needing its own picker UI. **Stage
> 8 also adds a visible countdown**: `src/modules/countdown.rs` maps a small
> ink pill (tabular-numeral whole seconds, self-expiring) for the duration of
> any `--delay N` on any of the three shot kinds — the sleep itself is
> unchanged (still `capture::sleep_for_delay`, run inside the blocking
> capture task), this is purely the visible half of a wait that used to be
> silent. **A Stage 5–7 bug fixed in Stage 8**: a delayed `--fullscreen` used
> to sleep for `2 × --delay` (once in `take_screenshot`'s old top-level
> sleep, again inside `freeze_focused_output`); delay now has exactly one
> owner per shot kind.
> **Stage 9 makes the `window` process and `OpenWindow` real**:
> `src/modules/app.rs` is a plain `iced::application` (not `iced_layershell`
> — a normal `Surface::Paper` niri toplevel, a separate process from the
> daemon, per Architecture's forced process split) with Screenshot/Record
> mode tabs, a target picker, delay/cursor/format/preset/audio options (all
> segmented controls — no new saola-theme gaps), and a Capture/Start
> Recording button. Pressing it hides the window
> (`iced::window::Mode::Hidden`), calls the daemon exactly the way `shot`/
> `record` already do, and re-shows on the reply — see the module's own doc
> comment for why Screenshot's wait is already complete (the `Screenshot`
> method itself blocks until done) while Record's is deliberately only a
> stub-reply wait for now (a real "hidden until `RecordingFinished`" wait
> needs recorder-state awareness and a connection-keyed signal subscription
> that PLAN.md assigns to Stage 12, not this one — `zbus::Connection` isn't
> `Hash`, which is part of why). `window edit <path>` now boots straight
> into a stub editor view (image decoded synchronously, shown at
> `ContentFit::Contain`, a "tools land in Stage 14" note, no canvas) instead
> of printing and exiting. `dbus.rs`'s `OpenWindow` spawns
> `saola-capture window [edit <path>]` detached instead of erroring — the
> `open` CLI verb and the toast's own (unchanged) direct spawn are two of
> the three "reopen" paths this enables; the tray menu's "Open Saola
> Capture" (Stage 12) turned out to be a third, but calls
> `dbus::spawn_window_process` **directly**, in-process, rather than going
> through the `OpenWindow` D-Bus method — `modules::tray` already runs
> inside the daemon, so a self-call over its own bus would be pure overhead
> for the same effect. None of the three can currently tell whether a
> window process is already alive-but-hidden, so a second `open` (from any
> of them) mid-capture spawns a second process — a recorded v0.1 gap, not
> an oversight (see the Stage 9 handoff).
> **Stage 10 makes the video capture path real, up to (not including) the
> encoder**: `src/capture/screencast.rs` is the whole
> `org.gnome.Mutter.ScreenCast` → PipeWire chain — `CreateSession` →
> `RecordMonitor`/`RecordWindow` → subscribe → `Start` → node id
> (`CastSession`), then a **dedicated OS thread** running the PipeWire main
> loop that negotiates the dmabuf/LINEAR format, mmaps each frame, and pushes
> **packed** BGRx copies down a bounded channel (`PipeWireStream`,
> `VideoFrame`). `record start --dry-run` is the user-visible half: it
> negotiates, watches frames for five seconds, prints the negotiated format
> plus the frame cadence, writes nothing, and tears everything down —
> **live-verified against the real session** (see the Stage 10 handoff for
> the transcripts: fullscreen negotiated `BGRx 2560x1600 modifier 0x0`, a
> `--window-id` cast negotiated `2507x1457` at stride 10240 and came back
> pixel-perfect, and a bogus window id produced CAPTURE-RESEARCH §5.3's
> documented self-destructing session as a clean error). Nothing in Stage 10
> touches the daemon, the tray, or any surface; `StartRecording` is still a
> stub until Stage 11 adds the encoder.
> **Stage 11 closes the recording loop — `record start|stop|toggle` genuinely
> records the focused output.** Three pieces: `src/encode/mod.rs` (the
> `EncoderSink` trait, `EncodePreset`/`VideoEncoder`, CAPTURE-RESEARCH §3.7's
> argument table *as data*, and `select_encoder` — the runtime VAAPI choice,
> written as a pure function over an injected capability oracle so it is
> unit-tested with fakes); `src/encode/ffmpeg_cli.rs` (the one v0.1 sink: an
> `ffmpeg` child fed packed BGRx on stdin, stderr drained by a thread that
> also keeps the last 40 lines for the error message, kill-and-reap on
> `Drop`, `ffmpeg`-missing detected *up front*, plus the `/dev/dri/renderD*`
> discovery and the trial-encode probe that answers the oracle); and
> `src/modules/recorder.rs` (the daemon's `RecorderState` — Idle → Starting →
> Recording → Stopping — and `pump_frames`, the blocking frame loop).
> `StartRecording`/`StopRecording` are no longer stubs, and the interface
> gained one additive read-only property, `Recording b`, because `toggle`
> cannot be implemented correctly without it. **Live-verified against the
> real session, 2026-08-09**: a 10 s fullscreen capture at 2560×1600 encoded
> by `hevc_vaapi` (`VAEntrypointEncSlice` in the log — real hardware, on
> `/dev/dri/renderD128`, the only render node present that day), **10.12 s of
> Matroska for 10.01 s of wall clock** and 0 dropped frames; `av1` (software
> `libsvtav1` — no AV1 encode entrypoint anywhere, §3.3 again) and `h264`
> (`h264_vaapi` → MP4 with `+faststart`) both produced clean, decodable
> files; and the awkward paths — stop-while-starting, a SIGKILLed encoder
> mid-recording, a missing ffmpeg, a non-`fullscreen` kind, `--audio` — were
> each driven live and each came back with one honest sentence and a clean
> teardown. See the Stage 11 handoff for the transcripts.
> **Stage 12 adds the tray item and real region/window recording.**
> `src/modules/tray.rs` is a served `org.kde.StatusNotifierItem` plus a small
> `com.canonical.dbusmenu` (Stop recording / Open Saola Capture / Quit
> daemon), exported on the *same* `io.saola.Capture1` connection the instant
> the bus name is claimed (`dbus::serve` spawns `tray::install` detached) and
> sharing the daemon's one `RecorderState` (`dbus::SharedRecorder`, now
> `pub(crate)`) as its single source of truth — no second copy of "is a
> recording happening". The icon is a procedural terracotta glyph (outline
> idle, filled while recording — AGENTS.md's own "solid icons for record/
> stop/play" rule), not an asset: this crate still has no `src/icons.rs`.
> Elapsed time lives on `Title`/`ToolTip` (read fresh on every property get,
> matching `RecorderState::elapsed`'s own doc comment); nothing pushes it
> per-second, because the one real host (`saola-panel`) has no rendering path
> for either property today — see the module's own doc comment for why that
> is the honest answer and not a shortcut, and for why the layer-shell
> "elapsed chip" PLAN.md made conditional stays **future work**, not built.
> `record start --region [--geometry WxH+X+Y]` and `record start --window
> [--window-id ID]` are both real now, through the daemon: `--geometry`/
> `--window-id` given skips straight to a scriptable recording; a bare
> `--region` reuses the **same** selection overlay `shot --region` maps
> (`dbus::CaptureService::begin_interactive_region`) and, per CAPTURE-
> RESEARCH D8, still casts the whole monitor and crops it in ffmpeg's own
> `-vf crop` filter (`encode::VideoSpec::crop`, a
> `capture::PixelRect` reused rather than duplicated) — there is no
> `RecordArea`. `record start --window` is `CastTarget::Window`, no crop at
> all (a window cast's own negotiated frame already *is* just that window).
> The app window's Record tab (`modules::app`) grew the same Fullscreen/
> Region/Window target picker the Screenshot tab has, and now genuinely
> hides for a recording's whole life: `StartRecording` returning is "the
> recording is live", not "it's done", so the window stays hidden until
> `RecordingFinished`/`Error` arrives on a standing signal listener
> (`record_signal_stream`, a zero-argument `fn` stream exactly like
> `main.rs`'s own `dbus_worker_stream` — `zbus::Connection` still isn't
> `Hash`, so `Subscription::run_with` still isn't the answer). The finish
> toast (`modules::toast::ToastKind::Recording`) is real too — click opens
> the containing directory via `xdg-open` (`main.rs::open_containing_dir`),
> since the editor has no video support yet. **Live-verified against the
> real session, 2026-08-09** (daemon on an isolated `XDG_DATA_HOME`/
> `save-dir`, the real `saola-panel` as the tray host, exactly like Stage
> 11's recipe): the SNI item registered with the live panel watcher
> (`RegisteredStatusNotifierItems` grew to include
> `io.saola.Capture1/StatusNotifierItem`, then shrank back on quit — no
> explicit unregister call needed, the protocol's own bus-owner-gone
> recovery); `GetLayout` over `busctl` returned the three rows with "Stop
> recording" correctly disabled while idle; a `record start --region
> --geometry 640x480+100+100` recording came back as exactly **960×720**
> HEVC (640×480 logical at scale 1.5, physical-pixel-exact against
> `capture::logical_to_pixel_rect`'s own §1.4 rounding); `record start
> --window --window-id <id>` negotiated and recorded that window's own frame
> with no crop; the finish toast rendered on screen with the real saved
> filename (`grim` + a cropped `magick` capture caught the card, ink
> background, "Recording saved" title and all); the tray menu's "Stop
> recording" row correctly no-op'd with a logged reason while idle; and
> "Quit daemon" cleanly stopped a live recording, shut the daemon down, and
> released the bus name. Teardown checked every time: `niri msg casts` →
> "No screencasts", `pgrep -x ffmpeg` empty, no leftover PipeWire
> `Video/Source` node, and the tray watcher's own item list back to its
> pre-test three. **One real bug this stage's own live-testing caught and
> fixed**: the first draft's `main.rs::run_record` still hardcoded
> `"fullscreen"` as `StartRecording`'s `kind` argument (a leftover from
> Stage 11, missed because `cargo test` cannot see across the D-Bus
> boundary) — every `--region`/`--window` CLI invocation was silently
> recording the whole monitor uncropped until the live region-recording
> check caught the mismatch between the requested and encoded resolution.
> Not (yet) live-tested: recording a region/window **from the app window**
> and stopping **from the tray** in one Jordan-driven session (the human-
> verify item below), and clicking the finish toast to actually open a
> folder (would need synthetic input — AGENTS.md's nested-niri rule doesn't
> cover the tray/app-window path at all, and this stage had no safe way to
> click a real tray icon without Jordan present).
> **Stage 13 makes `--audio mic|system|both` real, and fixes two timing
> defects it found on the way.** `src/audio.rs` is the new module: a pure
> `plan_audio` (every device-selection and degradation rule, unit-tested
> against fake device lists) over a `PulseDevices` snapshot that
> `query_devices` gets from **ffmpeg itself** (`ffmpeg -sources pulse` /
> `-sinks pulse`, which mark the server default with `*`) rather than from
> `pactl` — a deliberate, evidence-backed deviation from D7's wording that
> keeps **ffmpeg the sole external CLI** and asks the question through the
> same libavdevice backend that will open the device (CAPTURE-RESEARCH §4.5).
> `mic` → the default non-monitor source, `system` → the default sink's
> `.monitor`, `both` → **two `-f pulse` inputs mixed into one Opus track**
> (`amix=inputs=2:duration=longest:normalize=0`, §4.4's own answer; `encode::
> mix_arguments` carries the reasoning). Four new `capture.toml` knobs
> (`audio`, `audio-mic-source`, `audio-system-source`, `audio-offset`), a
> `--audio none` spelling so a keybind can override a config that turned it
> on, and the app window's Record tab audio picker is wired for real (it now
> also *starts* on the config's `audio` value). **Nothing about a missing
> device fails a recording**: `plan_audio` degrades — no mic for `--audio
> both` records the system half with a warning; nothing at all records video
> only — and the warning reaches the user as a notice toast
> (`dbus::DaemonEvent::Warning`, the first event in that enum with no D-Bus
> signal beside it, because nothing failed).
> **Two pre-existing timing defects this stage found by measuring A/V sync,
> both fixed** (§4.5): the rawvideo input had no `-framerate`, so its time
> base was 25 Hz and **every recording since Stage 11 was effectively 25 fps**
> with PTS snapped to a 40 ms grid (`encode::RAWVIDEO_TIMEBASE_HZ` now
> declares a 1 ms time base — 60 fps in, 24.8 fps out became 66.2 fps out);
> and a damage-driven cast of a still screen ended its video stream at 0.04 s,
> which `-shortest` then used to truncate the **audio** to 0.04 s as well
> (`modules::recorder::seal_last_frame` re-writes the last frame once at stop,
> so a recording's video is as long as the recording — a 7.1 s idle capture
> went from 0.048 s to 8.28 s).
> **A/V sync, measured** (transcripts in §4.5 and the Stage 13 handoff): the
> §4.3 start-offset is **−37 ms and constant to ±1 ms** over four runs, so
> D7's `pipe:3` escalation trigger ("escalate if the residual is not
> constant") is **not met** and `EncoderSink` still has no `write_audio`; the
> end-to-end clap test measures **audio ahead of video by +141 ms**, which is
> *path* latency (a video frame is timestamped when its bytes reach ffmpeg,
> after the compositor/PipeWire/channel/pipe; pulse reports 0 µs of latency
> for the capture stream, so the audio is not back-dated).
> `audio::DEFAULT_SYNC_OFFSET = 0.13` corrects the mean, residual +25…+53 ms.
> **It drifts within a recording** (+141 ms at t=2 s → +213 ms at t=14 s) and
> no constant can fix that — the real fix is taking the video PTS from the
> compositor's own SPA meta header instead of from arrival, which changes D6's
> model and is recorded as open work, not attempted here.
> Live-verified 2026-08-09 (isolated `XDG_DATA_HOME`/`save-dir`, a scratch
> null sink for the sync measurement, unloaded afterwards): `mic` recorded a
> valid Opus track (artifact deleted immediately), `both` produced the two
> `-itsoffset 0.13 -f pulse` inputs plus the `amix` graph and one mixed track,
> a bogus `audio-mic-source` warned and fell back to the default mic, a daemon
> with `PULSE_SERVER` pointed at nothing recorded **video only** with the
> warning toast's own layer surface appearing and then unmapping
> (`niri msg layers` 1 → 2 → 1 — the screen locked mid-session, so the card
> was verified by its surface rather than photographed), and **unloading the
> sink whose monitor was being recorded, mid-recording, did not kill
> anything** (the pulse shim rerouted; 7.28 s of continuous audio). **One real
> bug this stage's own live testing caught**: `audio-offset = 0.0` was omitted
> from the D-Bus option map as "no value", and the daemon defaults an absent
> key to 0.13 — so the knob could not be turned *off* at all. A zero here is a
> decision, not an absence, and it is now always sent.
> **Stage 14 makes `window edit <path>` a real annotation editor.**
> `src/modules/editor.rs` replaces Stage 9's read-only stub with
> `EditorModel` (pure data + functions, no `Theme`, unit-tested) — crop
> (drag + confirm), arrow, rectangle, ellipse and freehand tools; select/
> move/delete of placed annotations (**not** resize-after-placement — PLAN.md's
> task list names select/move/delete only, so a selected shape can be moved
> and deleted but not dragged bigger or smaller); undo/redo as whole-document
> snapshots (`Arc<capture::Frame>` clones cheaply, so a snapshot is O(1)
> except across a crop); and the restrained terracotta/ink/ivory color set at
> three stroke-width presets. Every annotation is stored as vector data
> ([`Shape`]) and painted **twice**, deliberately: an interactive
> `iced::widget::canvas::Program` (GPU vector geometry, redrawn live, never
> touching the base image's pixels) for on-screen editing, and a hand-rolled
> software rasterizer (`paint_arrow`/`paint_rect_outline`/
> `paint_ellipse_outline`/`paint_polyline`, each a free function over
> `&mut [u8]`) that composes the real save — the split exists because iced
> 0.14 has no supported way to extract owned RGBA bytes from a live `canvas`
> render, and because it keeps the tested surface (raster) fully independent
> of a live renderer. `storage.rs` gained two small exposures for this reuse
> rather than duplication: `pub fn encode_frame` (the same WebP/PNG encoder
> selection `save_capture` already made, callable directly) and `write_atomically`
> made `pub`. Save/Save As write straight to an explicit path (Save As is a
> path text field — no portal FileChooser, per Boundaries) rather than going
> through `save_capture`'s directory-resolution/filename-invention/history-
> index pipeline, which doesn't apply to editing a file that already exists;
> Copy always encodes PNG and hands it to a detached `clipboard-serve` helper,
> matching `storage.rs`'s own "the clipboard always gets PNG" rule and its
> reasoning for why a copy's owner must outlive the process that requested it.
> The editor window is now **resizable**, sized independently from the Main
> tab's fixed footprint (`modules::app::editor_window_size`) — a screenshot-
> sized canvas needs real room. **418 tests pass** (Stage 13: 373); no new
> dependency (`iced`'s `canvas` feature was already enabled, anticipating this
> stage). **GUI interaction (drag-to-draw, select/move, the canvas painter's
> visual output) was not live-tested** — this crate's live-testing recipe has
> no safe way to drive a plain `iced::application` window's pointer/keyboard
> without a human present, and the task that landed this stage was explicitly
> scoped to defer that; see the Stage 14 handoff for exactly what a human
> should check. Two small saola-theme gaps found and locally named, no tag
> bump (same posture as Stages 6/7): no token for the Crop tool's live
> dimming color (`modules::editor::draw_crop_dimming` — renamed
> `draw_region_dimming` in Stage 15 when Blur/Pixelate reused the same
> function for their own drag preview, same doc comment's reasoning — uses a
> plain black-at-low-alpha rather than `scrims.capture`, which is specified
> for the full-output capture overlay, not a windowed canvas) and no
> stroke-width token (`StrokeWidth::pixels`, three named literals, the same
> posture `modules::overlay`'s `HANDLE_RADIUS` already set).
> **Stage 15 finishes the annotation editor: text, numbered steps, and
> destructive blur/pixelate redaction.** `Tool`/`Shape` grow four cases
> (`Text`, `Step`, and the two region tools `Blur`/`Pixelate`, which are
> tools but — see below — **not** `Shape` variants) over Stage 14's
> foundation, following that stage's own "one more `Shape` variant, one more
> pair of painters" playbook. **Text**: click places an empty, selected
> caption (`Shape::Text { position, content, size }`, top-left-anchored);
> the toolbar's content field and a `TextSizeStop::{Small,Medium,Large}`
> segmented control (three real `saola_theme::Theme::typography.size`
> stops — `body`/`section_heading`/`screen_title` — resolved once into a
> plain `f32` at load, exactly like `ColorPalette` already resolves colors)
> edit it live; content/size edits don't push undo, the same posture
> `set_color`/`set_width` already established. **Step**: click drops a
> fixed terracotta-disc/ivory-numeral badge (`STEP_BADGE_RADIUS`, a named
> literal, not a token — same posture as `HANDLE_RADIUS`), numbered from a
> monotonic `next_step` counter that never renumbers on delete (explicit
> over clever). Real glyph rendering for both — the raster half's actual
> new capability — comes from `cosmic-text` (`raster_text`,
> `font_resources()`'s process-lifetime `FontSystem`/`SwashCache` pair): a
> **zero-net-new-crate** pick (already resolved transitively at exactly this
> version via `iced_wgpu`'s own text pipeline; full essay in `Cargo.toml`)
> that shapes `saola-theme`'s `typography.family_ui` ("IBM Plex Sans") and
> blends each glyph's per-pixel coverage through the same `blend_pixel` the
> Stage 14 shapes already use. The *interactive* half needs no such
> dependency — `canvas::Frame::fill_text` already goes through iced's own
> text pipeline — which is why the two-painter split (Architecture) keeps
> paying for itself: only the raster path had a real gap to fill.
> **Blur/Pixelate are region tools, not annotations**: a drag commits
> *immediately* on release (no Crop-style Apply step — see
> `EditorModel::apply_redaction`'s doc comment for exactly why that
> asymmetry is deliberate) by replacing the affected rectangle's pixels
> **in `self.canvas` itself** via a new pure kernel
> (`pixelate_region`/`blur_region`, `src/modules/editor.rs`) and pushing one
> undo snapshot — satisfying PLAN.md's task 3 literally: "stay editable
> in-session via the undo stack" is the *only* in-session reversibility
> promised, and it's what a `Snapshot` already gives every other action.
> **The redaction promise itself is now real, not just stated** (Boundaries'
> "blur/pixelate must be irreversible in exported files"): because the
> kernel replaces pixels in the live canvas rather than compositing a
> translucent shape on top of it, the original content in that rectangle
> exists nowhere after the mutation except in an in-memory `Snapshot` on the
> undo stack — never written to disk, gone when the process exits or the
> stack is pushed past. `pixelate_region` mosaics each block to its average
> color; `blur_region` is a two-pass **separable box blur using an `O(1)`-
> per-pixel sliding window** (`box_blur_axis` — the window's running sum is
> updated by one subtraction and one addition per pixel, so a larger
> `BLUR_RADIUS_PX` costs more padding, not more per-pixel work), sourced
> from a padded copy of the region so edge pixels blend with real
> neighboring content rather than a synthetic clamp at the drag rectangle's
> own boundary. **Kernel performance, measured on this machine**: pixelate
> is fast at any realistic scale (600×400 region on a 4K canvas: 377 µs
> release / 10 ms dev); blur's typical cost is small (600×400: 7.8 ms
> release / 121 ms dev) but its *worst case* — dragging Blur across an
> entire 4K canvas, which nothing stops a user from doing — is real:
> **361 ms in a `--release` build, 4.39 s in the dev build** `cargo run`
> actually uses (Cargo.toml's own `[profile.dev.package."*"] opt-level = 3`
> override optimizes *dependencies* but deliberately leaves this crate's own
> code, including `blur_region`, at opt-level 0 — the same class of gap that
> essay documents finding for WebP/PNG encoding in Stage 1). **This runs
> synchronously on `EditorState::update`'s call stack**, not inside
> `run_blocking`/`spawn_blocking` the way Save/Copy's `compose` call is — a
> worst-case whole-canvas Blur drag would visibly stall the app window's
> event loop for multiple seconds in a dev build. Recorded as open work for
> whoever next touches this path, not fixed here: `EditorModel::released`
> already returns a plain `bool` synchronously (mirrored by every other
> `EditorModel` mutator, which is what keeps the whole model "pure data plus
> pure functions, unit-tested" per this file's own testing rule), and moving
> just the redaction kernel off that path without breaking that contract
> needs real design, not a quick patch. **Export panel** (PLAN.md task 2):
> the footer grew a WebP/PNG format picker (`EditorState::export_format`,
> independent of the loaded `capture.toml` default from that point on) and a
> free-text quality field (`export_quality_text`, parsed by `parse_quality`
> — clamped `1..=100`, garbage falls back to the config default, the same
> "bad knob → warned default" posture `config.rs` already uses) plus a
> fourth action button, **Save & Copy** (`Message::SaveAndCopyPressed`) —
> "copy vs save vs both" turned out to need no new mode/state machine, just
> a third button that runs Save then Copy in one `Task`. The format picker
> governs Save/Save As only (an explicit `.png`/`.webp` extension typed into
> Save As still wins outright, unchanged from Stage 14) — **Copy is still
> hardcoded PNG regardless of it**, `copy_composed`'s own doc comment now
> says so explicitly, preserving `storage.rs`'s "the clipboard always gets
> PNG" rule byte for byte. **445 tests pass** (Stage 14: 418; +27, mostly
> the redaction kernels tested on synthetic buffers — uniform-region
> no-op, sharp-edge smoothing, "only pixels inside the rect change",
> every boundary/degenerate case — plus the Text/Step model tests and
> `parse_quality`). **`raster_text` is deliberately tested for
> not-panicking only, never for exact glyph output** — real glyph shapes
> depend on which fonts are actually installed on whatever machine runs
> `cargo test`, which this suite has no control over and shouldn't assert
> against; see the module doc comment. **GUI interaction (typing a caption,
> watching a Step badge render, dragging out a Blur/Pixelate rectangle and
> watching the image actually blur) was not live-tested**, same unavoidable
> reason as Stage 14 (no safe way to drive a plain `iced::application`
> window's pointer/keyboard without Jordan present) — see the Stage 15
> handoff for exactly what a human should check, now including "does the
> font actually render as IBM Plex Sans, or as whatever fontconfig fell back
> to" as a new item Stage 14's checklist didn't have. Two more saola-theme
> gaps found and locally named, no tag bump (same posture as Stages 6/7/14):
> no size token for the export panel's quality field (`QUALITY_FIELD_WIDTH`)
> and no size/ratio tokens for the Step badge (`STEP_BADGE_RADIUS`,
> `STEP_BADGE_FONT_RATIO`) — all three are drawing/layout parameters in the
> same category `StrokeWidth::pixels` and `HANDLE_RADIUS` already
> established as "not a design system's concern."
> **Stage 16 lands the history library, real `PickColor`, and GIF/animated-
> WebP export — no stub methods remain anywhere in `io.saola.Capture1`.**
> **`PickColor` is real**: `dbus.rs::CaptureService::pick_color` now calls
> `src/modules/picker.rs`, which proxies niri's own
> `org.gnome.Shell.Screenshot.PickColor`, copies the resulting hex to the
> clipboard (`storage::copy_text_to_clipboard`, a new sibling of the
> existing PNG-only clipboard functions — Wayland text, not an image), and
> raises a swatch toast (`modules::toast::ToastKind::Swatch`: a tile painted
> the picked color itself, body text in the design system's mono family per
> the style brief). **A real quirk this stage's own research corrected**:
> niri's `PickColor` does **not** return a bare `(ddd)` the way `dbus.rs`'s
> old stub doc comment assumed — `busctl --user introspect
> org.gnome.Shell.Screenshot /org/gnome/Shell/Screenshot` (read-only,
> live-verified against Jordan's real niri session) shows `PickColor`'s real
> signature is `a{sv}`, a one-entry dict with the triple under a `"color"`
> key; `modules::picker::extract_rgb` is what unwraps it. This crate's own
> `io.saola.Capture1::PickColor() -> (ddd)` is unaffected — live-verified via
> `busctl --user introspect io.saola.Capture1 /io/saola/Capture1` against an
> isolated test daemon — that is a different interface with a signature this
> crate chose deliberately, not a copy of niri's. **The interactive grab
> itself was never triggered live** (this stage's own constraints forbid
> it — no human present to click or cancel niri's real pointer grab);
> verified instead by `modules::picker`'s unit tests (hex formatting,
> `a{sv}` decoding against synthetic replies) and the two read-only
> introspections above. `src/modules/history.rs` is the browsable capture
> library — screenshots from `storage.rs`'s JSONL index, recordings found by
> scanning the save directory for `Recording_*.{mkv,mp4}` (no index row for
> recordings exists, and this stage deliberately keeps it that way — see the
> module's own doc comment for the full reasoning it recorded) — rendered as
> a scrollable list of thumbnailed rows (no flex-wrap widget is in this
> crate's dependency set, so "grid" is a list of already-thumbnailed cards,
> not a wrapped multi-column layout) inside a **new in-app `ViewState::
> History`** on the app window (`modules::app`'s own doc comment now
> documents this as a deliberate, narrow exception to "no navigation between
> views" — history has no separate-document lifetime the way an edit
> target does). Actions: Open/Edit (screenshots) or Show in folder
> (recordings, since the editor still has no video support), Copy
> (screenshots only — WebP entries decode-then-PNG-reencode through
> `image`'s already-enabled WebP decode feature, no new dependency), Show in
> folder, and Delete with a two-step inline confirm ("Delete this capture?
> This can't be undone." — wording carries the severity, no red, per Design
> language). **Deletion never rewrites `history.jsonl`**: the index stays
> append-only exactly as documented; a deleted screenshot's row is filtered
> out on every future load the same way a row whose file went missing any
> other way already was. **GIF and animated-WebP export**
> (`src/encode/export.rs`, a new sibling of `ffmpeg_cli.rs` — a **batch**
> ffmpeg job over an existing file via `Command::output()`, not the
> streaming `EncoderSink` trait) is a per-recording action on the History
> screen: GIF runs ffmpeg's documented two-pass `palettegen`/`paletteuse`
> recipe, animated WebP runs ffmpeg's one-pass `libwebp_anim` muxer (not the
> vendored `webp` crate, which is still-image-only) — both resampled to a
> fixed 12 fps and reported back with the exported file's real size. The
> size-warning teaching note PLAN.md's task 3 asks for is a persistent line
> above the list whenever any recording is present: exporting stores every
> frame independently, so the result can be much larger than the source
> recording. **Zero new dependencies this stage** — `image`'s WebP decode,
> `serde_json`, and ffmpeg (already the sole external CLI) cover everything;
> `Cargo.lock` is unchanged apart from this crate's own version. **445 tests
> at Stage 15 → 478 at Stage 16** (+33: `modules::picker`'s hex/decode
> tests, `modules::history`'s merge/scan/delete/format tests,
> `encode::export`'s filename-collision and argument-builder tests,
> `storage.rs`'s new JSONL-reader tests). **Live-verified against the real
> session, 2026-08-17**: an isolated test daemon (`XDG_DATA_HOME`/
> `--config-dir`/`save-dir` all in a scratch dir, teardown-checked — bus
> name released, both processes gone) took a real `shot --fullscreen` and
> produced a correct history row; `saola-capture window` booted against that
> same daemon and rendered the Main tab's new "History"/"Pick Color" buttons
> correctly (a `grim` capture confirmed the layout, styling and both new
> buttons visually — see the Stage 16 handoff for the image). **Not live-
> tested**: actually clicking History/Pick Color/any row action (this
> stage's own scope note: synthetic input against a plain toplevel window
> was not exercised, matching Stage 9/14/15's "no safe way to drive a plain
> `iced::application` window's pointer/keyboard without Jordan present"
> posture, and PickColor's own interactive grab is explicitly off-limits per
> this stage's constraints); a real GIF/WebP export was not run against a
> real recording (would need one to exist first — reasoned through and
> argument-tested, not executed). See the Stage 16 handoff for the full
> human-check list.
> PLAN.md is the staged build plan. Sections marked *(pending Stage N)* fill
> in as later stages land.
>
> **2026-08-08 amendment (decided with Jordan)**: config migrated KDL → TOML
> (`capture.toml`) in Stage 4, which is why PLAN.md's original Stages 4–18
> are numbered 5–19. Documents written before the amendment — CAPTURE-RESEARCH
> §8's stage pointers, handoffs 1–3 — use the old numbering: add 1 to any
> stage reference ≥ 4. This file's references are current.
>
> Stage 10's clang prerequisite is **cleared and now spent** — Jordan
> installed clang 22.1.8 (2026-08-08, verified live), and Stage 10 confirmed
> `pipewire` 0.10 builds here end to end (see CAPTURE-RESEARCH §2.0/D5 and
> the `pipewire` survey in `Cargo.toml`).

## Commands

```sh
cargo build
cargo clippy --all-targets -- -D warnings   # warnings are errors, keep green
cargo test
cargo fmt --check

cargo run -- daemon                # the long-running daemon (surfaceless as of Stage 3)
cargo run -- shot --fullscreen     # what the Print keybind invokes
cargo run -- shot --region         # interactive: maps Stage 7's selection overlay, blocks
                                   # until the user confirms; exits 1 on cancel
cargo run -- shot --region --geometry 600x450+100+100  # scriptable, skips the overlay
cargo run -- shot --window         # the focused window (no picker — CAPTURE-RESEARCH D3)
cargo run -- shot --window --window-id 42  # scriptable: an explicit niri-ipc window id
cargo run -- shot --fullscreen --delay 3   # any shot kind: countdown pill, then flash+toast
cargo run -- shot --fullscreen --no-daemon --format=webp --output=/tmp  # headless/scriptable
cargo run -- record start|stop|toggle [--preset hevc|av1|h264] [--audio none|mic|system|both]
                                   # real as of Stage 11, for the focused output. `start` returns
                                   # once the recording is live; `stop` blocks until ffmpeg has
                                   # flushed and prints the saved path (~0.2 s, measured);
                                   # `toggle` reads the daemon's `Recording` property first.
                                   # `--audio` is **real as of Stage 13** (Opus, one track);
                                   # it defaults to `capture.toml`'s `audio` knob, and `none` is
                                   # how a keybind overrides a config that turned audio on. A
                                   # device that isn't there degrades the recording to video-only
                                   # with a warning toast — it never fails the recording.
cargo run -- record start --region         # Stage 12: interactive — reuses the same overlay
                                           # `shot --region` maps; still casts the whole monitor
                                           # and crops in ffmpeg (CAPTURE-RESEARCH D8, no `RecordArea`)
cargo run -- record start --region --geometry 640x480+100+100  # scriptable, skips the overlay
cargo run -- record start --window         # Stage 12: the focused window, no crop
cargo run -- record start --window --window-id 5  # scriptable: an explicit niri-ipc window id
SAOLA_CAPTURE_FFMPEG_LOGLEVEL=verbose cargo run -- daemon   # the daemon's ffmpeg children run at
                                   # -loglevel warning by default; `verbose` is what prints the
                                   # `Using VAAPI entrypoint VAEntrypointEncSlice (6).` line that
                                   # *proves* a recording was hardware-encoded (CAPTURE-RESEARCH §3.1)
cargo run -- record start --dry-run          # Stage 10: negotiate a real screencast, log the
                                             # SPA format + 5 s of frame cadence, write NOTHING,
                                             # tear down. Never contacts the daemon.
cargo run -- record start --dry-run --window-id 16  # cast one window instead of the focused output
                                                    # (dry-run's own window_id, independent of
                                                    # the real --window flag above — see cli.rs)
cargo run -- pick-color             # real as of Stage 16 — blocks until you click (or Escape,
                                    # unverified — see the Stage 16 handoff), prints #RRGGBB,
                                    # copies it to the clipboard, and shows a swatch toast
cargo run -- open                  # raises the app window (spawns it detached — Stage 9)
cargo run -- window                # the app window process — Screenshot/Record tabs, real as of
                                   # Stage 9; Stage 16 adds a "History"/"Pick Color" row under them
cargo run -- window edit <path>    # boots straight into the real annotation editor (Stage 14,
                                   # text/step/blur/pixelate added Stage 15) — crop/arrow/rectangle/
                                   # ellipse/freehand/text/step/blur/pixelate, undo/redo, an export
                                   # panel (format/quality), Save/Save As/Copy/Save & Copy
cargo run -- --config-dir ~/scratch shot --fullscreen  # capture.toml from an alternate dir
```

**Stage 16's History screen** is reached from the app window's Main tab
("History" button), not a separate CLI verb or `window` subcommand — it's
in-app navigation within the already-running `window` process
(`modules::app::ViewState::History`), not a spawn. It shows every saved
screenshot (from `history.jsonl`) and recording (found by scanning the save
directory for `Recording_*.{mkv,mp4}` — see `src/modules/history.rs`'s own
doc comment for why that's a directory scan and not an index-schema bump),
with Open/Edit, Copy (screenshots only), Show in folder, Delete (two-step
confirm) and, per recording, Export GIF/Export WebP
(`src/encode/export.rs`, ffmpeg's two-pass `palettegen`/`paletteuse` for
GIF, one-pass `libwebp_anim` for animated WebP).

There is also a **hidden** verb, `saola-capture clipboard-serve --mime
image/png`, which reads bytes on stdin and serves them as the Wayland
selection until something else claims it. It is an implementation detail of
`--no-daemon` captures (a Wayland "copy" needs a live process to answer paste
requests, and a CLI verb exits immediately) — spawned detached by
`storage.rs`, never typed by hand, hidden from `--help`.

`record start --dry-run` is the second exception to "every verb is a daemon
client" (after `--no-daemon` shots): it talks straight to niri's
`org.gnome.Mutter.ScreenCast` and to PipeWire, in-process, and never looks
the daemon up — see `capture::screencast::dry_run`'s doc comment for the
three reasons. That makes it safe to run while Jordan's real daemon (and a
real recording, once Stage 11 lands) is up.

Every CLI verb above except `--no-daemon` shots and `record --dry-run` is a
real D-Bus client of the daemon as of Stage 3 (auto-spawning it detached, retrying once, if the bus
name is unowned) — `busctl --user introspect io.saola.Capture1
/io/saola/Capture1` shows the live interface once a daemon is running. As of
Stage 5 `Screenshot` is real, as of Stage 9 `OpenWindow`, as of Stage 11
`StartRecording`/`StopRecording`, and **as of Stage 16 `PickColor`** — no
stub methods remain on `io.saola.Capture1`. Stage 11 also added the interface's
one **property**, `Recording b` (read-only, true while anything is starting,
recording *or* stopping) — an additive extension, not a change to the frozen
signal contract, and the only way `record toggle` can choose a branch without
parsing an error string. Nothing emits `PropertiesChanged` for it yet.
**Stage 12's tray item turned out not to need it**: `modules::tray` runs
*inside* the daemon process, so it shares the same in-process
`dbus::SharedRecorder` `CaptureService` holds (a clone of the same `Arc`)
rather than reading `Recording` over D-Bus at all — a genuine external
consumer of the property (a future saola-notifications, `busctl --user
monitor`) is still the reason it would want `PropertiesChanged`, and that
remains unbuilt.

**The tray item is a served object on the same connection**, not a fourth
run mode (Stage 12): `busctl --user introspect io.saola.Capture1
/StatusNotifierItem` and `.../StatusNotifierMenu` show it once a daemon is
running, whether or not any host (`saola-panel`) is watching —
`org.kde.StatusNotifierWatcher`'s own `RegisterStatusNotifierItem` is
best-effort and retried on watcher-appearance, never blocking daemon boot.

**`Screenshot` can block for minutes, on purpose** (Stage 7): an interactive
`region` call does not return until the user confirms or cancels — the same
contract `slurp` has. Nothing times it out (`zbus::Connection`'s
`method_timeout` defaults to `None`) and nothing else on the bus is blocked
meanwhile (zbus dispatches each call on its own task). A cancel is a D-Bus
error (`the region selection was cancelled`), so `shot --region` exits **1**
with nothing saved. A second `shot --region` while one is up is refused
immediately (`a region selection is already in progress`) rather than
stacking a second exclusive-keyboard surface.

**`StopRecording` blocks too, for the same reason and with the same
safety** (Stage 11): it does not return until ffmpeg has flushed and the cast
is torn down, so its reply *is* the saved path. Measured at ~0.22 s for a
10 s Matroska recording; an MP4 is slower because `+faststart` rewrites the
whole file, which is why `FINISH_WAIT` is 30 s before the SIGINT escalation.
`StartRecording`, by contrast, returns as soon as the recording is **live**
(negotiated, ffmpeg spawned, first frame written) — ~0.33 s measured, most of
it the VAAPI device probe on a cold daemon.

Live-testing anything that maps overlay surfaces or grabs the keyboard
happens in a **nested niri** (see Conventions), never the real session.
Booting the daemon itself is safe in the real session — the daemon maps real
surfaces (the flash, permanently; the toast, while captures are recent) but
neither grabs the keyboard (`KeyboardInteractivity::None` on both — see
`main.rs`'s `flash_surface_settings`/`toast_surface_settings`). **Stage 7's
region overlay is the exception and the one thing that must never be
exercised outside a nested niri**: `overlay_surface_settings` asks for
`KeyboardInteractivity::Exclusive`, so a bug that leaves it mapped takes the
keyboard with it. Booting the daemon is still safe; running
`shot --region` against the real session is not, until Jordan is driving it
himself. **Stage 8's countdown pill joins the flash/toast side of that
line, not the overlay's**: `countdown_surface_settings` asks for
`KeyboardInteractivity::None` and `events_transparent: true`, exactly like
the flash, so `shot --fullscreen --delay N` (or `--region`/`--window` with a
delay) is safe to trigger in the real session — live-verified for the
capture half via `--no-daemon --window` (real niri, read-only, no synthetic
input; see the Stage 8 handoff for what that run caught and fixed). A
`--window` shot — with or without `--window-id`, through the daemon or
`--no-daemon` — never maps a surface at all and is equally safe.
**Stage 10's `record start --dry-run` is on the safe side of that line too,
and it has to be**: a nested niri started *without* `--session` does **not
serve `org.gnome.Mutter.ScreenCast` at all** (verified live in Stage 10 —
`busctl --user list` on the private bus shows nothing from the nested
compositor, and the dry run fails with `ServiceUnknown`), so the nested-niri
recipe simply cannot exercise the recording path. Testing it means the real
session, which is what the Stage 2 probes already did for the same
interface: the dry run maps no surface, grabs no keyboard, injects no input,
writes no file, never touches the daemon or the clipboard, and its teardown
was verified (`niri msg casts` → "No screencasts", no leftover PipeWire
node). It does briefly cast the screen into memory and discard it.
**Stage 11's real recordings sit on the same side of that line, with one
addition: they write files and spawn a child process.** A recording maps no
surface, grabs no keyboard and injects no input, so nested niri is still both
impossible (no ScreenCast there) and unnecessary — but the isolation that
matters shifts from "don't touch the display" to "don't touch Jordan's data".
The recipe Stage 11 used, and the one to copy: check `busctl --user list |
grep io.saola.Capture1` **first** — if a real daemon owns the name, do not
kill it, and note that a private `dbus-daemon` bus is *not* an escape hatch
here, because niri serves `org.gnome.Mutter.ScreenCast` on the session bus
and the daemon reaches it over that same `Connection`, so isolating the bus
isolates the cast away too. With the name unowned, run the test daemon on the
real session bus with `XDG_DATA_HOME` pointed at a temp dir and a
`--config-dir` whose `capture.toml` sets `save-dir` to a temp dir (the
**daemon's** environment decides where files land, but `save-dir` travels
from the *CLI's* config over the bus — see `RecordOptions::to_dbus_options`).
Teardown checklist grows one item: `niri msg casts` → "No screencasts",
`pw-dump` shows no `Video/Source` beyond the webcams, **and `pgrep -x ffmpeg`
is empty**.

## Architecture

PLAN.md's Architecture section is binding; read it first. Summary:

- **Process split is forced by the toolkit**: iced_layershell's daemon hosts
  layer-shell surfaces only, so the app window/editor is a separate plain
  iced process. D-Bus (`io.saola.Capture1`) is the seam between CLI, window
  process, keybinds, panel tray, and daemon.
- **Two trait boundaries — new paths go behind them, never around them**:
  - `CaptureBackend` (`src/capture/mod.rs`): screenshots via
    `zwlr_screencopy_v1` (`screencopy.rs`), video via niri's
    `org.gnome.Mutter.ScreenCast` v4 + a PipeWire stream on a dedicated
    thread (`screencast.rs`). Future compositor/portal portability lives
    here and nowhere else.
  - `EncoderSink` (`src/encode/mod.rs`): ffmpeg CLI (`ffmpeg_cli.rs`,
    rawvideo on stdin, `hevc_vaapi`→MKV primary, SVT-AV1 and H.264/MP4
    presets) is the only v0.1 implementation; the trait is what lets
    in-process encoders replace it later. **Real as of Stage 11**, with two
    deliberate deviations from PLAN.md's trait sketch, both documented at
    `encode/mod.rs`'s head: there is **no `pts` argument** on `write_video`
    (D6 pins `-use_wallclock_as_timestamps 1`, so a `pts` would be a
    parameter every implementation must ignore — a worse lie than not having
    it), and **no `write_audio`** (D7 settled audio as an `-f pulse` input
    ffmpeg opens *itself*, so no PCM ever crosses this trait; the §4.4
    escalation to PCM on `pipe:3` is the stage that should add it, with a
    real body rather than a `todo!()`). `start(...)` is an inherent
    constructor per implementation, not a trait method — a constructor cannot
    be dispatched through `dyn`.
- **Region capture freezes first**: capture the output, then map the overlay
  over the frozen frame; crop in memory. No self-capture race. **Real as of
  Stage 7**, and the sequence is fixed at exactly three hops:
  `capture::freeze_focused_output` (blocking, in the D-Bus method's
  `spawn_blocking`) → `dbus::DaemonEvent::BeginRegion` carries an
  `image::Handle` **copy** of the pixels to the iced daemon, which maps
  `modules::overlay` → the confirmed `LogicalRect` comes back down a
  capacity-1 `mpsc` reply channel and `capture::crop_frozen_frame` crops the
  **original `Frame`**, which never left the D-Bus task. There is no second
  screencopy anywhere in that chain, and there must never be: the overlay is
  mapped by then, and screencopy composites layer-shell surfaces.
- **Four surface lifecycles now exist, and a new surface picks one
  deliberately** (`main.rs`'s `SurfaceRole`): *permanent* (the flash —
  spawned at boot, never unmapped, toggles opacity; the only shape that
  survives a ~140 ms visible lifetime, see the latency gotcha below),
  *respawn-to-resize* (the toast — unmap and respawn whenever its content
  changes its height, so its input region always matches what is drawn),
  *reactive-with-Exclusive-keyboard* (the overlay — spawned on demand, torn
  down the moment the user acts), and — new in Stage 8 —
  *reactive-without-keyboard* (the countdown pill: spawned on the first
  delayed shot, torn down the instant its own clock reaches zero). Reactive
  is forced for the overlay: `Exclusive` keyboard cannot be pre-warmed at
  boot without holding the keyboard forever. Measured live, Stage 7:
  ~450–560 ms from `shot --region` starting to the overlay's first
  composited frame, nearly all of it process start plus the ~0.3 s freeze —
  survivable precisely because the overlay stays up until the user acts. The
  countdown is reactive too, but for a different reason than the overlay:
  it has real content that changes over its own lifetime (the number), so
  there is no idle state worth pre-warming at boot the way the flash's
  opacity-only content allows — and unlike the overlay it carries none of
  the keyboard risk, so (per the same latency reasoning) a `--delay 1`
  countdown is the shortest-lived reactive surface in the daemon and the
  first one worth measuring if a very short delay ever looks like it flashed
  too briefly. **The app window (Stage 9, `modules::app`) is not a fifth
  entry in this registry** — it isn't a layer-shell surface at all, has no
  `SurfaceRole`, and isn't owned by `Daemon`; it's a separate process
  running a plain `iced::application`, a normal niri toplevel. Its own
  "hide/show" (`iced::window::set_mode`) is a different mechanism from
  every lifecycle above, unrelated to `main.rs`'s surface-spawning code.
- **Recording state lives in the daemon** and survives window closes; the
  PipeWire thread never blocks on the encoder (bounded channel, drop + log).
  **Real as of Stage 10 for everything up to the encoder**
  (`capture/screencast.rs`), with three rules Stage 11+ must not rediscover:
  - **The pw main loop is a plain OS thread and everything PipeWire stays
    inside it.** `MainLoopRc`/`ContextRc`/`CoreRc`/`StreamBox` are all `Rc`
    and therefore not `Send`. Two channels cross the boundary outward
    (`std::sync::mpsc`: an unbounded control channel that must never drop,
    and a `sync_channel(4)` frame channel that drops with `try_send` +
    a counter), and exactly one crosses inward
    (`pipewire::channel::Sender`, whose receiver is attached to the loop as
    an event source — **the only correct way to poke a running pw loop from
    another thread**; calling `quit()` from outside would race a non-`Sync`
    object).
  - **Teardown order is fixed: consumer first, producer second.**
    `PipeWireStream::stop()` (quit the loop → `pw_stream_disconnect` → join)
    and only *then* `CastSession::close()` (`Session.Stop`). The other order
    makes the node vanish under a live consumer and logs a scary
    `StreamState::Error` for what was a clean stop. And **never `Stop` a
    session the compositor already closed** (CAPTURE-RESEARCH D8) —
    `CastSession::closed()` is a non-blocking poll of the retained
    `Session.Closed` signal stream, and `close()` consults it first.
  - **Subscribe before `Start`.** `PipeWireStreamAdded` fires ~immediately
    after `Start`; subscribing afterwards loses the race. `Session.Closed`
    is subscribed even earlier, because a bad `RecordWindow` id is accepted
    at call time and only self-destructs later (§5.3) — that is a
    three-way `tokio::select!` between the node id, `Closed`, and a 5 s
    timeout, and the three outcomes are deliberately different errors.
  **Stage 11 built the rest of it** (`modules/recorder.rs` + `dbus.rs`), and
  adds four rules of its own:
  - **The full teardown order is consumer → encoder → producer**, extending
    Stage 10's two-step: `PipeWireStream::stop()`, *then* `sink.finish()`
    (which closes ffmpeg's stdin and waits for the flush), *then*
    `CastSession::close()`. The first two are blocking and run on the pump's
    `spawn_blocking` task; the third is a D-Bus call and runs on the async
    supervisor that awaits it. That split is why a recording is two tasks and
    not one.
  - **Exactly one finalization site.** A recording can end without anyone
    asking (a dead encoder, a compositor-closed cast), so the supervisor —
    not `StopRecording` — is what emits the signal, raises the toast and
    returns the machine to `Idle`. A waiting `StopRecording` parks on a
    oneshot registered *under the same lock that sets the stop flag*, which
    is what stops the supervisor from finishing in between and concluding
    nobody was listening.
  - **`Starting` and `Stopping` are phases because they take real time**, and
    the guarded `Idle → Starting` transition is taken *before* any slow work.
    That one transition is the whole defence against two concurrent
    recordings. Stop-while-starting is therefore expressible (the stop is
    remembered; the start unwinds itself on arrival) and was driven live.
  - **A mid-recording renegotiation is fatal, on purpose.** ffmpeg's
    `-video_size` is fixed for a process's life and the raw pipe carries no
    framing, so a second `param_changed` at a different size would shear the
    video rather than fail. `encode::NegotiatedGuard` ends the recording with
    a stated error instead; a re-announcement of the *same* size is ignored.
- **The VAAPI device is discovered at encoder start, never hardcoded**
  (CAPTURE-RESEARCH D6's 2026-08-08 amendment, real in Stage 11):
  `ffmpeg_cli::render_nodes` enumerates `/dev/dri/renderD*` **sorted** (so
  ties are deterministic and the iGPU — which is driving the display, so its
  encoder reads the frames without a cross-device copy — keeps winning), and
  `probe_encoder` answers "can this node open this encoder?" with a **tiny
  real trial encode** (`-f lavfi -i color=…` → `hwupload` → encoder →
  `-f null -`), cached per `(device, encoder)` for the process's life. ffmpeg
  stays the sole external CLI; there is no runtime `vainfo`. Three things
  worth not rediscovering:
  - **The trial frame must be ≥130×128.** The first draft probed at 64×64 and
    *every* codec on *every* node "failed" — with
    `Hardware does not support encoding at size 64x64 (constraints: width
    130-8192 height 128-4352)`, a size rejection that is indistinguishable
    from a missing entrypoint if you only check the exit status.
    `PROBE_SIZE` is 256 and a test guards it.
  - **A trial encode answers a strictly better question than `vainfo`.**
    `vainfo` reports what the driver advertises; the probe reports what *this
    ffmpeg build, on this device, right now* can open. §3.3's AV1 case is
    exactly that gap — the driver advertises `VAProfileAV1Profile0` and the
    encode still fails, because the profile is decode-only.
  - **Measured live (2026-08-09):** one node, `/dev/dri/renderD128`;
    `av1_vaapi` → no (179 ms), `hevc_vaapi` → yes (150 ms), `h264_vaapi` →
    yes (125 ms); `hevc` resolved in 330 ms total on a cold daemon, ~0 ms
    warm. The `av1` preset therefore falls back to software `libsvtav1`,
    which is the only preset that works with **no** usable render node at
    all — `hevc`/`h264` fail there with an error naming `--preset av1` and
    the `vaapi-device` knob.
- **The annotation editor paints every shape twice, on purpose, and Stage 15
  kept doing so** (`modules::editor`, Stage 14, extended Stage 15): a placed
  annotation is vector data (`Shape`) with exactly one painter for on-screen
  interaction (`EditorCanvas`, an `iced::widget::canvas::Program` — GPU
  geometry, redrawn live, never touches the base image's pixel buffer) and a
  completely separate one for the real save (`compose` plus the `paint_*`
  free functions — a hand-rolled software rasterizer over `&mut [u8]`, run
  once at Save/Copy time). Not a stopgap: iced 0.14 has no supported way to
  extract owned RGBA bytes from a live `canvas` render mid-frame, and even if
  it did, a GPU round trip is the wrong tool for "encode these exact bytes to
  WebP." The two painters must stay visually consistent but never need to be
  pixel-identical — only the raster half is unit-tested (AGENTS.md's own
  "pure data + functions, unit-tested" testing rule), the interactive half is
  GUI and deferred to human-verify. **Stage 15's `Text`/`Step` tools proved
  the playbook exactly as predicted** — one more `Shape` variant each, one
  more arm in every exhaustive `match` (`shape_bounds`/`shape_hit`/
  `translate_shape`/`paint_shape`/`draw_shape`), no redesign — with one
  addition neither prior stage needed: real glyph rendering, which is why
  `cosmic-text` entered the dependency tree (`raster_text`, the raster side;
  `canvas::Frame::fill_text`, already free on the interactive side, needed
  no new dependency at all). **`Blur`/`Pixelate` are `Tool`s, deliberately
  not `Shape`s** — see the Stage 15 status paragraph above and
  `EditorModel::apply_redaction`'s doc comment for why a destructive
  region-pixel mutation (committed straight into `self.canvas`, no
  composited-on-top annotation) is what AGENTS.md's Boundaries "redaction
  must be irreversible in exported files" promise actually requires, and why
  that made this the one Stage 15 tool that *didn't* fit the "one more Shape
  variant" pattern.
- **Two iced_layershell surface gotchas, found live in Stage 6 and binding on
  every future surface (the region overlay, recording chip, tray popovers if
  any land here):**
  - **The app-wide surface background must be set transparent explicitly**
    (`.style(Daemon::style)` in `run_daemon`, returning
    `iced::theme::Style { background_color: Color::TRANSPARENT, .. }`,
    copied from `saola-panel::main::Panel::style`). Without it, iced clears
    every surface to `to_iced_theme`'s `background` (`palette.ink`) before
    drawing anything, so a surface that doesn't cover 100% of its own area
    with an explicit style shows opaque ink through the gaps — invisible on
    a surface that's mapped-and-torn-down within a couple hundred
    milliseconds (which is why Stage 5's surfaceless daemon and Stage 6's
    first flash draft never revealed it), but a permanent, obvious solid-ink
    rectangle on any surface that stays mapped. Live-verified with `grim` +
    pixel sampling in nested niri; see the Stage 6 handoff for the exact
    repro.
  - **A layer-shell surface spawned reactively (on the triggering event) can
    lose its entire visible window to Wayland/GPU setup latency** — the
    chain from "an event arrives" to "a pixel is composited" crosses several
    scheduler hops (D-Bus/channel forwarding, iced's message queue,
    `NewLayerShell`, the compositor's configure round trip, the first GPU
    frame), and a surface whose *whole lifetime* is short (Stage 6's flash,
    at ~140 ms) can be torn down before any of that finishes — live-verified
    in nested niri: ten consecutive `grim` captures immediately after a
    completed `shot --fullscreen`, zero showing the flash, with the exact
    same code rendering correctly once given 5 s to work with. **Fix used
    for the flash**: spawn once, at daemon boot, and never tear down —
    toggle opacity/visibility instead of the surface's existence
    (`Daemon::boot`, `SurfaceRole::Flash`). This only works for a surface
    that's harmless to leave mapped indefinitely (click-through, invisible
    at rest, no keyboard) — the toast (needs real input) and the future
    region overlay (needs `Exclusive` keyboard) can't use the same trick and
    must find their own answer to "is this surface reliably visible in time"
    if it becomes a problem for them too.
- `docs/CAPTURE-RESEARCH.md` is the evidence of record for every capture-path
  decision (shm vs dmabuf, window-capture mechanism, audio transport,
  verified ffmpeg command lines), with raw transcripts and probe sources in
  `docs/research/2026-08-08-stage2/`. **Its §8 decision list is binding on
  Stages 5, 7, 8 and 10–13 (its own text says 4, 6, 7 and 9–12 — pre-renumber
  numbering).** Do not re-litigate it from theory; extend it
  with new evidence. The decisions that most often get re-invented wrong:
  - **Recording is dmabuf-only.** shm is refused by niri's cast node at both
    the format and the buffer-allocation layer — there is no shm fallback to
    write. Request modifier `LINEAR` and mmap the fd; use `chunk->stride`
    (not `width * 4`) and ignore `maxsize`/`chunk->size`. **All of this is
    implemented and live-confirmed as of Stage 10**
    (`capture/screencast.rs`): a non-dmabuf buffer is a first-class
    `CastControl::Error`, not a fallback; the mapping length comes from
    `lseek(fd, 0, SEEK_END)`; `copy_packed_rows` is the single place the
    stride is applied and it is unit-tested; and a live `--window-id` cast
    negotiated **2507×1457 at stride 10240** (= the 2560-px *output* pitch)
    and produced a pixel-perfect, unsheared frame. `PW_STREAM_FLAG_MAP_
    BUFFERS` still does not map dmabufs — the mmap is by hand.
  - **Window screenshots go through niri-ipc `ScreenshotWindow`**, not a
    geometry crop: niri exposes no pixel position for tiled windows, so the
    crop rectangle is not computable. It also clobbers the clipboard
    unconditionally, so `storage.rs` owns the final clipboard state — real
    as of Stage 8 (`capture/screencopy.rs::ScreencopyBackend::
    capture_window`, `storage.rs`'s `else if kind == ShotKind::Window`
    branch). **Live-caught gotcha**: the IPC reply from `Action::
    ScreenshotWindow` lands before the PNG is necessarily written — a
    straight `fs::read` right after the reply intermittently raced an
    `ENOENT` against a file that appeared a few milliseconds later (caught
    against Jordan's real session; the file, once found, was complete and
    byte-valid — this was a read-too-early race, not a corrupt write).
    `read_window_screenshot_with_retry` retries only `ErrorKind::NotFound`,
    bounded at 500 ms.
  - **ffmpeg needs `-use_wallclock_as_timestamps 1 -fps_mode vfr`** on the
    rawvideo input (casts are variable-rate; without it the video plays
    fast), GPU colour conversion with the matrix pinned
    (`scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=tv`), and an
    explicit crop to even dimensions (`hevc_vaapi` silently resizes odd
    inputs). **All of it is implemented and live-confirmed as of Stage 11**
    (`encode::ffmpeg_args`/`filter_chain`), with three refinements the table
    itself doesn't state: `-fps_mode**:v**` is stream-qualified so a Stage 13
    audio input isn't caught by it; `-video_size` carries the **negotiated**
    size while the `crop` filter carries the **even** one (swapping them
    shears the video instead of failing, which is why they are separate
    concepts in `VideoSpec`); and `-y` is mandatory because ffmpeg's stdin
    *is* the video pipe, so a `-n` overwrite prompt could never be answered.
    Measured end to end: 10.01 s of wall clock → 10.12 s of Matroska, PTS
    starting at 0 and monotonic — the fast-forward §4.2 warns about is
    genuinely absent.
    **Stage 13 adds two corrections to that same command line, both found by
    measuring A/V sync and both fixing defects that were invisible in a file's
    duration** (§4.5): the rawvideo input now declares
    `-framerate 1000` — the input **time base**, immediately before the
    wallclock flag that overrides the timestamps it would otherwise generate,
    which is the only way that flag is safe — because the demuxer's default of
    25 was snapping every PTS to a **40 ms grid** and `-fps_mode:v vfr` was
    discarding whatever landed in a taken slot (a 60 fps source came out at
    24.8 fps); and the recorder re-writes the last frame once at stop
    (`modules::recorder::seal_last_frame`), because a damage-driven cast of a
    still screen ends its video stream at the last change — which, with the
    `-shortest` an infinite `-f pulse` input requires, truncated the **audio**
    to 0.048 s on a 7.1 s recording. **The remaining, unfixed one**: because a
    frame is timestamped when its bytes reach ffmpeg, the video timeline
    drifts later as encoder backlog accumulates (+141 ms of audio-ahead at
    t=2 s, +213 ms at t=14 s). No `-itsoffset` constant can correct a drift;
    the fix is to take the video PTS from the compositor's own SPA meta header
    instead of from arrival, which needs a framed transport to ffmpeg rather
    than the raw pipe — i.e. a change to D6's model, recorded rather than
    attempted.
  - **`iced_layershell` is confirmed viable for the overlay** (Exclusive
    keyboard, Escape, pixel-exact drag, frozen-frame background — all
    live-tested in nested niri). Multi-output is source-verified only, and
    **Stage 7 shipped single-output on purpose** (D10 allows it): the overlay
    maps on the focused output only, so a selection cannot cross outputs.
    `overlay_surface_settings` already takes an output name, so the
    multi-output version is a loop plus a shared coordinate space, not a
    rewrite.
  - **A screencopy buffer is the output's *framebuffer*, not what the user
    sees** — new in Stage 5, extending §1.2 (which covered only the unrelated
    `y_invert` flag, still always 0 on niri). On an output whose
    `wl_output.geometry.transform` isn't `Normal`, the captured pixels must
    have the **inverse** of that transform applied, or the screenshot comes
    out mirrored/rotated. Caught live: nested niri's winit output is
    `Flipped180` ("flipped vertically") and the first draft's captures were
    upside down versus `grim`; with the correction they are **byte-identical**
    to grim's. Jordan's eDP-1 is `Normal`, so nothing in the real session
    would ever have shown this. `capture/screencopy.rs::undo_output_transform`.
  - **`grim` is not a byte-exact oracle at fractional scale.** grim composites
    into a surface sized `logical × scale` and resamples; on a 1.5-scale output
    (`825 × 1.5 = 1237.5`) that leaves ~0.03% of pixels differing by ≤5/255 at
    edges and in gradients. Set the output to scale 1 before demanding an exact
    match. Same class of artefact §1.4 already flagged for grim's `-g` cropping.

## Design language (binding)

- `saola-theme` is consumed as a git dependency pinned to a **release tag**
  (with matching `version`), never `branch = "main"`. Bumping the tag is a
  deliberate, reviewed change.
- **Zero hardcoded colors or sizes.** Every value comes from
  `saola_theme::tokens`, every widget style from `saola_theme::style`. If a
  style is missing, add it to saola-theme (and note the tag bump) — never
  restyle locally.
- Three colors, never a fourth: ink, ivory, terracotta. Severity is carried
  by wording, not color. Layer-shell surfaces are ink; the app window is
  paper.
- The capture surfaces are **already specified** in
  `docs/SAOLA-STYLE-GUIDE.md` (verbatim copy of the design-system spec —
  if implementation disagrees with it, the implementation is wrong):
  - Overlay: `scrims.capture` outside the selection, `radii.selection`
    (6 px) on the selection rect, dashed terracotta edge, round terracotta
    handles, tabular-numeral size readout, floating toolbar (§2/§4/§7).
  - Toast: the §6 notification card (440 px ink, 26 px radius, 36 px icon
    tile, 3 px life rule) with §5 timing (350 ms in, 5 s rest, 1 s fade,
    stack of 3, hover pauses).
  - Record/stop/play are among the only **solid** icons; everything else is
    Lucide at stroke 2.75. Size/duration readouts use tabular numerals.
  - Run every new surface through §11's checklist.
- `src/icons.rs` copies saola-panel's pattern (stroke baked into assets,
  `include_bytes!`, svg tint via theme roles). Migrating icons to a shared
  saola-icons crate is **recorded debt**, not this repo's job.
- **saola-theme v0.5.0 token/style gaps found in Stage 6** (documented and
  worked around locally per the rule above's spirit — no tag bump yet, since
  each was answered by deriving from *existing* tokens rather than needing a
  genuinely new one; a future consolidated pass should still upstream them):
  - No dedicated flash/shutter motion duration — `modules::flash::fade`
    reuses `motion.hover` (140 ms).
  - `saola_theme::style::container::card(theme, Surface::Ink)` paints the
    *opposite* of what the ink notification card needs (an ivory card, not
    an ink one) — `modules::toast::ink_card_style` composes the right thing
    locally from `palette.ink`/`on_ink.primary`/`radii.card`/
    `shadows.popover`.
  - No `Sizes.icon_tile` field for the toast's 36 px icon tile, and no
    life-rule-thickness field for its 3 px terracotta rule —
    `modules::toast::ICON_TILE_SIZE`/`LIFE_RULE_HEIGHT` are the spec's
    literal values, named and documented at their one definition site.
- **saola-theme v0.5.0 gaps found in Stage 7** (same posture, still no tag
  bump). `scrim.capture`, `radii.selection`, `palette.accent`,
  `container::popover`, `container::bar_pill` and `button::rest` all existed
  and are used verbatim; `sizes.window_border` (2 px) is reused as the
  selection edge's stroke width, on the grounds that it is the system's one
  *thin decorative line* thickness. Three genuine design-token gaps, all in
  `modules::overlay`: no handle size (`HANDLE_RADIUS`), no dash pattern for
  the one dashed edge in the whole style guide (`DASH_SEGMENTS`), no width
  for a small numeric readout pill (`READOUT_WIDTH`).
  **Deliberately *not* filed as gaps**: `HANDLE_HIT_RADIUS`,
  `EDGE_SNAP_DISTANCE` and `MIN_SELECTION` describe pointer *behaviour*, not
  appearance — a design system has no opinion on how close to an edge a drag
  should snap, and upstreaming them would miscategorise interaction as style.
- **saola-theme gaps found in Stage 14** (same posture, still no tag bump),
  both in `modules::editor`: no color token for a windowed canvas's crop-tool
  dimming (`scrims.capture` is specified for the full-output capture overlay
  specifically, not this surface, so `draw_crop_dimming` uses a plain black
  at low alpha rather than reaching for a token that doesn't mean this); and
  no stroke-width scale for a drawing tool's pen (`StrokeWidth::pixels`,
  three named literals — the same "a design system has no opinion on a
  drawing tool's own parameters" reasoning `modules::overlay`'s
  `HANDLE_RADIUS` already established, not a numeric-size gap like the ones
  above).
- **saola-theme gaps found in Stage 15** (same posture, still no tag bump),
  all in `modules::editor`: no size/ratio tokens for the Step badge
  (`STEP_BADGE_RADIUS`, `STEP_BADGE_FONT_RATIO` — the same drawing-tool-
  parameter category `StrokeWidth::pixels` already established, not a
  numeric-size gap) and no width token for the export panel's small quality
  `text_input` (`QUALITY_FIELD_WIDTH` — a one-off layout parameter, not a
  reusable size scale value). **Not a gap, worth noting as the positive
  case**: the Text tool's three size stops (`TextSizeStop::{Small,Medium,
  Large}`) are *not* a new literal scale — they resolve to three existing
  `saola_theme::Theme::typography.size` entries (`body`/`section_heading`/
  `screen_title`), the same "reuse an existing token rather than inventing
  a parallel scale" instinct the format/color pickers already followed.
- **saola-theme gap found in the 2026-08-09 UI-polish pass** (same posture,
  still no tag bump): v0.5.0 ships `style::segmented::track`/`segment` but no
  *geometry* to go with them — no gap between adjacent segments, no inset of
  the row from the track's edge. `modules::app::SEGMENT_INSET` (4.0, shared
  by `modules::editor`'s twin `segmented_row` via a `pub(super)` re-use, so
  the two surfaces cannot drift) is that value, and it is **not invented
  here**: it is what saola-theme's own reference usage of those two helpers
  does (`examples/gallery/main.rs`: `container(row(segments).spacing(4))
  .style(track).padding(4)`). Worth upstreaming as a token in the same
  consolidated pass as the gaps above, since every consumer of a segmented
  control will otherwise re-derive it. **Why it is not cosmetic**: every
  segment is a full `radii.pill`, so at zero spacing adjacent pills' rounded
  ends scallop into one another and a four-option control reads as a row of
  overlapping blobs rather than one control — verified by before/after
  screenshots of the real app window.
  **Also found in that pass, and not a gap at all — just an unused style**:
  both `scrollable`s in this crate (`modules::app::main_view`,
  `modules::editor::footer_view`) were unstyled, so they rendered iced's
  *default* near-black scrollbar straight over `paper_window`'s rounded
  corner. `saola_theme::style::scrollable::rest` already existed and is now
  applied; check for that helper before adding any new scrollable.
- **No new saola-theme gap found in Stage 16** — `modules::history`'s one
  bare-literal size, `THUMBNAIL_MAX_DIM` (96px), is the same "a design
  system has no opinion on a drawing surface's own layout parameter"
  category `modules::editor`'s `STEP_BADGE_RADIUS`/`QUALITY_FIELD_WIDTH`
  already established, not a value a token scale should own. Everything
  else this stage drew (the History screen's rows, the swatch toast) reused
  existing tokens/styles (`button::rest`/`active`, `scrollable::rest`,
  `mono_font`, `on_paper.*`, `sizes.*`) with no new derived style needed.

## Conventions

- **No-panic rule**: no `panic!`/`unwrap`/`expect`/indexing on runtime
  paths. A dead daemon means `Print` silently does nothing — silent absence
  is the worst failure mode. Absent services (no daemon, no tray host, no
  ffmpeg) degrade gracefully or produce actionable errors, never crashes.
- **Teaching notes**: Jordan is newer to Rust — comment the non-obvious
  (async ownership, the pipewire thread bridge, SPA pods, zbus macros) as
  teaching notes; prefer explicit code over clever abstraction.
- **Dev builds optimize dependencies** (`[profile.dev.package."*"]
  opt-level = 3` in `Cargo.toml` — do not remove): the keybinds run the
  debug binary, and at opt-level 0 the per-screenshot encoders (vendored C
  libwebp via `cc`, `image`'s PNG/deflate stack) cost ~4.8 s per shot —
  measured 2026-08-08 as the entire cause of a "4-second screenshot" bug.
  With the override the same shot is sub-second; `saola-capture`'s own code
  stays unoptimized and debuggable. Full essay in `Cargo.toml`.
- **Dependency surveys**: every non-trivial dependency carries a dated
  `Cargo.toml` comment essay — alternatives considered and why they lost.
  Heavyweight deps and build-time C toolchains need strong justification.
  Stage 1 landed the WebP encoder, clipboard, and CLI parser surveys; Stage 4
  landed the TOML crate survey; Stage 5 landed the `libc` and `serde_json`
  surveys (essays live in `Cargo.toml`; outcomes below).
  Stage 10 landed the `pipewire` survey (outcome below), closing the last
  survey PLAN.md deferred. Stage 12 landed the `serde` survey (outcome
  below), the only new dependency since **until Stage 15's `cosmic-text`**
  (the Text/Step tools' real font shaping and glyph rasterization, via
  `raster_text` — full essay in `Cargo.toml`). Like `serde`/`serde_json`
  before it, `cosmic-text` is **zero net new crates**: it was already
  resolved in `Cargo.lock` at exactly this version, pulled transitively by
  `iced_wgpu`'s own text-rendering pipeline (`cryoglyph`) — `Cargo.lock`'s
  diff for this stage is one line, a new edge to an already-compiled crate,
  not a new `[[package]]` entry. **Stage 16 added no dependency and no new
  runtime binary either**: the history library reads `storage.rs`'s existing
  JSONL format and a plain directory scan; `PickColor` is a `#[zbus::proxy]`
  client against an interface niri already serves (same shape
  `capture/screencast.rs`'s `ScreenCastProxy` already established); a
  screenshot's WebP-to-PNG re-encode for Copy uses `image = "0.25"`'s
  already-enabled WebP *decode* feature (the WebP survey's own gap was the
  lossy *encoder*, which is why `webp` entered the tree in Stage 1 — decode
  was never missing); and GIF/animated-WebP export is two more argument
  lists to the same `ffmpeg` binary this crate already shells out to,
  through `std::process::Command::output()`. `Cargo.lock` is unchanged
  apart from this crate's own version bump. **Stage 13 added no dependency
  either — nor any new runtime binary**: audio is `-f pulse` argv plus
  `ffmpeg -sources/-sinks pulse` for device names, so the PKGBUILD gains
  nothing (a `pactl`-based design would have added `libpulse` to `depends`).
  **Stage 11 added no dependency at all** — the
  whole encoder is `std::process` plus one `libc::kill` for the SIGINT that
  `std::process::Child` cannot send (it only offers SIGKILL, which for ffmpeg
  means an unfinalized file), and `libc` was already in the tree. That is the
  `EncoderSink` boundary paying for itself: ffmpeg stays a CLI, so no
  `ffmpeg-sys`/`libav` link ever enters the graph.
  Stage 8 confirmed CAPTURE-RESEARCH §5.2's prediction: window capture
  (`capture/screencopy.rs`'s `capture_window`/`focused_window`) is entirely
  `niri_ipc::Request::Windows`/`FocusedWindow`/`Action::ScreenshotWindow` —
  zero new dependencies, and `wayland-protocols`' `staging` feature (which
  `ext_foreign_toplevel_list_v1` would have needed) was never added.
  - **WebP**: `image` 0.25's `WebPEncoder` is lossless-only (verified in its
    source); no pure-Rust lossy encoder exists on crates.io. Picked `webp =
    "0.3"` (wraps `libwebp-sys`, resolves to 0.9.6 — vendors and compiles
    libwebp's C sources via the `cc` crate; no system-dylib escape hatch at
    this version). Build-time C toolchain required (confirmed working);
    fully static, so no runtime `libwebp.so` and no PKGBUILD `depends`
    entry, only a `makedepends` one. Rejected: `libwebp-sys`'s
    `system-dylib` feature (not available in the 0.9.x line actually
    resolved) and spawning `cwebp` (a second external-CLI runtime
    dependency alongside ffmpeg, on the hot per-screenshot path).
  - **Clipboard**: `wl-clipboard-rs = "0.9"` — pure Rust, reuses the
    wayland-client stack already needed for screencopy, no new runtime
    binary. Rejected spawning `wl-copy`: the `wl-clipboard` package is
    **not installed** on Jordan's machine (verified live), so it would add
    a third `sudo pacman -S` line and a silent-breakage risk on the
    always-on `--copy` default.
  - **CLI parser**: `clap = { version = "4", features = ["derive"] }` —
    four subcommands with a real flag surface (Architecture) justify
    derive's generated `--help`/validation over hand-rolling; its
    syn/quote/proc-macro2 chain overlaps with zbus's and wayland-scanner's
    own proc macros, so the marginal dependency cost is small. `lexopt`/
    `pico-args` stay zero-dependency but would mean hand-writing subcommand
    dispatch and config-override precedence this app doesn't need to own.
  - **libc** (Stage 5, `libc = "0.2"`) — **zero net new crates** (already in
    the tree transitively). Two uses, both unavoidable: `memfd_create` +
    `ftruncate` for the `wl_shm` buffer screencopy blits into, and
    `localtime_r` for local-time capture filenames (`std::time` knows only the
    epoch; converting to a local civil date needs the system tz database).
    Rejected `rustix` (safer wrappers, also already in the tree — but no
    `localtime_r`, so `libc` would still be needed, and one dep beats two) and
    `chrono`/`time`/`jiff` (none in the tree, all heavier than one format
    string per screenshot).
  - **serde_json** (Stage 5, `serde_json = "1"`) — also **zero net new
    crates** (niri-ipc's own transport already pulls it). Backs the
    append-only JSON-Lines history index. Used via `serde_json::Map`/`Value`
    only, **no `#[derive(Serialize)]`**, matching `config.rs`'s hand-walked
    posture. Chosen over a hand-rolled TSV specifically for escaping: a saved
    path can legally contain tabs, newlines and quotes.
  - **pipewire** (Stage 10, `pipewire = { version = "0.10", features =
    ["v0_3_33"] }`) — the official pipewire-rs bindings, the only
    maintained Rust binding for `pw_stream`. **Not** zero-net-new-crates
    (unlike `libc`/`serde_json`): it adds `pipewire`, `pipewire-sys`,
    `libspa`, `libspa-sys` plus a build-time-only tail (`bindgen`,
    `clang-sys`, `system-deps`, and a second `toml` 1.1.4 that is a build
    dependency of a build script and never linked, so the "one resolved
    runtime `toml`" reasoning above still holds). Justified because there is
    no lighter way to speak PipeWire and PipeWire is the only video
    transport niri offers. Rejected: hand-rolled FFI (would mean
    hand-writing the SPA pod builder/parser, where a wrong byte layout fails
    at *negotiation*, not compile time), `gstreamer` + `pipewiresrc` (a
    second large media framework alongside the ffmpeg CLI boundary), and
    `ashpd` (portals are forbidden by Boundaries).
    - **`libspa` is not declared separately** — `pipewire` re-exports it as
      `pipewire::spa`, so one line makes a version skew impossible. Raw SPA
      constants come through `pipewire::spa::sys::*`.
    - **The feature gate is a floor, not a version stamp.** `v0_3_33` is the
      *minimum* that provides the one gated item this code needs
      (`PropertyFlags::DONT_FIXATE`, which CAPTURE-RESEARCH D4's modifier
      property is written in terms of). Jordan's PipeWire is 1.6.8 so
      `v1_2_0` would also build, but picking it would raise the required
      system libpipewire for every other machine in exchange for API this
      crate never calls. Bump the gate when a stage needs something newer,
      and say which item forced it.
    - **Packaging (Stage 17):** it *links* the system PipeWire via
      `system-deps`/pkg-config rather than vendoring it (the opposite of
      `webp`), so `pipewire` is a real PKGBUILD `depends` entry, plus
      `clang` in `makedepends` and `libclang-dev` in CI for the bindgen run.
  - **serde** (Stage 12, `serde = { version = "1", features = ["derive"] }`)
    — **zero net new crates** (already resolved via `serde_json` and via
    zbus's own `zvariant` crate, which every `#[derive(zvariant::Type)]` in
    this codebase already depends on transitively). `modules::tray`'s
    `com.canonical.dbusmenu` server needs one wire struct (`RawMenuNode`,
    `(ia{sv}av)`) with `#[derive(Type, Serialize)]` so its static D-Bus
    signature matches what a typed proxy (the panel's own `GetLayout`
    declaration) checks a reply against. Rejected: building the reply as a
    bare `zvariant::Value` tree with no derive at all — its *static*
    signature is `v` (variant), not `(ia{sv}av)`, which a real dbusmenu
    client would reject as a mismatch. Full essay in `Cargo.toml`.
  - **Surprise**: `niri-ipc` is `GPL-3.0-or-later` (verified from its own
    `Cargo.toml`, not just crates.io metadata) — the only non-`MIT OR
    Apache-2.0`-compatible-by-permissive-default dependency in the tree.
    Rust static-links, so the distributed binary is a combined work under
    GPL-3.0-or-later's terms even though this repo's source stays dual
    MIT/Apache-2.0. This is the same posture `saola-panel` already accepts
    with the same dependency — not a new decision, flagged for awareness.
- **Config**: `~/.config/saola/capture.toml`, `toml = "0.9"` (same major line
  `saola-theme`'s own `saola-tokens` crate already pulls in — unifies to one
  resolved `toml` version instead of two), hand-walked over `toml::Table`
  (no `serde::Deserialize` on `CaptureConfig` itself) for per-knob warnings;
  bad knob → warn + that knob's default; bad file → warn + all defaults,
  still start. Sibling resolution order (`--config-dir` >
  `$SAOLA_CONFIG_DIR` > `$XDG_CONFIG_HOME/saola` > `~/.config/saola`).
  **Migrated from KDL in Stage 4** — same knobs, names, defaults, and
  `CaptureConfig` API as the Stage 3 KDL version; only the file format and
  `src/config.rs`'s parsing internals changed. Schema (landed Stage 4,
  `src/config.rs`; verbatim in the Stage 4 handoff for Stage 17's README).
  Bare top-level keys, no `[capture]` wrapper table — the file is already
  capture's own, so there's no sibling config to disambiguate against:
  ```toml
  save-dir = "~/Pictures/Screenshots"  # default: unset (storage.rs falls back to ~/Pictures/Captures)
  image-format = "webp"                # "webp" | "png", default "webp"
  webp-quality = 90                    # 1..=100, default 90 (WebP only; PNG is lossless) — added Stage 5
  png-also = false                     # default false
  video-preset = "hevc"                # "hevc" | "av1" | "h264", default "hevc"
  vaapi-device = "/dev/dri/renderD129" # default: unset = discover it (Stage 11). An explicit
                                       # render node for the hardware presets. Deliberately NOT
                                       # validated by config.rs — "is this a usable render node?"
                                       # is a hardware question, not a parse question; a path
                                       # that doesn't exist warns and falls back to full
                                       # discovery in ffmpeg_cli::choose_encoder, which is the
                                       # same warn-and-default rule applied at the layer that can
                                       # actually check. A path that *does* exist is used **on
                                       # its own**, so the override genuinely overrides rather
                                       # than merely reordering.
  audio = "none"                       # "none" | "mic" | "system" | "both", default "none" —
                                       # what a bare `record start` records. Added Stage 13;
                                       # `--audio` overrides it, `--audio none` overrides it off.
  audio-mic-source = "alsa_input.…"    # default: unset = the server's default input. An explicit
                                       # PulseAudio source name. Deliberately NOT validated by
                                       # config.rs, exactly like `vaapi-device`: a name that
                                       # exists is used on its own, one that doesn't warns and
                                       # falls back to discovery in `audio::plan_audio`.
  audio-system-source = "….monitor"    # default: unset = the default output's `.monitor`
  audio-offset = 0.13                  # seconds of `-itsoffset` on every audio input; default
                                       # `audio::DEFAULT_SYNC_OFFSET` (0.13, measured — see that
                                       # constant and CAPTURE-RESEARCH §4.5). Positive delays the
                                       # audio. Accepts a float or an integer, ±5 s; outside that
                                       # (or non-finite) warns and defaults. **0.0 is a real
                                       # value**, not "unset" — it turns the correction off.
  cursor = true                        # default true
  delay = 0                            # whole seconds, default 0
  toasts = true                        # the saola-notifications kill-switch, default true
  copy = true                          # default true
  ```
  A `capture.kdl` found in the resolved config dir with no `capture.toml`
  next to it logs a one-line migration hint naming both paths (a warning,
  not an error — `capture.kdl` is no longer read at all; defaults still
  apply until it's ported by hand).
- **Saved captures and the history index** (`src/storage.rs`, Stage 5).
  Files go to `--output` > `save-dir` > `~/Pictures/Captures` (created on
  demand), named `Screenshot_YYYY-MM-DD_HH-MM-SS.<ext>` in **local** time,
  with a `-1`, `-2`, … suffix on collision; every write is `.name.part` +
  `rename`, so a failed write never leaves a truncated image. The clipboard
  always gets **PNG** (`image/png` is what every paste target understands),
  regardless of the saved format. The index is append-only **JSON Lines** at
  `$XDG_DATA_HOME/saola/capture/history.jsonl` (default
  `~/.local/share/saola/capture/history.jsonl`), one object per line with
  `v/unix/path/png?/kind/format/width/height/scale/bytes` — readers must
  ignore unknown keys and skip unparseable lines. Full spec on
  `storage::HistoryEntry`; `storage::read_history_entries` (Stage 16) is the
  one reader, and `modules::history` is its consumer. Clipboard and
  index failures **warn and continue** — the file is already on disk.
- **Recordings share that directory and almost nothing else** (Stage 11,
  `storage::allocate_recording_path`). They are named
  `Recording_YYYY-MM-DD_HH-MM-SS.<mkv|mp4>` — a different prefix so one
  directory sorts into two groups, and collision-checked against **both**
  video extensions so a `.mkv` and a `.mp4` a second apart cannot share a
  stem. Three deliberate differences from the still-image path:
  - **No `.part`+`rename`.** `storage::write_atomically` exists because a
    screenshot is encoded in memory and written in one shot, so "complete" is
    knowable; a recording is written incrementally by an external process
    over minutes, and a rename would only make a recording interrupted by a
    crash *disappear* instead of being recoverable. Matroska is designed to
    survive truncation, which is part of why it is the primary container.
    **Caveat found live**: that only helps once bytes have actually reached
    the disk — ffmpeg's own AVIO buffer flushed roughly every ~250 KB in the
    measured run, so a recording SIGKILLed after 3.8 s had written *nothing*
    yet, and `FfmpegSink`'s zero-byte cleanup (which removes only
    zero-length files, never a partial one) correctly took it away.
  - **No clipboard.** Nothing pastes a video.
  - **No history-index row, and Stage 16 kept it that way on purpose.**
    `HistoryEntry`'s documented schema fixes `format` to `"webp" | "png"`
    and carries still-image-only fields (`png`, `scale`); rather than bump
    the schema (a `v: 2`, or a `type` key) to give recordings a row,
    `modules::history` finds them with a directory scan filtered on this
    prefix and `VIDEO_EXTENSIONS` (both now `pub(crate)` in `storage.rs`
    specifically so the library can reuse them) — see that module's own doc
    comment for the full reasoning (a schema bump would touch a stable,
    load-bearing writer for a reader-only feature; the filename convention
    already carries everything the library needs; a scan self-heals when a
    file is deleted by hand, an index row would not). Verified live: a run
    with an isolated
    `XDG_DATA_HOME` produced four recordings and **no** `history.jsonl` at
    all.
- **One runtime**: `zbus 5` with `default-features = false, features =
  ["tokio"]`; never a second async runtime. The **PipeWire main loop is the
  one sanctioned extra thread** — it bridges to the daemon via a bounded
  channel and is documented as the exception. Capture itself is *blocking*
  (Wayland roundtrips, ~0.3 s of compositor blit, an encode): the daemon runs
  it on `tokio::task::spawn_blocking`, guarded by `Handle::try_current()`
  because `spawn_blocking` panics outside a runtime (`dbus::run_blocking`).
  **Stage 11 adds a second long-lived thread, and it needs no sanction**:
  `encode::ffmpeg_cli`'s stderr drain (one per live ffmpeg). It runs no
  executor, owns nothing but an `Arc<Mutex<VecDeque<String>>>`, and exits
  when the pipe closes. It is not an optimisation — **a child whose stderr
  pipe fills blocks in `write`, stops reading its own stdin, and the whole
  recording deadlocks with nothing having failed**. The alternative,
  `Stdio::inherit()`, needs no thread but throws away the tail, which is
  exactly where "No space left on device" appears. The recording *pump* is
  not a thread of its own either: it is `spawn_blocking` work, held for the
  whole recording, which is what tokio's blocking pool is for.
- **An option map carries decisions, and a zero can be one** (Stage 13, found
  live). `RecordOptions::to_dbus_options` omits keys whose *absence is itself
  the decision* (`geometry`, `window-id`, `audio` — the decode side treats an
  absent key and "no" identically). `audio-offset` is not like that: the
  decode side defaults an absent key to `audio::DEFAULT_SYNC_OFFSET`, which is
  **not** zero, so omitting an explicit `audio-offset = 0.0` silently kept the
  0.13 s correction and made the knob impossible to turn off. Before adding a
  `if value != default { insert }` to that map, check what the decoder does
  with the key's absence; `cargo test` cannot see across the D-Bus boundary,
  and only a live A/V measurement caught this one.
- "Every module maps to a signal, not a poll." Modules follow the sibling
  shape: state struct + `view(&Theme) -> Element` + `subscription()` +
  nested `Message` enum.
- **The zbus-hosted D-Bus service and the iced daemon's own surfaces are
  different async tasks** (Stage 6, `dbus_worker_stream`/`dbus::DaemonEvent`):
  when a served method (`CaptureService::screenshot`) needs to poke the
  daemon's `update` loop, the bridge is a small bounded
  `iced::futures::channel::mpsc::channel` (never `tokio::sync::mpsc` — `iced`
  already re-exports the `futures` crate, so this is **zero net new
  crates/features**, matching this repo's habitual dependency bar). The
  served method offers to it with `try_send`, never `.send().await` — a full
  channel degrades to a logged warning, never a blocked D-Bus reply.
  **Stage 10 decided the PipeWire side differently, on purpose**: the pw
  thread is *not* async (a C event loop calling synchronous callbacks — no
  executor to `.await` on, no `Waker` to wake), and Stage 11's consumer is a
  blocking write into ffmpeg's stdin, so both ends of a futures channel's
  reason to exist are unused. `capture/screencast.rs` therefore uses
  **`std::sync::mpsc`** — also zero new crates — with the same `try_send`
  posture: `sync_channel(4)` for frames (drop + count + log on full, per
  PLAN.md's backpressure rule) and a *separate unbounded* channel for
  control messages (the negotiated format, a fatal error, end-of-stream),
  because merging them would let "the queue was briefly full" swallow the
  negotiated format. The rule to carry forward is the posture, not the
  crate: **never block a producer on a consumer; pick the channel that fits
  the thread you are on.**
- **Mechanical iced 0.14.2 gotchas, found in Stage 6** (cheap to relearn
  the hard way, cheaper to just know): `iced::widget::Space::new()` takes
  **zero** arguments in this crate's resolved version — size it with
  `.width(..)`/`.height(..)` builder calls, not `Space::new(w, h)` or a
  `Space::with_width(w)` associated function (neither exists here, despite
  looking plausible from memory of other iced versions).
  `iced::widget::image::Handle` derives `Clone`/`PartialEq`/`Eq` but **not**
  `Debug` (checked directly in `iced_core-0.14.0/src/image.rs`) — wrap it in
  a local newtype with a hand-written `Debug` before putting it in any type
  that needs to derive `Debug` (`main.rs`'s `Thumbnail` and
  `modules::overlay::FrozenFrame` are the two examples).
- **More mechanical iced 0.14.2 gotchas, found in Stage 7:**
  - **`iced::event::listen_with` takes a `fn` pointer, not a closure**, so
    it cannot capture any state to filter on. It *does* hand the
    `window::Id` and the `event::Status` to that function, so the pattern is
    "put the Id in the message, filter in `update`" — `main.rs`'s
    `overlay_event_subscription` / `Message::OverlayEvent`.
  - **`event::Status::Captured` is the only thing separating "clicked a
    widget" from "clicked the surface underneath it"** when raw events and a
    widget tree share one surface. Consult it for *presses*; forward
    releases and motion regardless (`modules::overlay::message_from_event`
    documents why).
  - **A `button` with no `on_press` does not capture its press.** A disabled
    button therefore lets the click fall through to whatever is listening
    underneath — which for the overlay meant "miss Cancel by three pixels,
    start a drag". Wrap chrome that must swallow input in
    `mouse_area(..).on_press(some_noop_message)`, which *does* capture (and
    which iced correctly skips when a child button already captured).
  - **Anything a `Message` derives, `#[to_layer_message]` enforces** — so a
    reply channel riding in a message must be `Clone`. `futures::channel::
    oneshot::Sender` is not; a capacity-1 `mpsc::Sender` is, and is a
    oneshot in every way that matters.
  - `keyboard::Event::KeyPressed` has a **`repeat: bool`** field in this
    version — building one in a test needs it.
  - The dashed selection edge is `canvas::Path::rounded_rectangle` +
    `canvas::Stroke { line_dash: LineDash { segments, offset }, .. }`; both
    exist and work. There is no widget-level dashed border.
- **Mechanical iced gotchas found in Stage 9** (the app window — a plain
  `iced::application`, not `iced_layershell`, so some of these are new
  territory rather than repeats):
  - **`iced::widget::image` (the module/fn) shadows the `image` crate's own
    name** the instant `use iced::widget::image;` is in scope (needed for
    `image::Handle`) — a bare `image::open(path)` then resolves against the
    iced widget module (which has no `open`), not the crate. A leading `::`
    forces crate-root resolution: `::image::open(path)` (`modules::editor::
    load_canvas` as of Stage 14; originally `modules::app::load_image`, moved
    when the editor stopped being a stub). No prior module needed both the
    iced widget half and the external crate's decode half in the same file.
  - **A plain `iced::application`'s `Message` needs `#[derive(Clone)]`**,
    the same requirement the daemon's `Message` has for an unrelated reason
    (`#[to_layer_message(multi)]`) — here it's `button`/`mouse_area`'s own
    bound when built through the `row!`/`container` helpers. Found by
    letting the compiler's own "consider annotating with `#[derive(Clone)]`"
    suggestion do the work, not by predicting it.
  - **`iced::Subscription::run_with<D: Hash>` cannot take a
    `zbus::Connection` as its keying data** — `Connection` isn't `Hash`.
    This is why `modules::app`'s Record tab didn't (Stage 9) subscribe to the
    `RecordingFinished` signal per-connection. **Stage 12's answer**: skip
    `run_with` entirely — `modules::app::record_signal_stream` is a
    zero-argument `fn` pointer plugged into a plain `Subscription::run`,
    exactly `main.rs::dbus_worker_stream`'s own shape, building its own
    independent `Connection` inside the stream rather than keying on one from
    the outside.
  - `window::open_events()` is enough to learn a single-window
    `iced::application`'s own `window::Id` — no `window::latest()`/
    `oldest()` round trip needed when there's provably only ever one window.
- **Mechanical iced gotchas found in Stage 14** (`iced::widget::canvas`, the
  first module in this crate that draws interactive vector geometry rather
  than a `canvas` limited to a scrim/dashed-edge overlay like `modules::
  overlay`'s):
  - **`canvas::Program::update` hands you a resolved, widget-local cursor
    position on *every* event** (`cursor.position_in(bounds)`), including
    `ButtonPressed`/`ButtonReleased` — unlike the raw `iced::Event` stream
    `main.rs`'s `event::listen_with` forwards to `modules::overlay`, where a
    mouse button event genuinely carries no coordinates of its own and
    `Overlay::press`/`release` have to fall back to a remembered `cursor:
    Option<Point>`. A `canvas::Program` never needs that workaround — see
    `modules::editor::EditorCanvas::update`.
  - **Two style helpers built the same way
    (`saola_theme::style::button::rest`/`active`, both `-> impl Fn(&iced::
    Theme, Status) -> Style`) are two different opaque types**, even though
    they satisfy the same bound — `if primary { active(..) } else { rest(..)
    }` fails to typecheck (`expected active::{opaque#0}, found
    rest::{opaque#0}`). AGENTS.md's own Conventions already prefers
    duplicating a `.style(...)` call per match arm over `Box<dyn Fn>`; this
    is the concrete error that rule is written to route around
    (`modules::editor::action_button`).
  - **`clippy::large_enum_variant` is a real, load-bearing lint here.**
    `modules::app::ViewState::Editor` grew a whole `EditorModel` (undo/redo
    stacks included) inside it, which made `ViewState` several hundred bytes
    even while sitting in the tiny `Main` arm — `Box<editor::EditorState>`
    fixes it, and is the right shape generally: only one `ViewState` value
    ever exists per process, so the extra indirection is free.
  - `canvas::Stroke` is `Copy`; `iced::Rectangle` has no `.right()`/
    `.bottom()` convenience methods (just `x + width`/`y + height` by hand)
    but does have `.contains`/`.expand`/`.shrink`, which cover most of what
    `modules::overlay::Rect` hand-rolls for its own, differently-shaped
    needs (see that module's doc comment on why it isn't reused here).
- **Testing**: pure logic (selection geometry, recorder state machine,
  config, undo/redo, swizzle/crop, blur kernels) unit-tested directly;
  buses/compositors behind traits with fakes. **Never `std::env::set_var`
  in a test** (binding, learned the hard way in Stage 5): `cargo test` runs
  every test in the binary on parallel threads of *one process*, so two
  tests each redirecting `$XDG_DATA_HOME`/`$HOME` at their own temp dir
  clobber each other — an intermittent ~1-in-15 failure that looks like a
  filesystem flake. The shape to copy instead: the environment is read once
  at a thin production wrapper (`storage::save_capture`,
  `storage::history_path`, `CaptureConfig::resolve_path`), and the logic underneath
  takes the resolved path as an argument (`storage::save_capture_indexing_to`,
  `storage::history_dir`, `config::config_dir_from`); tests call the
  argument-taking half. **The nested-niri rule
  (binding, from saola-lockscreen/CLAUDE.md)**: anything mapping overlay
  surfaces or grabbing the keyboard is live-tested against a nested niri
  first — spawn `niri -c /tmp/nested-niri.kdl &` *without* `--session`,
  override `NIRI_SOCKET` explicitly (your shell's points at the outer
  niri), run against the nested `WAYLAND_DISPLAY` only, tear down after.
  Never run input-grabbing tests in the real session without Jordan
  present. **Stage 5 earned this rule its keep**: the nested winit output's
  `Flipped180` transform exposed a mirrored-capture bug that the real
  session (transform `Normal`) could never have shown. Two nested-niri
  gotchas found there: `niri msg output winit scale N` works, but
  `... transform 90` is silently ignored (the winit backend pins its own
  transform), so the rotation cases stay untested; and comparisons against
  `grim` are only byte-exact at **scale 1** (see Architecture). **Stage 6
  earned it again**: both Architecture bullets above (the transparent-
  background requirement and the surface-creation-latency finding) were
  invisible to `cargo test` and found only by mapping real surfaces in
  nested niri. Two additions to the recipe: `niri msg layers` (not
  `windows`, not `outputs`) lists layer-shell surfaces by namespace/output;
  `magick file.png -format "%[pixel:p{X,Y}]" info:` (ImageMagick, already
  installed) samples one pixel's color from a `grim` capture without opening
  it, cheap enough to script into a tight loop for a "did this render in
  time" check the way a single screenshot at an arbitrary offset can't
  answer reliably. (**Argument order corrected in Stage 7** — the Stage 6
  handoff's `magick -format … info: file.png` form fails with "no decode
  delegate"; the file must come first.) **Stage 7 earned the rule a third
  time** and adds four more pieces to the recipe, all of which cost real
  time to rediscover:
  - **Give the nested session its own D-Bus bus.** `dbus-daemon --session
    --print-address --fork`, then export `DBUS_SESSION_BUS_ADDRESS` for the
    nested daemon *and* every CLI verb aimed at it. Without this the test
    daemon fights Jordan's real one for `io.saola.Capture1`, and a real
    `Print` press in his session gets answered by a daemon rendering onto a
    nested display.
  - **The daemon's environment decides where captures land, not the CLI's.**
    The save happens daemon-side, so `XDG_DATA_HOME`/`save-dir` must be
    overridden on the *daemon* process; overriding them on `shot` does
    nothing and quietly appends test rows to `~/.local/share/saola/capture/
    history.jsonl`. (`--output` is the exception — it travels over the bus.)
  - **Scripted input**: the Stage 2 `inject` probe generalises well. Stage 7
    used a `move / press / drag / release / key / sleep` command-script
    variant of it (built from `docs/research/2026-08-08-stage2/inject.rs`),
    which is what made a full drag → resize → move → confirm round trip
    reproducible instead of eyeballed.
  - **Locating widgets to click**: dump one scanline of a `grim` capture
    (`magick shot.png -crop WIDTHx1+0+Y +repage txt:`) and find the runs of
    ivory — that gives exact button centres to aim the injected pointer at,
    without guessing from a layout calculation.
  **Stage 8: a real incident, not a near-miss — read this before injecting
  anything.** A Stage 8 live-test script built the exact nested-niri setup
  above correctly (private D-Bus bus, nested `NIRI_SOCKET`, isolated
  `XDG_DATA_HOME`) but the line invoking the injector itself was missing its
  own `WAYLAND_DISPLAY=$NESTED_WAYLAND` prefix — every *other* command in
  the script had it, this one didn't, and nothing caught the omission before
  it ran. The injector fell back to whatever `WAYLAND_DISPLAY` the shell
  already had, which was **Jordan's real session**, and
  `zwlr_virtual_pointer_manager_v1` turned out to be reachable there too
  (contra the Stage 2 probe's note that it wasn't, on that machine, in that
  setup — evidently that has changed, or the setup differs enough not to
  rely on it). The result: one real, synthetic left-click landed on Jordan's
  actual screen (move → press → release, no keys) before anyone noticed and
  killed everything. No keyboard input was sent, and the nested processes
  themselves were fully torn down (verified via `ps aux` — no leaked niri
  process, daemon, `dbus-daemon`, or client left running), but a stray click
  reached the real desktop, which is exactly what this whole rule exists to
  prevent. **The fix isn't "be more careful" — it's structural**: never rely
  on remembering to prefix every single injected-input command by hand.
  Export `WAYLAND_DISPLAY`/`NIRI_SOCKET` once, for the *whole test script's*
  environment (`export`, not a per-line prefix), so there is exactly one
  place to get it right instead of N, and — belt and braces — have the
  injector itself refuse to run unless `WAYLAND_DISPLAY` is explicitly
  passed as an argument rather than inherited from the environment at all,
  so a forgotten prefix is a hard error, not a silent fallback to whatever
  display happened to be ambient. Whoever runs live input-injection tests
  next should build that guard into the tool before using it, not after.
  **Stage 10 found the rule's first genuine limit**: a nested niri started
  without `--session` serves **no D-Bus interfaces at all** on the private
  bus — `busctl --user list` against it shows neither
  `org.gnome.Mutter.ScreenCast` nor `org.gnome.Mutter.DisplayConfig`, and a
  `record start --dry-run` aimed at it fails with `ServiceUnknown`. So the
  entire recording path (Stages 10–13) **cannot** be exercised in nested
  niri; it must be tested against the real session, exactly as the Stage 2
  probes were. That is acceptable specifically because a screencast maps no
  surface, grabs no keyboard, needs no injected input and (in dry-run form)
  writes nothing — the three things the nested rule exists to contain are
  all absent. Verify teardown afterwards every time: `niri msg casts` must
  say "No screencasts", and `pw-dump` must show no leftover `Video/Source`
  node beyond the webcams. Stage 10's own runs did, four times.
  **Stage 11 adds one live-testing trap that cost two runs**: `pkill -f
  <pattern>` matches **the test script's own command line**, because the
  script text contains the pattern — so `pkill -KILL -f hevc_vaapi` inside a
  script kills the script. Both times it looked like the daemon had hung.
  Build the pattern at runtime (`KILLPAT=$(printf 'hevc_%s' 'vaapi')`) or
  match by process name (`pkill -x ffmpeg`), never by a literal that also
  appears in the script. Same class of foot-gun as Stage 8's missing
  `WAYLAND_DISPLAY` prefix, and the same fix: make the wrong thing
  unexpressible rather than remembering not to write it.
  **Stage 12 found that the tray/dbusmenu surface needs neither the
  nested-niri rule nor Jordan's live session**: `org.kde.StatusNotifierItem`/
  `com.canonical.dbusmenu` are pure D-Bus, no Wayland surface, no keyboard,
  no synthetic input — a menu click is a plain method call
  (`busctl --user call io.saola.Capture1 /StatusNotifierMenu
  com.canonical.dbusmenu Event isvu -- <id> clicked s "" 0`), not a pointer
  event, so exercising "Stop recording"/"Quit daemon" live needs nothing
  more than the daemon and (to prove real host integration, not just that
  the object exists) whatever already owns `org.kde.StatusNotifierWatcher`
  — Jordan's real `saola-panel`, running the whole time, completely
  unaffected by a second item registering and unregistering against it.
  This is how Stage 12 verified the SNI item, the menu, the region/window
  crop math and the finish toast against the real session without ever
  touching nested niri or asking Jordan to drive anything by hand — see the
  Stage 12 handoff for the exact transcripts. The one thing this recipe
  still can't reach is a real pointer click on the tray icon itself (that
  needs synthetic input against the real desktop, which remains
  Jordan's-presence-only per the rule above).
  **Stage 13 adds the audio half of the recipe, which needs three extra
  isolations and one substitute for a human:**
  - **A scratch null sink, never Jordan's speakers.** `pactl load-module
    module-null-sink sink_name=saola_test …` returns a module id; keep it and
    `pactl unload-module <id>` in the teardown. Point the recording at it with
    `audio-system-source = "saola_test.monitor"` in the test `capture.toml`
    (which also exercises the override knob) rather than by changing the
    default sink — **never `pactl set-default-sink`**, that is Jordan's
    session state. Route a test player into it with `pactl move-sink-input
    <index> saola_test`; `PULSE_SINK` was tried first and **SDL ignores it**
    (verified the hard way — ffplay played out of the real speakers for one
    run before it was moved).
  - **`PULSE_SERVER=/nonexistent` on the *daemon* is the "no audio hardware"
    fake.** It makes ffmpeg's own device enumeration come back empty, which is
    exactly the input `audio::plan_audio`'s degradation path takes, and it
    isolates to the test daemon.
  - **The clap test, without a human to clap**: generate one clip whose white
    flash and 1 kHz click share a timestamp (`drawbox …
    enable='lt(mod(t-4,2),0.1)'` plus `aevalsrc 'sin(2*PI*1000*t)*between(mod
    (t,2),0,0.05)'`), play it with `ffplay`, record **that window**
    (`--window-id`) with `--audio system` off the null sink, then locate both
    events in the output — video with `signalstats,metadata=print:key=lavfi.
    signalstats.YAVG`, audio with `silencedetect=noise=-40dB:d=0.02`. Verify
    the harness against the *source* file first (it measured 0.0 ms there).
    The player's own A/V sync is the irreducible error term; the player-free
    cross-check is ffmpeg's `-loglevel info` input dump, which prints each
    input's absolute `start` wallclock —
    `SAOLA_CAPTURE_FFMPEG_LOGLEVEL=info` on the test daemon, and the file's
    start-offset desync *is* `audio_start − video_start`.
  - **Recording the microphone is the one thing to do sparingly**: a few
    seconds, only to prove the path negotiates and produces a valid Opus
    track, and delete the file in the same command. Never analyse or keep it.
  - Teardown grows: `pactl list short sinks | grep saola_test` empty,
    `pactl list short sink-inputs` empty, `pactl info`'s Default Sink/Source
    unchanged, and no `ffplay` left running.
  **And one thing this recipe genuinely could not reach**: the screen locked
  (saola-lockscreen) partway through Stage 13's session, so the warning toast
  could not be photographed. It was verified instead by its **layer surface**
  — `niri msg layers | grep -c 'Namespace: "saola-capture"'` going 1 → 2 → 1
  across the toast's life, the flash being the permanent one. Worth
  remembering as a cheap toast check even when the screen *is* visible.
  **Stage 16 found a real, safe middle ground for `PickColor`**: niri's
  `org.gnome.Shell.Screenshot.PickColor` is a genuine interactive pointer
  grab against the real session (the same "must never be exercised outside
  a nested niri, and nested niri serves no D-Bus interfaces at all" bind
  Stage 10 already documented for the ScreenCast interface applies here too
  — read-only introspection is safe, an actual call is not, without a human
  present to click or cancel it). What *is* both safe and useful: `busctl
  --user introspect org.gnome.Shell.Screenshot /org/gnome/Shell/Screenshot`
  (confirms the interface is served and its exact signature — this is how
  Stage 16 caught the `a{sv}`-not-`(ddd)` quirk, entirely read-only) and
  `busctl --user introspect io.saola.Capture1 /io/saola/Capture1` against an
  isolated test daemon (confirms this crate's *own* `PickColor() -> (ddd)`
  is unaffected). Whoever next needs to verify the interactive half end to
  end should run `saola-capture pick-color` themselves, present, ready to
  click or Escape — the same posture Stage 7's overlay and Stage 13's
  microphone recording already established for "needs a human, not a
  script".
- **Conventional Commits** (release-plz derives bumps); `chore:`/`ci:`/
  `docs:`/`test:` are changelog-invisible. Never hand-edit versions or
  `CHANGELOG.md`.

## Releases

release-plz in git-only mode, mirrored from saola-panel: release-pr +
release jobs, tags `saola-capture-v{version}`, PKGBUILD attached as a
release asset (not pushed to AUR), `0.1.0-dev` suffix as the prerelease
gate. Set up in Stage 17.

## Boundaries (binding)

- **The sudo rule**: no agent runs `sudo` or edits Jordan's user/system
  config — that includes `~/.config/niri/config.kdl` (keybinds,
  `spawn-at-startup`) and package installs (`sudo pacman -S ffmpeg`). Print
  the exact lines/commands for Jordan and wait.
- **No portals.** xdg-desktop-portal Screenshot/ScreenCast is broken by
  configuration on this machine and portals gate untrusted apps — this is a
  first-party DE component. Capture goes direct: `zwlr_screencopy_v1` and
  `org.gnome.Mutter.ScreenCast` (served by niri itself). A future
  **saola-portal** is deferred entirely; if compositor portability is ever
  needed, it enters through the `CaptureBackend` trait only.
- **ffmpeg is an external CLI boundary.** Never link ffmpeg/libav
  libraries; never run its installer. Its absence is a clean runtime error
  naming the install command. The `EncoderSink` trait exists so ffmpeg can
  become optional later. **It is also the *only* external CLI** — Stage 11
  refused a runtime `vainfo` and answered "can this device encode?" with a
  trial encode; Stage 13 refused a runtime `pactl` and answers "what audio
  devices exist?" with `ffmpeg -sources/-sinks pulse` (`src/audio.rs`, and
  CAPTURE-RESEARCH §4.5 for why that is the *better* question and not merely
  the cheaper dependency). Adding a second runtime binary needs the same bar a
  new crate does.
- **Toasts are interim.** A future **saola-notifications** component owns
  notifications; this app's toasts follow the style-guide card spec
  exactly, honor the `toasts false` config kill-switch, and the
  `io.saola.Capture1` signals (`CaptureTaken`, `RecordingStarted`,
  `RecordingFinished`, `Error`) are the stable contract that component will
  consume. Don't build a notification daemon here.
- **Redaction is a promise**: blur/pixelate must be irreversible in
  exported files. **Real as of Stage 15**: `modules::editor::EditorModel::
  apply_redaction` mutates the live canvas's own pixels in place rather than
  compositing a translucent shape on top of them at save time, so a
  Saved/Copied file has no path back to the original content — see that
  method's doc comment for the full reasoning, including why this is the
  one Stage 15 tool that is *not* a `Shape` variant.
- Recording is user-initiated only — no capture without an explicit user
  action (keybind, CLI, button); the tray item is visible for the entire
  duration of every recording.
