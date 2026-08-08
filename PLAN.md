---
project_type: rust
max_retries: 1
on_failure: halt
---

# saola-capture — screenshots and screen recording for the Saola DE (v0.1)

## Context

Jordan is building "Saola", a Linux desktop environment in Rust targeting the
**niri** compositor. Four sibling projects exist and are the convention
sources — mirror them, don't reinvent:

- **saola-theme** (`~/Developer/saola-theme`) — the design system: iced 0.14
  integration + pure-data tokens. Consumed as a git dependency pinned to a
  **release tag** (`saola-theme-v0.5.0` today), never `branch = "main"`. Its
  `design/SAOLA-STYLE-GUIDE.md` (copied verbatim into this repo as
  `docs/SAOLA-STYLE-GUIDE.md`) already specifies this app's surfaces: the
  capture overlay (`scrims.capture`, `radii.selection` 6px, dashed terracotta
  selection edge with round handles, tabular-numeral size readout, floating
  toolbar — §7), the notification card (§6) and its timing (§5), and the
  new-surface checklist (§11).
- **saola-panel** (`~/Developer/saola-panel`) — the status bar; source of the
  repo layout, the `build_pattern::daemon` + `SurfaceRole` multi-surface
  layer-shell architecture (`src/main.rs`), the icon pattern (`src/icons.rs`),
  the `io.saola.*` D-Bus convention (`src/modules/claude.rs`), the KDL config
  style, and the avoid-heavyweight-deps rule (dependency-survey essays in
  `Cargo.toml`). It is also the **running SNI tray host** this app's recording
  indicator registers with.
- **saola-lockscreen** (`~/Developer/saola-lockscreen`) — its `CLAUDE.md`
  carries the binding rules that transfer here: the sudo rule, the
  **nested-niri testing rule** (verbatim procedure), the no-panic rule, the
  teaching-notes commenting style, and the image-decode-before-iced pattern
  (`src/wallpaper.rs`).
- **saola-session** (`~/Developer/saola-session`) — this PLAN.md's structural
  template; its `docs/SIGNALS.md` (a live D-Bus/IPC survey) already contains
  verified facts this project builds on.

Goal: a macOS-esque capture experience. `Print` → camera flash → toast
notification → clicking the toast opens the app on the screenshot with editing
tools. The app can also be opened directly to screenshot a region/window or
record the screen; triggering a capture hides the app window (Saola has no
minimize — the daemon's tray presence and overlay carry the interaction) until
the capture completes. Space-efficient formats first: WebP for images,
HEVC-in-MKV and AV1 for video; popular formats as the app matures.

**Platform facts (verified live on Jordan's machine, 2026-08-08):**

- niri 26.04 exposes `zwlr_screencopy_v1` (stills), `zwlr_layer_shell_v1`
  (overlays), `ext_foreign_toplevel_list_v1` (window enumeration), and serves
  `org.gnome.Mutter.ScreenCast` v4 **itself** on the session bus (PipeWire
  screencast, the same interface xdg-desktop-portal-gnome would call) plus
  `org.gnome.Shell.Screenshot` (full-screen only, and `PickColor`). niri-ipc
  (pin `=26.4.0`, same as the panel) provides window/output geometry, the
  `screenshot*` actions, and `ScreenshotCaptured`/`Casts*` events.
- **The xdg-desktop-portal Screenshot/ScreenCast path is broken by
  configuration on this machine** (niri's portal config wants
  xdg-desktop-portal-gnome, which is not installed; xdg-desktop-portal-wlr
  excludes niri from its `UseIn`). This app goes **direct to the compositor**
  — no portals, ever (see Boundaries in `CLAUDE.md`).
- **No encoder is installed**: no ffmpeg, no GStreamer encode plugins. The GPU
  (Radeon 680M, RDNA2) has VA-API hardware encode for H.264 and HEVC, **not**
  AV1. PipeWire 1.6.8 is fully live.
- `Print` is unbound in `~/.config/niri/config.kdl` and keybinds are niri
  `spawn` binds that **Jordan edits by hand** — no agent ever touches that
  file; print the exact bind lines instead.
- No notification daemon exists; the style guide reserves that for a future
  **saola-notifications** component. This app draws its own spec-compliant
  toast in the meantime (see Decisions).

**Decisions made with the user:**

- **Notifications**: saola-capture renders its own layer-shell toast per the
  style guide's notification card (§6) and timing (§5). Clicking it opens the
  editor. Every capture also broadcasts `io.saola.Capture1` signals so a
  future saola-notifications can take over rendering; a config kill-switch
  (`toasts false`) exists from day one so the handoff needs no code change.
- **Encoding**: **ffmpeg as an external CLI up front** (raw frames piped to
  stdin; `hevc_vaapi`→MKV primary preset, SVT-AV1 and H.264/MP4 presets),
  behind an `EncoderSink` trait so in-process encoders can later replace it
  or make ffmpeg optional. ffmpeg is never linked as a library; its absence
  is a clean, actionable runtime error, and the install command
  (`sudo pacman -S ffmpeg`) is printed for Jordan, never run.
- **Feature scope for v0.1**: fullscreen/region/window screenshots; screen
  recording with audio (mic/system/both, Opus); annotation editor (crop,
  arrow, rect, ellipse, freehand, text, numbered steps, blur/pixelate);
  capture history library; color picker; delayed capture; GIF/animated-WebP
  export.
- **CLI is a first-class scriptable interface**: e.g. `saola-capture shot
  --fullscreen --format=webp --output=$HOME/Pictures`, printing the saved
  path to stdout. `--no-daemon` captures fully in-process (headless, no
  flash/toast) for scripts; interactive pickers require the daemon.
- **License**: dual MIT OR Apache-2.0, matching the siblings (Stage 1
  replaces the current single Apache LICENSE).
- **saola-portal is deferred entirely.** Compositor portability lives behind
  the `CaptureBackend` trait and nowhere else.
- **Config format is TOML** (`capture.toml`; amended 2026-08-08 after
  Stage 3 landed, superseding the original KDL choice): Saola intends to
  outgrow niri — other compositors, possibly its own — so the config
  language optimizes for the general Linux audience (frozen TOML 1.0 spec,
  universal familiarity, mature tooling, no KDL v1/v2 boolean schism)
  rather than for matching niri's `config.kdl`. Stage 4 migrates this repo;
  Stages 4–18 of the original plan were renumbered 5–19 for it. The
  siblings (panel/lockscreen/session) and saola-theme's design doc migrate
  separately — recorded debt, not this repo's job. Documents written before
  the amendment (`docs/CAPTURE-RESEARCH.md` §8, handoffs 1–3) still use the
  old numbering: add 1 to any stage reference ≥ 4.
- The sudo rule: no agent runs `sudo` or edits Jordan's user/system config;
  exact commands are printed for him.

**Pre-plan live probes (2026-08-08)** already verified part of Stage 2 —
evidence in `docs/research/2026-08-08-probes/` (read its README before
Stage 2). Headlines: the full Mutter.ScreenCast handshake works, but the
session interface has **no `RecordArea`** (region recording =
RecordMonitor + crop) and the cast node advertises **dmabuf-only** (BGRx,
physical-res, MANDATORY modifier prop incl. LINEAR; no shm pod);
cursor-mode is the Mutter enum 0/1/2, not the portal bitmask; drag through
a grab overlay is pixel-exact; screencopy **composites overlay surfaces**
(freeze-before-map is mandatory); PickColor works and returns exact
colors; `niri msg casts` + cast events work as assumed.

**Risks this plan sequences around** (research first, riskiest subsystems
right after the MVP): iced_layershell's overlay fidelity (per-output
surfaces, exclusive keyboard from iced specifically) is unproven — Stage 2
confirms it or the overlay falls back to a hand-rolled
smithay-client-toolkit surface; SPA buffer negotiation — the cast node
advertises dmabuf-only, so plan for **dmabuf-first (LINEAR-modifier mmap
as the conservative path)** unless Stage 2 proves shm negotiable; ffmpeg
is absent until Stage 2 (Jordan installs it there, and the whole encode
chain is verified before any pipeline code exists); the window-capture
mechanism has three candidates and no winner until Stage 2 (RecordWindow
exists and takes `window-id (t)`, id-space match with niri-ipc untested);
A/V sync is decided by measurement in Stage 13; v0.1 may ship a
single-output-aware overlay with the limitation documented.

**Every stage subagent must first read the Architecture section of this file
(`PLAN.md` at repo root) and this repo's `CLAUDE.md`** (written in Stage 1;
carries the sudo rule, nested-niri rule, no-panic rule, design-language
rules, and Boundaries). Stages that touch UI must also read
`docs/SAOLA-STYLE-GUIDE.md` §5–§7 and §11.

