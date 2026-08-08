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

> Status: pre-implementation. PLAN.md is the staged build plan; Stage 1
> scaffolds the crate. Sections marked *(pending Stage N)* fill in as
> stages land.

## Commands

```sh
cargo build
cargo clippy --all-targets -- -D warnings   # warnings are errors, keep green
cargo test
cargo fmt --check

cargo run -- daemon                # the long-running daemon (layer-shell surfaces)
cargo run -- shot --fullscreen     # what the Print keybind invokes
cargo run -- shot --region         # overlay selection (needs the daemon)
cargo run -- shot --fullscreen --no-daemon --format=webp --output=/tmp  # headless/scriptable
cargo run -- record start|stop|toggle
cargo run -- window                # the app window / editor process
```

Live-testing anything that maps overlay surfaces or grabs the keyboard
happens in a **nested niri** (see Conventions), never the real session.

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
  over the frozen frame; crop in memory. No self-capture race.
- **Recording state lives in the daemon** and survives window closes; the
  PipeWire thread never blocks on the encoder (bounded channel, drop + log).
- `docs/CAPTURE-RESEARCH.md` *(pending Stage 2)* is the evidence of record
  for every capture-path decision (shm vs dmabuf, window-capture mechanism,
  audio transport, verified ffmpeg command lines). Do not re-litigate it
  from theory; extend it with new evidence.

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

## Conventions

- **No-panic rule**: no `panic!`/`unwrap`/`expect`/indexing on runtime
  paths. A dead daemon means `Print` silently does nothing — silent absence
  is the worst failure mode. Absent services (no daemon, no tray host, no
  ffmpeg) degrade gracefully or produce actionable errors, never crashes.
- **Teaching notes**: Jordan is newer to Rust — comment the non-obvious
  (async ownership, the pipewire thread bridge, SPA pods, zbus macros) as
  teaching notes; prefer explicit code over clever abstraction.
- **Dependency surveys**: every non-trivial dependency carries a dated
  `Cargo.toml` comment essay — alternatives considered and why they lost.
  Heavyweight deps and build-time C toolchains need strong justification.
  *(Surveys pending Stage 1: WebP encoder, clipboard, CLI parser; Stage 9:
  pipewire.)*
- **Config**: `~/.config/saola/capture.kdl`, `kdl = "6.7.1"`, hand-walked
  (no serde derive) for per-knob warnings; bad knob → warn + that knob's
  default; bad file → warn + all defaults, still start. Sibling resolution
  order (`--config-dir` > `$SAOLA_CONFIG_DIR` > `$XDG_CONFIG_HOME/saola` >
  `~/.config/saola`).
- **One runtime**: `zbus 5` with `default-features = false, features =
  ["tokio"]`; never a second async runtime. The **PipeWire main loop is the
  one sanctioned extra thread** — it bridges to the daemon via a bounded
  channel and is documented as the exception.
- "Every module maps to a signal, not a poll." Modules follow the sibling
  shape: state struct + `view(&Theme) -> Element` + `subscription()` +
  nested `Message` enum.
- **Testing**: pure logic (selection geometry, recorder state machine,
  config, undo/redo, swizzle/crop, blur kernels) unit-tested directly;
  buses/compositors behind traits with fakes. **The nested-niri rule
  (binding, from saola-lockscreen/CLAUDE.md)**: anything mapping overlay
  surfaces or grabbing the keyboard is live-tested against a nested niri
  first — spawn `niri -c /tmp/nested-niri.kdl &` *without* `--session`,
  override `NIRI_SOCKET` explicitly (your shell's points at the outer
  niri), run against the nested `WAYLAND_DISPLAY` only, tear down after.
  Never run input-grabbing tests in the real session without Jordan
  present.
- **Conventional Commits** (release-plz derives bumps); `chore:`/`ci:`/
  `docs:`/`test:` are changelog-invisible. Never hand-edit versions or
  `CHANGELOG.md`.

## Releases

release-plz in git-only mode, mirrored from saola-panel: release-pr +
release jobs, tags `saola-capture-v{version}`, PKGBUILD attached as a
release asset (not pushed to AUR), `0.1.0-dev` suffix as the prerelease
gate. Set up in Stage 16.

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
