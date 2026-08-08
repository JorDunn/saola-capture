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

> Status: Stages 1–4 landed (repo skeleton, dependency survey; every capture
> path proven with live evidence in `docs/CAPTURE-RESEARCH.md`; full CLI
> parsing, `capture.toml` config, the `io.saola.Capture1` bus, and a
> surfaceless daemon boot). Every CLI verb now really talks to the daemon
> over D-Bus — auto-spawning it detached and retrying once if the bus name
> is unowned — but every method it calls (`Screenshot`, `StartRecording`,
> `StopRecording`, `PickColor`, `OpenWindow`) is still a stub that logs and
> returns a clean D-Bus error naming the stage that implements it; so is
> `--no-daemon` and the `window` process. PLAN.md is the staged build plan.
> Sections marked *(pending Stage N)* fill in as later stages land.
>
> **2026-08-08 amendment (decided with Jordan)**: config migrated KDL → TOML
> (`capture.toml`) in Stage 4, which is why PLAN.md's original Stages 4–18
> are numbered 5–19. Documents written before the amendment — CAPTURE-RESEARCH
> §8's stage pointers, handoffs 1–3 — use the old numbering: add 1 to any
> stage reference ≥ 4. This file's references are current.
>
> Stage 10's clang prerequisite is **cleared** — Jordan installed clang 22.1.8
> (2026-08-08, verified live), so `pipewire` 0.10's bindgen build is unblocked
> (see CAPTURE-RESEARCH §2.0/D5).

## Commands

```sh
cargo build
cargo clippy --all-targets -- -D warnings   # warnings are errors, keep green
cargo test
cargo fmt --check

cargo run -- daemon                # the long-running daemon (surfaceless as of Stage 3)
cargo run -- shot --fullscreen     # what the Print keybind invokes
cargo run -- shot --region --geometry 600x450+100+100  # scriptable, skips the overlay
cargo run -- shot --fullscreen --no-daemon --format=webp --output=/tmp  # headless/scriptable
cargo run -- record start|stop|toggle [--preset hevc|av1|h264] [--audio mic|system|both]
cargo run -- pick-color
cargo run -- open
cargo run -- window [edit <path>]  # the app window / editor process
cargo run -- --config-dir ~/scratch shot --fullscreen  # capture.toml from an alternate dir
```

Every CLI verb above except `--no-daemon` shots is a real D-Bus client of the
daemon as of Stage 3 (auto-spawning it detached, retrying once, if the bus
name is unowned) — `busctl --user introspect io.saola.Capture1
/io/saola/Capture1` shows the live interface once a daemon is running. Every
served method still answers with a stub `Error` until its stage lands
(`src/dbus.rs` names which).

Live-testing anything that maps overlay surfaces or grabs the keyboard
happens in a **nested niri** (see Conventions), never the real session.
Booting the daemon itself is safe in the real session — Stage 3's daemon
is surfaceless (`StartMode::Background`, no shell role, no keyboard grab)
and will stay that way until Stage 6/7 spawn the first on-demand surface.

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
    (not `width * 4`) and ignore `maxsize`/`chunk->size`.
  - **Window screenshots go through niri-ipc `ScreenshotWindow`**, not a
    geometry crop: niri exposes no pixel position for tiled windows, so the
    crop rectangle is not computable. It also clobbers the clipboard
    unconditionally, so `storage.rs` owns the final clipboard state.
  - **ffmpeg needs `-use_wallclock_as_timestamps 1 -fps_mode vfr`** on the
    rawvideo input (casts are variable-rate; without it the video plays
    fast), GPU colour conversion with the matrix pinned
    (`scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=tv`), and an
    explicit crop to even dimensions (`hevc_vaapi` silently resizes odd
    inputs).
  - **`iced_layershell` is confirmed viable for the overlay** (Exclusive
    keyboard, Escape, pixel-exact drag, frozen-frame background — all
    live-tested in nested niri). Multi-output is source-verified only.

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
  Stage 1 landed the WebP encoder, clipboard, and CLI parser surveys; Stage 4
  landed the TOML crate survey (essays live in `Cargo.toml`; outcomes below).
  *(Survey pending Stage 10: pipewire —
  Stage 1 recorded version/SPA notes only, per PLAN.md, without adding the
  dependency yet. Stage 2 added the decisive build fact: `pipewire-sys` runs
  bindgen, so it needs **libclang at build time** — Jordan installed clang
  22.1.8 (2026-08-08), so the local build is unblocked; it remains a
  `makedepends`/CI build-dep entry in Stage 17.)*
  Stage 8 will also need `wayland-protocols`' **`staging`** feature if it ever
  touches `ext_foreign_toplevel_list_v1` — but per CAPTURE-RESEARCH §5.2 it
  should not: `niri msg windows` returns a superset in one call.
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
  save-dir = "~/Pictures/Screenshots"  # default: unset (storage.rs, Stage 5, falls back to ~/Pictures/Captures)
  image-format = "webp"                # "webp" | "png", default "webp"
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