## Architecture

**One binary crate, three run modes** dispatched in `main.rs`:

1. **`saola-capture daemon`** — the long-running heart: an `iced_layershell`
   `build_pattern::daemon` (copy the panel's `SurfaceRole` registry pattern
   from `~/Developer/saola-panel/src/main.rs`) owning every layer-shell
   surface (selection overlay, flash, toast stack, optional recording chip),
   the capture engine, the recording pipeline, the SNI tray item, and the
   `io.saola.Capture1` bus name. Recording state lives here and survives
   everything else. Autostarted via niri `spawn-at-startup` (printed for
   Jordan in Stage 17).
2. **`saola-capture window [edit <path>]`** — a separate process: a plain
   iced multi-window app (main window with Screenshot/Record modes, history
   library, annotation editor) on `Surface::Paper` as a regular niri
   toplevel. It is a D-Bus client of the daemon: capture buttons call the
   daemon and hide the window. The split is forced by the toolkit —
   iced_layershell's daemon hosts layer-shell surfaces only, not xdg
   toplevels — and D-Bus is the seam.
3. **CLI verbs** — `shot [--fullscreen|--region [--geometry WxH+X+Y]|--window]`,
   `record start|stop|toggle [--preset hevc|av1|h264] [--audio mic|system|both]`,
   `pick-color`, `open`. Flags (`--format`, `--output`, `--delay`,
   `--cursor`, `--copy/--no-copy`, `--no-toast`) override `capture.toml`
   defaults and travel as the D-Bus `a{sv}` options map; the saved path
   prints to stdout. Default behavior is a thin call against the warm daemon
   (this is what `Print` binds to), auto-spawning it detached (retry once) if
   the name is unowned. `--no-daemon` runs the capture in-process and
   headless via the same library code.

```
saola-capture/
├── Cargo.toml                  # survey essays on every non-trivial dep
├── src/
│   ├── main.rs                 # mode dispatch: daemon / window / CLI verbs
│   ├── config.rs               # ~/.config/saola/capture.toml (hand-walked TOML)
│   ├── icons.rs                # panel's include_bytes+tint pattern, local copy
│   ├── dbus.rs                 # io.saola.Capture1 service + client proxy
│   ├── capture/
│   │   ├── mod.rs              # CaptureBackend trait + Frame (RGBA8 + size + scale)
│   │   ├── screencopy.rs       # zwlr_screencopy_v1 — screenshots
│   │   └── screencast.rs       # Mutter.ScreenCast session + pipewire thread — video
│   ├── encode/
│   │   ├── mod.rs              # EncoderSink trait + presets
│   │   └── ffmpeg_cli.rs       # shell-out impl, rawvideo on stdin
│   ├── storage.rs              # save dir, filenames, history index, clipboard
│   └── modules/                # sibling pattern: state + view(&Theme) + subscription()
│       ├── overlay.rs  flash.rs  toast.rs  tray.rs  recorder.rs      # daemon
│       └── app.rs  editor.rs  history.rs  picker.rs                  # window process
├── docs/
│   ├── SAOLA-STYLE-GUIDE.md    # verbatim copy from saola-theme
│   ├── CAPTURE-RESEARCH.md     # Stage 2 — verified capture-path research
│   └── REVIEW-v0.1.md          # Stage 18
└── contrib/aur/PKGBUILD        # Stage 17
```

