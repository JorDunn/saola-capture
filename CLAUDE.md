# CLAUDE.md — saola-capture

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
stage and says so in its handoff. A stale CLAUDE.md is a bug.

> Status: Stages 1–10 landed (repo skeleton, dependency survey; every capture
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
> `open` CLI verb, the toast's own (unchanged) direct spawn, and (once
> Stage 12 builds it) the tray menu are the three "reopen" paths this
> enables; none of them can currently tell whether a window process is
> already alive-but-hidden, so a second `open` mid-capture spawns a second
> process — a recorded v0.1 gap, not an oversight (see the Stage 9
> handoff).
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
> Still stubs, each answering with a clean error naming its stage:
> `StartRecording`/`StopRecording` (Stage 11), `PickColor` (Stage 16).
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
cargo run -- record start|stop|toggle [--preset hevc|av1|h264] [--audio mic|system|both]
cargo run -- record start --dry-run          # Stage 10: negotiate a real screencast, log the
                                             # SPA format + 5 s of frame cadence, write NOTHING,
                                             # tear down. Never contacts the daemon.
cargo run -- record start --dry-run --window-id 16  # cast one window instead of the focused output
                                                    # (dry-run only until Stage 12)
cargo run -- pick-color
cargo run -- open                  # raises the app window (spawns it detached — Stage 9)
cargo run -- window                # the app window process — Screenshot/Record tabs, real as of Stage 9
cargo run -- window edit <path>    # boots straight into the (stub) editor view on that file
cargo run -- --config-dir ~/scratch shot --fullscreen  # capture.toml from an alternate dir
```

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
Stage 5 `Screenshot` is real, and as of Stage 9 so is `OpenWindow`; the
remaining served methods (`StartRecording`, `StopRecording`, `PickColor`)
still answer with a stub `Error` until their stage lands (`src/dbus.rs`
names which).

**`Screenshot` can block for minutes, on purpose** (Stage 7): an interactive
`region` call does not return until the user confirms or cancels — the same
contract `slurp` has. Nothing times it out (`zbus::Connection`'s
`method_timeout` defaults to `None`) and nothing else on the bus is blocked
meanwhile (zbus dispatches each call on its own task). A cancel is a D-Bus
error (`the region selection was cancelled`), so `shot --region` exits **1**
with nothing saved. A second `shot --region` while one is up is refused
immediately (`a region selection is already in progress`) rather than
stacking a second exclusive-keyboard surface.

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
    in-process encoders replace it later.
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
    inputs).
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
  survey PLAN.md deferred.
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
  `storage::HistoryEntry`; Stage 16's library is its consumer. Clipboard and
  index failures **warn and continue** — the file is already on disk.
- **One runtime**: `zbus 5` with `default-features = false, features =
  ["tokio"]`; never a second async runtime. The **PipeWire main loop is the
  one sanctioned extra thread** — it bridges to the daemon via a bounded
  channel and is documented as the exception. Capture itself is *blocking*
  (Wayland roundtrips, ~0.3 s of compositor blit, an encode): the daemon runs
  it on `tokio::task::spawn_blocking`, guarded by `Handle::try_current()`
  because `spawn_blocking` panics outside a runtime (`dbus::run_blocking`).
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
    forces crate-root resolution: `::image::open(path)`
    (`modules::app::load_image`). No prior module needed both the iced
    widget half and the external crate's decode half in the same file.
  - **A plain `iced::application`'s `Message` needs `#[derive(Clone)]`**,
    the same requirement the daemon's `Message` has for an unrelated reason
    (`#[to_layer_message(multi)]`) — here it's `button`/`mouse_area`'s own
    bound when built through the `row!`/`container` helpers. Found by
    letting the compiler's own "consider annotating with `#[derive(Clone)]`"
    suggestion do the work, not by predicting it.
  - **`iced::Subscription::run_with<D: Hash>` cannot take a
    `zbus::Connection` as its keying data** — `Connection` isn't `Hash`.
    This is why `modules::app`'s Record tab doesn't (yet) subscribe to the
    `RecordingFinished` signal per-connection; Stage 12 needs a different
    shape (a connection-independent key, or a hand-rolled
    `iced::stream::channel` worker like `main.rs::dbus_worker_stream`).
  - `window::open_events()` is enough to learn a single-window
    `iced::application`'s own `window::Id` — no `window::latest()`/
    `oldest()` round trip needed when there's provably only ever one window.
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
  become optional later.
- **Toasts are interim.** A future **saola-notifications** component owns
  notifications; this app's toasts follow the style-guide card spec
  exactly, honor the `toasts false` config kill-switch, and the
  `io.saola.Capture1` signals (`CaptureTaken`, `RecordingStarted`,
  `RecordingFinished`, `Error`) are the stable contract that component will
  consume. Don't build a notification daemon here.
- **Redaction is a promise**: blur/pixelate must be irreversible in
  exported files.
- Recording is user-initiated only — no capture without an explicit user
  action (keybind, CLI, button); the tray item is visible for the entire
  duration of every recording.