### Trait boundaries (binding)

New capture or encode paths go **behind these traits, never around them**:

- `CaptureBackend` (`capture/mod.rs`): `outputs()`, `capture_output(id,
  cursor) -> Frame`, `capture_region(id, rect, cursor) -> Frame` (crop of a
  full output capture), `capture_window(ref) -> Frame` (mechanism decided by
  Stage 2). `Frame` is RGBA8 + dimensions + scale. Future compositor or
  portal portability lives here and nowhere else.
- `EncoderSink` (`encode/mod.rs`): `start(video_spec, audio_spec, preset,
  path)`, `write_video(frame, pts)`, `write_audio(pcm, pts)`, `finish() ->
  PathBuf`. ffmpeg CLI is the only v0.1 implementation; the trait is what
  makes it swappable/optional later.

### Data flows (binding)

- **PrintScr**: niri bind spawns `saola-capture shot --fullscreen` → D-Bus →
  daemon: screencopy frame → RGBA → WebP (+PNG if configured) → save +
  clipboard → flash surface → toast with thumbnail → `CaptureTaken(path,
  kind)` signal.
- **Region**: daemon captures the target output **first** (frozen frame — no
  race with the overlay's own pixels, exact crop source), then maps the
  overlay: frozen frame + `scrims.capture` outside the selection + dashed
  terracotta edge at `radii.selection` + 8 round handles + tabular size
  readout + floating toolbar. Confirm → crop in memory → same tail as
  PrintScr. Escape → unmap, nothing saved.
- **Recording**: Mutter.ScreenCast `CreateSession` →
  `RecordMonitor`/`RecordWindow` (there is no `RecordArea` on niri —
  region recording is a monitor cast cropped before encode) → `Start` →
  PipeWire node id →
  pipewire-rs stream on a **dedicated thread** (the pw main loop is not
  tokio; frames cross a bounded channel; teaching-note the threading) →
  `EncoderSink` (ffmpeg child: rawvideo stdin, `hwupload,hevc_vaapi`,
  MKV) → SNI item shows recording; stop (tray/CLI) → drain → `finish()` →
  toast + `RecordingFinished(path)`.
- **Toast click**: spawn detached `saola-capture window edit <path>`
  (screenshots) or open the containing directory (videos, until the editor
  learns video) → toast dismissed.

### D-Bus interface `io.saola.Capture1` (firmed up in Stage 3)

Path `/io/saola/Capture1`. Methods: `Screenshot(kind s, options a{sv}) → s`,
`StartRecording(kind s, options a{sv})`, `StopRecording() → s`,
`PickColor() → (ddd)`, `OpenWindow(mode s)`. Signals: `CaptureTaken(path s,
kind s)`, `RecordingStarted(kind s)`, `RecordingFinished(path s)`,
`Error(message s)`. **The signals are the future saola-notifications
contract — keep them stable.**

### Backpressure and failure posture (binding)

- The PipeWire thread never blocks on the encoder: bounded channel, drop
  frames and log when full.
- Audio failure degrades to video-only with a warning toast — never a dead
  pipeline.
- No `panic!`/`unwrap`/`expect` on runtime paths (clippy-enforced, sibling
  rule): a crashed daemon means `Print` silently does nothing, and toasts
  mean nobody is watching a terminal to notice.
- Absent services (no daemon for the CLI, no tray host, no ffmpeg) produce
  actionable errors or graceful degradation, never crashes.

### Testing strategy

- Pure logic (selection geometry, recorder state machine, config parsing,
  filename generation, undo/redo) is unit-tested directly; buses and
  compositors are behind traits with fakes (the sleep-module pattern in
  `~/Developer/saola-session/src/modules/sleep.rs`).
- Anything that maps overlay surfaces or grabs keyboard is live-tested in a
  **nested niri** first (the lockscreen `CLAUDE.md` procedure, verbatim);
  never `--session`, never input-grabbing tests in Jordan's real session
  without him present.
- Recording end-to-end checks are Jordan-run (real session, real GPU);
  stages print the exact commands and wait for his confirmation.
- Gates for every code stage: `cargo build && cargo clippy --all-targets --
  -D warnings && cargo test` plus `cargo fmt --check` in CI.

## Stage 1 — Repo skeleton + dependency survey

```yaml
model: sonnet
effort: medium
tools: [Read, Write, Edit, Bash, Glob, Grep]
verify:
  files:
    - Cargo.toml
    - rust-toolchain.toml
    - rustfmt.toml
    - CLAUDE.md
    - AGENTS.md
    - LICENSE-MIT
    - LICENSE-APACHE
    - docs/SAOLA-STYLE-GUIDE.md
    - src/main.rs
  command: cargo build && cargo clippy --all-targets -- -D warnings
```

Read Architecture above, then `~/Developer/saola-panel/`'s `Cargo.toml`,
`rust-toolchain.toml`, `rustfmt.toml`, `CLAUDE.md`, and
`~/Developer/saola-lockscreen/CLAUDE.md`.

1. `cargo init` in the existing repo (keep `.gitignore`, `README.md`; the
   dual `LICENSE-MIT`/`LICENSE-APACHE` pair, `docs/SAOLA-STYLE-GUIDE.md`,
   `CLAUDE.md` and `AGENTS.md` already exist from the planning session —
   verify, don't recreate). Set `license = "MIT OR Apache-2.0"`. Mirror
   `rust-toolchain.toml` and `rustfmt.toml` verbatim from saola-panel.
2. Dependencies — survey before pinning, each choice recorded as a
   `Cargo.toml` comment essay in the siblings' format (alternatives, date,
   why): `iced 0.14` (features: wayland, tokio, svg, image, advanced, canvas
   — verify against what the panel/lockscreen enable), `iced_layershell 0.19`,
   `saola-theme` pinned `tag = "saola-theme-v0.5.0"` with matching `version`,
   `zbus 5` (`default-features = false, features = ["tokio"]` — one runtime,
   sibling rule), `niri-ipc = "=26.4.0"` (exact pin, same comment as the
   panel), `wayland-client 0.31` + `wayland-protocols` +
   `wayland-protocols-wlr` (verify the screencopy feature flag and record
   it), `kdl = "6.7.1"`, `image 0.25` + a WebP encoder (the `image` crate
   cannot do lossy WebP — survey `webp` (libwebp wrapper) vs alternatives;
   record the build-time implications against the house no-C-toolchain
   aversion and pick), a clipboard approach (`wl-clipboard-rs` vs spawning
   `wl-copy` — survey and record; note wl-clipboard is not currently
   installed), a CLI arg parser (`clap` vs `lexopt`/`pico-args` under the
   light-deps rule — record), `pipewire` crate (survey now, note version and
   the SPA story, but do **not** add it yet — Stage 10 adds it with Stage 2's
   evidence in hand).
3. `src/main.rs` compiles to a stub that dispatches
   `daemon`/`window`/`shot`/`record`/`pick-color`/`open` subcommands to
   `todo-style` stubs that print and exit 0 (no `todo!()` — no-panic rule),
   plus `--version`.
4. Update the existing `CLAUDE.md` (written in the planning session; its
   structure and Boundaries are binding — do not restructure): fill in the
   Commands section with the real invocations, and append the dependency
   survey outcomes where the file marks them pending. **CLAUDE.md must be
   kept current as stages land** — every stage handoff notes anything that
   changes it.

Handoff: exact versions resolved, the WebP/clipboard/arg-parser picks and
why, the screencopy feature flag verified, any surprises.

## Stage 2 — Capture research: prove every capture path before building on it

```yaml
model: opus
effort: high
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [1]
verify:
  files:
    - docs/CAPTURE-RESEARCH.md
```

The load-bearing unknowns, resolved with **evidence** (command output, source
references, transcripts — not inference), in the mold of
`~/Developer/saola-session/docs/SIGNALS.md`. **Start from
`docs/research/2026-08-08-probes/README.md`** — the pre-plan probes already
answered several items below (marked); fold their findings into
`docs/CAPTURE-RESEARCH.md` rather than re-running them, and focus effort on
the listed gaps. Throwaway prototypes go in the scratch directory, never
this repo. Split the work across two subagents if useful (stills + overlay
vs screencast + encode). Produce `docs/CAPTURE-RESEARCH.md`:

1. **Screencopy handshake** against the real niri (read-only, safe):
   formats offered per output, y-invert flag behavior, cursor compositing
   options, per-output vs region semantics. grim's source is the reference
   implementation; `grim` is installed for output comparison.
2. **Mutter.ScreenCast v4** *(largely pre-answered — see probes)*: the
   handshake transcript exists; the node advertises **dmabuf-only** (BGRx,
   physical-res, MANDATORY modifier incl. LINEAR), no `RecordArea`,
   Mutter-enum cursor-mode. The remaining question: **is shm negotiable
   anyway?** Write a minimal pipewire-rs consumer offering BGRx without
   the modifier prop and inspect the negotiated Buffers `dataType`. Close
   with the **buffer-path decision and fallback chain** (expected:
   dmabuf-with-LINEAR mmap as conservative primary if shm is refused).
3. **ffmpeg encode chain**: ffmpeg is not installed — print
   `sudo pacman -S ffmpeg` for Jordan and **wait for his confirmation**.
   Then verify `ffmpeg -encoders | grep vaapi`, and run a synthetic
   smoke test: generated rawvideo on stdin → `hwupload` → `hevc_vaapi` → MKV,
   confirming hardware encode works on the 680M and recording the exact
   working command line (Stage 11 reuses it verbatim). Also test the AV1
   preset (`libsvtav1`, software) and H.264/MP4.
4. **Audio transport decision**: PCM piped on `pipe:3` vs ffmpeg pulling
   from the PipeWire pulse shim (`-f pulse -i <device>`), including how each
   affects A/V sync and device selection. Decide what Stage 13 implements.
5. **Window capture mechanism**: three candidates — niri-ipc
   `screenshot-window` action (where does the file land? is there pixel
   access without disk?), geometry-crop from an output screencopy using
   niri-ipc/foreign-toplevel geometry (what about overlapping floating
   windows?), or a one-frame `RecordWindow` cast (probes confirmed it
   takes `window-id (t)` with validation deferred to Start; check the id
   space matches niri-ipc ids — needs a consumer attached). Note
   `ext_image_copy_capture` is absent on niri 26.04, so grim-style `-T`
   toplevel capture is not a candidate. Decide, with evidence.
6. **Overlay viability in iced_layershell** (nested niri, lockscreen
   procedure): per-output overlay surfaces, `KeyboardInteractivity::Exclusive`
   with Escape, pointer drag fidelity on `Layer::Overlay`, and whether a
   frozen-frame image background at output size renders acceptably. If any
   of this fails, document the smithay-client-toolkit fallback shape.
7. **Enumeration** *(pre-answered — see probes)*: `niri msg casts` and the
   `CastsChanged`/`CastStartedOrChanged`/`CastStopped` events are
   confirmed working; verify the `ext_foreign_toplevel_list_v1` window
   listing shape is all that remains.

Close with a **decision section** — every later stage keys off it; do not
guess where evidence is thin, say what would firm it up.

Handoff: the decision section verbatim, plus anything surprising about the
probes.

## Stage 3 — Config + CLI + D-Bus dispatch + daemon scaffold

```yaml
model: sonnet
effort: medium
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [2]
verify:
  files:
    - src/config.rs
    - src/dbus.rs
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Read Architecture, Stage 2's handoff, the panel's `src/config.rs` (KDL style)
and `src/modules/claude.rs` (io.saola D-Bus conventions).

1. `config.rs`: parse `~/.config/saola/capture.kdl` (sibling resolution
   order) — `save-dir`, `image-format webp|png` (+ `png-also <bool>`),
   `video-preset hevc|av1|h264`, `cursor <bool>`, `delay <secs>`,
   `toasts <bool>` (the saola-notifications kill-switch), `copy <bool>`.
   Hand-walked KDL, per-knob warn+default, unit tests with fixture strings
   including missing-file and nonsense-value paths.
2. Full CLI parsing (Stage 1's chosen parser): the subcommands and flags in
   Architecture, flags overriding config into a resolved `CaptureOptions`;
   unit tests for flag parsing and precedence. Saved path prints to stdout;
   errors to stderr with nonzero exit.
3. `dbus.rs`: serve `io.saola.Capture1` in the daemon (methods stubbed to
   log-and-Error for now, signals defined); client proxy for CLI/window;
   CLI auto-spawns the daemon detached and retries once when the name is
   unowned; a second `daemon` invocation exits cleanly when the name is
   taken (single-instance).
4. The daemon boots as a surfaceless iced_layershell daemon with the
   SurfaceRole registry in place (no surfaces yet) and clean SIGTERM/SIGINT
   shutdown.

Handoff: the interface as built (`busctl --user introspect` output), the
config schema verbatim (Stage 17's README reuses it), how surfaces get
spawned at runtime.

## Stage 4 — Config migration: KDL → TOML

```yaml
model: sonnet
effort: medium
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [3]
verify:
  files:
    - src/config.rs
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

*(Added 2026-08-08 — see the config-format decision in Context. Stage 3
built `config.rs` against KDL; this stage replaces the format before
anything else consumes it.)*

Read Stage 3's handoff (the KDL schema and the `CaptureConfig` API it
documents) and the current `src/config.rs`.

1. Survey the TOML crate in `Cargo.toml`'s essay format (dated;
   `toml` vs `toml_edit` vs `basic-toml` — this is a read-only consumer,
   so format-preserving editing buys nothing; record the tradeoffs and
   the resolved version). Swap `kdl` out for the pick; remove the `kdl`
   dependency entirely.
2. Rewrite `config.rs` for `~/.config/saola/capture.toml`: same resolution
   order (`--config-dir` > `$SAOLA_CONFIG_DIR` > `$XDG_CONFIG_HOME/saola` >
   `~/.config/saola`), same knobs, same kebab-case names (bare keys —
   TOML allows dashes unquoted), same defaults, and the same public
   `CaptureConfig` API so no caller in `main.rs`/`cli.rs` changes. Keep
   the hand-walked posture — walk the parsed table, no serde derive —
   with per-knob warn+default; bad file → warn + all defaults, still
   start. Top-level keys, no `[capture]` wrapper table (the file is
   already capture's own; record the decision in a comment).
3. Port every fixture test (missing file, garbage file, per-knob nonsense
   values, tilde expansion, resolution-order precedence); the test count
   must not drop below Stage 3's.
4. If `capture.kdl` exists in the resolved config dir and `capture.toml`
   does not, log a one-line migration hint naming both paths (a warning,
   not an error — defaults still apply).
5. Update `CLAUDE.md` (keep-current rule): the Config convention bullet
   (file name, crate, TOML schema verbatim, drop the `#true`/`#false`
   gotcha), the Commands example mentioning `capture.kdl`, and the Status
   block.

Handoff: the TOML schema verbatim (Stage 17's README consumes it), the
crate survey outcome, confirmation the `CaptureConfig` API is unchanged
(so Stage 3's handoff stays accurate apart from the file format), and the
sibling/design-doc migration debt note.

## Stage 5 — Screencopy backend + WebP/PNG save + clipboard

```yaml
model: opus
effort: high
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [4]
verify:
  files:
    - src/capture/mod.rs
    - src/capture/screencopy.rs
    - src/storage.rs
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Read Architecture (trait boundary, data flows), Stage 2's decision section
(formats, y-invert), and the lockscreen's `wallpaper.rs` (decode-before-iced
precedent).

1. `capture/mod.rs`: `CaptureBackend` trait + `Frame` exactly as
   Architecture specifies, with teaching-note docs.
2. `capture/screencopy.rs`: the `zwlr_screencopy_v1` implementation per
   Stage 2's findings — shm buffers, format swizzle (BGRA/XRGB → RGBA),
   y-invert handling, cursor option. Unit-test swizzle and crop math on
   synthetic buffers (stride ≠ width×4, odd sizes, y-inverted).
3. `storage.rs`: save-dir resolution (config → `~/Pictures/Captures`
   fallback, created on demand), timestamp filenames, WebP encode (Stage 1's
   pick; quality knob from config) + optional PNG, clipboard copy (Stage 1's
   pick), and the beginnings of the history index (a simple on-disk index
   the Stage 16 library reads — keep the format documented and boring).
4. Wire `shot --fullscreen` end-to-end **both ways**: via the daemon D-Bus
   path and via `--no-daemon` in-process (same library calls, no UI). Path
   prints to stdout.
5. Live check (nested niri, procedure from `CLAUDE.md`): `shot --fullscreen
   --no-daemon` against the nested compositor produces a correct WebP
   (compare against `grim` output). Print the commands for Jordan if a real-
   session check is wanted; do not run input-grabbing tests yourself.

Handoff: formats/y-invert realities vs Stage 2's predictions, the trait as
landed, clipboard behavior, index format.

## Stage 6 — Flash + toast: the PrintScr MVP

```yaml
model: sonnet
effort: medium
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [5]
verify:
  files:
    - src/modules/flash.rs
    - src/modules/toast.rs
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Read `docs/SAOLA-STYLE-GUIDE.md` §5 (timing), §6 (notification card), §11
(new-surface checklist), and the panel's popover/surface code.

1. `modules/flash.rs`: full-output overlay surface, click-through, a ~150 ms
   ivory fade (motion per §5's family; every value from tokens).
2. `modules/toast.rs`: the §6 card verbatim — 440 px ink card, 26 px radius,
   thumbnail in the 36 px icon tile, 3 px life rule animating the §5 timing
   (350 ms slide-in, 5 s rest, 1 s fade, stack of 3 replacing oldest, hover
   pauses). Click → spawn detached `saola-capture window edit <path>` (the
   window mode is still a stub — that's fine, it must at least print).
   Honor `toasts false`.
3. Wire the PrintScr flow end-to-end: `shot --fullscreen` via daemon now
   flashes, toasts, saves, copies, and emits `CaptureTaken`.
4. Walk §11's checklist for both new surfaces in a code comment.
5. Print for Jordan the exact niri bind lines (he edits the config himself):
   `Print { spawn "saola-capture" "shot" "--fullscreen"; }` with
   `hotkey-overlay-title`, and suggest `Mod+Shift+S` for region once Stage 7
   lands. **Human-verify with Jordan (real session, safe — no input grab):
   press Print → flash + toast + WebP on disk. This is the usable-MVP
   milestone.**

Handoff: toast lifecycle wiring, any saola-theme style gaps hit (if a style
was missing and added to saola-theme, note the required tag bump — never
restyle locally).

## Stage 7 — Region selection overlay

```yaml
model: opus
effort: high
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [6]
verify:
  files:
    - src/modules/overlay.rs
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Read the style guide §7 (capture overlay row: dashed terracotta selection
edge, round terracotta handles, size readout, floating toolbar), §2
(`scrims.capture`), §4 (`radii.selection`), and Stage 2's overlay findings.

1. `modules/overlay.rs`: frozen-frame background (capture first, then map —
   Architecture's region flow), `scrims.capture` outside the selection,
   dashed terracotta edge at `radii.selection`, 8 round drag handles,
   drag-to-create + move + resize, tabular-numeral size readout, floating
   toolbar (confirm / cancel / fullscreen / window mode toggle), Escape
   cancels, `KeyboardInteractivity::Exclusive` per Stage 2.
2. The geometry core (hit-testing, handle drag, clamping, snap) is pure
   functions with exhaustive unit tests.
3. Wire `shot --region` end-to-end; `--geometry WxH+X+Y` skips the overlay
   entirely (scriptable path). Single-output is acceptable for v0.1 if
   Stage 2 found multi-output spawning hard — document the limitation.
4. Live check (nested niri): full drag/adjust/confirm/cancel round-trip.
   **Never test exclusive-keyboard surfaces in the real session without
   Jordan present.**

Handoff: the overlay input model, multi-output posture, theme gaps.

## Stage 8 — Window capture + delayed capture

```yaml
model: sonnet
effort: medium
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [7]
verify:
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Read Stage 2's window-capture decision and Stage 7's overlay input model.

1. Implement `shot --window` per Stage 2's decided mechanism (window pick
   via click-through-overlay highlight or toolbar list — whichever Stage 2's
   evidence supports; geometry from niri-ipc/foreign-toplevel).
2. Delayed capture: `--delay N` and the config default — countdown surfaces
   as toast-style pill or overlay badge; applies to all three shot kinds.
3. Edge cases per Stage 2: floating overlapping windows, fractional scale.

Handoff: mechanism as shipped, edge cases verified, countdown UX as built.

## Stage 9 — Main app window (window process)

```yaml
model: sonnet
effort: medium
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [8]
verify:
  files:
    - src/modules/app.rs
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Read the style guide §7 (window chrome: paper surface, 46 px header), §11,
and Architecture's process-model rationale.

1. `modules/app.rs`: the `saola-capture window` iced app — `Surface::Paper`
   toplevel, Screenshot/Record mode tabs (segmented control from
   saola-theme), target picker (fullscreen/region/window), options (delay,
   cursor, format/preset, audio — audio is inert until Stage 13), a
   prominent capture/record button.
2. Capture buttons D-Bus-call the daemon and hide the window until the
   `CaptureTaken`/`RecordingFinished` signal (Saola has no minimize — hidden
   window + daemon presence is the model); reopening via `open` verb, toast
   click, or tray menu.
3. `window edit <path>` opens straight into the (stub) editor view with the
   image displayed — Stage 14 fills in the tools.
4. Walk §11's checklist in a comment.

Handoff: window↔daemon protocol as used, hide/reopen behavior, editor stub
shape for Stage 14.

## Stage 10 — ScreenCast session + PipeWire frames

```yaml
model: opus
effort: high
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [6]
verify:
  files:
    - src/capture/screencast.rs
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Parallel-safe with Stages 7–9. Read Stage 2's ScreenCast transcript and
shm/dmabuf decision; add the `pipewire` crate now with its survey essay.

1. `capture/screencast.rs`: Mutter.ScreenCast session lifecycle
   (CreateSession → Record* → Start → node id → Stop), zbus-side in the
   daemon's runtime.
2. The PipeWire stream runs on a **dedicated thread** (pw main loop ≠
   tokio): SPA buffer negotiation per Stage 2's decision (probes showed
   the node advertises dmabuf-only with LINEAR available — expect
   dmabuf/LINEAR mmap primary; shm only if
   green-lit), frames crossing a bounded channel to the daemon side.
   Teaching-note the threading and the SPA pod handling — this is the
   hardest plumbing in the project.
3. Clean teardown in both directions (session closed vs stream error), and
   `CastsChanged` awareness via niri-ipc.
4. `record start --dry-run`: negotiates, logs format + frame cadence for 5
   seconds, writes nothing, tears down. **Human-verify with Jordan: dry-run
   in the real session shows steady frames at the negotiated size.**

Handoff: the exact negotiated SPA format struct, stride/padding gotchas,
teardown ordering — Stage 11 consumes this verbatim.

## Stage 11 — EncoderSink + ffmpeg CLI: recording end-to-end

```yaml
model: opus
effort: high
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [10]
verify:
  files:
    - src/encode/mod.rs
    - src/encode/ffmpeg_cli.rs
    - src/modules/recorder.rs
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Read Stage 2's verified ffmpeg command lines and Stage 10's handoff.

1. `encode/mod.rs`: the `EncoderSink` trait as Architecture specifies, plus
   `EncodePreset` (hevc→MKV primary, av1→MKV, h264→MP4) with the argument
   tables from Stage 2.
2. `encode/ffmpeg_cli.rs`: spawn ffmpeg (rawvideo on stdin, preset args),
   stderr drained to the log (never let the pipe fill), kill-on-drop +
   zombie reaping, missing-ffmpeg detected up front with a clean error
   naming `sudo pacman -S ffmpeg`, disk-full and mid-stream-death surfaced
   as `Error` signal + toast.
3. **VAAPI device + AV1 capability probe (runtime, never hardcoded)**
   *(amendment 2026-08-08, agreed with Jordan)*: do not bake
   `/dev/dri/renderD128` into the presets — Jordan's dGPU is normally
   disabled but can appear, adding a second render node and potentially
   reshuffling which device owns `renderD128`. At encoder start, enumerate
   `/dev/dri/renderD*` and pick the node: prefer one with a working AV1
   encode entrypoint, else the first with a working encode entrypoint for
   the chosen preset. Probe via a tiny ffmpeg null-sink trial encode per
   candidate (`-f lavfi -i color=…` → `hwupload` → encoder → `-f null -`) —
   keeps ffmpeg the sole external CLI; do **not** add a runtime `vainfo`
   dependency. Cache the result per daemon run. The AV1 preset uses
   `av1_vaapi` when the chosen node supports it, else falls back to
   `libsvtav1` (the software path Stage 2 §3.3/§3.4 characterizes, 30 fps
   cap). Add an optional `vaapi-device` override knob to `capture.toml`
   (per-knob warn+default convention). No usable VAAPI node → hevc/h264
   presets fail with an actionable error; av1 still works via software.
   Selection logic is pure and unit-tested with fake probe results.
4. `modules/recorder.rs`: the state machine (Idle → Starting → Recording →
   Stopping → Idle) unit-tested against a fake sink, including backpressure
   (bounded channel full → drop frame + count + log, never block the pw
   thread), stop-while-starting, encoder-death-while-recording.
5. Wire `record start` / `record stop` / `record toggle` end-to-end for
   fullscreen. **Human-verify with Jordan: a 10 s real-session recording
   plays in mpv, and the ffmpeg log confirms `hevc_vaapi` (hardware) was
   used.**

Handoff: sink trait as landed, preset argument tables, the device-probe
selection behavior (and what it chose on the live iGPU-only machine),
measured frame-drop behavior, the A/V timing model prepared for Stage 13.

## Stage 12 — Recording UX: tray item, region/window recording

```yaml
model: sonnet
effort: medium
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [11, 9]
verify:
  files:
    - src/modules/tray.rs
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Read the panel's `src/modules/tray/` (host side — the counterpart of the
item this stage builds) and Architecture's recording flow.

1. `modules/tray.rs`: a StatusNotifierItem served from the daemon via zbus —
   idle and recording icon states (record glyph is one of the sanctioned
   solid icons), menu: Stop recording / Open Saola Capture / Quit daemon.
   The panel is the live host; degrade silently if no host (sibling rule:
   absent service → render nothing).
2. Recording elapsed-time: tray tooltip/title; add the small chip surface
   only if Stage 2/7 evidence says a layer-shell pill is cheap — otherwise
   note it as future work.
3. `record start --region` (overlay reuse selects the rect; there is no
   `RecordArea` — record the monitor and crop before encode, per Stage
   2's decision on where the crop runs cheapest) and
   `record start --window` (→ `RecordWindow`); finish toast per
   Architecture's toast-click flow (videos open containing dir for now).
4. `RecordingStarted`/`RecordingFinished` signals wired; the app window's
   Record tab now drives real recordings and hides while recording.
   **Human-verify with Jordan: record a region from the app window, stop
   from the panel tray, toast appears, file plays.**

Handoff: SNI registration details (any panel quirks found), chip decision,
elapsed-time source.

## Stage 13 — Audio capture → Opus track

```yaml
model: opus
effort: high
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [11]
verify:
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Read Stage 2's audio-transport decision and Stage 11's timing model.

1. Implement the decided transport (PCM on `pipe:3` from a PipeWire capture
   stream, or ffmpeg `-f pulse` inputs) for mic, system audio, or both;
   config + `--audio` flag + app-window toggle select it; Opus in MKV.
2. A/V sync verified by measurement (clap test — record a sharp sound with
   visible cause, measure offset in the file); pick the PTS source of truth
   accordingly and document it.
3. Audio failure (missing device, stream death) degrades to video-only with
   a warning toast — never a dead pipeline; unit-test the degradation path
   with fakes.

Handoff: transport as shipped, measured sync offset, device-selection notes.

## Stage 14 — Annotation editor I: canvas, crop, shapes

```yaml
model: sonnet
effort: high
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [9]
verify:
  files:
    - src/modules/editor.rs
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Parallel-safe with 10–13. Read the style guide (tool palette styling comes
from saola-theme; terracotta is the default annotation color), the
saola-theme `CLAUDE.md` iced-0.14-gotchas section, and Stage 9's editor stub.

1. `modules/editor.rs`: iced canvas over the image — tools: crop (drag +
   confirm), arrow, rectangle, ellipse, freehand; stroke width and the
   restrained color set (terracotta default; ink/ivory alternates); select/
   move/delete of placed annotations; undo/redo stack.
2. The annotation model and undo/redo are pure data + functions, unit-tested;
   rendering composes annotations over the RGBA base at save time
   (re-encode WebP/PNG via `storage.rs`), plus copy-to-clipboard of the
   composed result.
3. Toast click and history open now land in a real editor. Save-as dialog
   can be a path field for v0.1 (no portal FileChooser — Boundaries).

Handoff: canvas architecture, tool-state model (Stage 15 extends it),
raster-composition performance notes.

## Stage 15 — Annotation editor II: text, steps, blur/pixelate

```yaml
model: sonnet
effort: medium
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [14]
verify:
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Read Stage 14's tool-state model.

1. Text tool (IBM Plex Sans via theme fonts, size stops from tokens),
   numbered-step badges (auto-incrementing terracotta discs, ivory
   numerals), blur and pixelate region tools (CPU raster ops on the image
   buffer — fine at screenshot sizes; unit-test the kernels on synthetic
   images).
2. Export panel: format (WebP/PNG), quality, copy vs save vs both.
3. Blur/pixelate compose destructively into the export (that is the point —
   redaction), but stay editable in-session via the undo stack; make the
   redaction semantics explicit in a teaching note (a blur that can be
   trivially reversed from the exported file is a privacy bug).

Handoff: kernel performance at 4K, redaction semantics as shipped.

## Stage 16 — History library, color picker, GIF export

```yaml
model: sonnet
effort: medium
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [12, 15]
verify:
  files:
    - src/modules/history.rs
    - src/modules/picker.rs
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Read Stage 5's index format and Stage 11's preset tables.

1. `modules/history.rs`: a browsable grid of past captures (thumbnails from
   the index; screenshots and recordings), actions: open/edit, copy, show
   in folder, delete (with confirm — wording carries severity, no red).
2. `modules/picker.rs`: `pick-color` verb + app button → niri's
   `org.gnome.Shell.Screenshot.PickColor` → swatch toast + hex (mono font)
   copied to clipboard.
3. GIF and animated-WebP export of recordings (ffmpeg `palettegen`/
   `paletteuse` two-pass; anim-WebP preset) as a history/editor action with
   a size warning teaching-note in the UI copy.

Handoff: index/library behavior, export presets, PickColor quirks.

## Stage 17 — CI, packaging, autostart, README

```yaml
model: haiku
effort: low
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [16]
verify:
  files:
    - .github/workflows/ci.yml
    - .github/workflows/release-plz.yml
    - .github/workflows/pkgbuild-release.yml
    - release-plz.toml
    - contrib/aur/PKGBUILD
    - README.md
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Mirror the panel's `.github/workflows/`, `release-plz.toml`, and
`contrib/aur/PKGBUILD` (adjust `APT_BUILD_DEPS` for this crate's native
needs — wayland, pipewire headers; PKGBUILD `depends` gains `ffmpeg`, plus
the clipboard tool if Stage 1 chose spawning). `CHANGELOG.md` seeded for
release-plz; version stays `0.1.0-dev` (the siblings' prerelease gate).

`README.md`: what it is, install, the full `capture.toml` reference (Stage
4's schema verbatim), CLI examples (including the scriptable
`--no-daemon`/`--geometry`/stdout-path forms), and a **"Jordan runs these"**
section: the niri bind lines (`Print`, `Mod+Shift+S` region, `Mod+Shift+R`
record toggle, each with `hotkey-overlay-title`), the
`spawn-at-startup "saola-capture" "daemon"` line, `sudo pacman -S ffmpeg`,
and the known-limitations list (no portals by design, single-output overlay
if applicable, saola-notifications handoff note).

Handoff: the exact commands Jordan runs, CI status expectations.

## Stage 18 — Review: capture-integrity and silent-failure audit (read-only)

```yaml
model: opus
effort: high
tools: [Read, Grep, Glob, Bash]
depends_on: [17]
verify:
  files:
    - docs/REVIEW-v0.1.md
```

Read-only adversarial review of the whole crate;
`~/Developer/saola-session/docs/REVIEW-v0.1.md` is the rigor bar. Bash is
for `cargo clippy`/`cargo test`/`cargo tree` and read-only inspection — NO
source edits; findings go in the report for Stage 19. Consider a two-agent
split: one on the capture/encode/threading core, one on UI/D-Bus/storage.

Audit at minimum:

1. **Panic surface** on runtime paths (clippy-verified) — a dead daemon
   means Print silently does nothing.
2. **The pw-thread ↔ encoder pipeline**: deadlock windows, backpressure
   actually dropping (not blocking), teardown races (stop while starting,
   encoder death mid-frame, session revoked by compositor).
3. **ffmpeg child lifecycle**: zombies, kill-on-drop, stderr pipe fill,
   disk-full, output-file collision.
4. **D-Bus surface abuse**: unauthenticated session-bus peers starting
   recordings or spamming Screenshot — acceptable on a session bus, but
   memory/rate must be bounded and Error paths clean.
5. **Overlay failure modes**: stuck `Exclusive` keyboard (crash while
   overlay mapped), orphaned frozen-frame surfaces, multi-output confusion.
6. **Format/extension mismatches**, history-index corruption handling,
   clipboard lifetime.
7. **Theme compliance**: grep for hardcoded colors/sizes (must be zero),
   solid-icon rule, tabular numerals on readouts.
8. **Dependency review**: `cargo tree -e normal` for surprises; every
   non-trivial dep has its survey essay.

Write `docs/REVIEW-v0.1.md`: severity-ordered findings with file:line and
fix sketches; explicitly record what was checked and found clean.

## Stage 19 — Fixes + release prep

```yaml
model: sonnet
effort: medium
tools: [Read, Write, Edit, Bash, Glob, Grep]
depends_on: [18]
verify:
  files:
    - CHANGELOG.md
  command: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
```

Read `docs/REVIEW-v0.1.md` first; fix every must-fix, document deliberate
won't-fixes with reasoning. Final README/CHANGELOG pass; ensure `CLAUDE.md`
reflects everything that changed since Stage 1 (the keep-current rule).
Version stays `0.1.0-dev` — tagging is Jordan's call after he runs the
end-to-end sequence this stage writes into the README: Print screenshot,
region + window shots, a 30 s recording with system audio stopped from the
tray, an annotate-and-export round trip, a GIF export, and `--no-daemon`
scripted capture.

Handoff: findings fixed vs deferred, the end-to-end sequence's expected
observations, anything left for v0.2 (in-process encoders / optional
ffmpeg, video trimming in the editor, saola-notifications handoff,
multi-output overlay if deferred, saola-icons migration).
