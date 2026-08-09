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

> Status: Stages 1–6 landed (repo skeleton, dependency survey; every capture
> path proven with live evidence in `docs/CAPTURE-RESEARCH.md`; full CLI
> parsing, `capture.toml` config, the `io.saola.Capture1` bus, a surfaceless
> daemon boot; Stage 5's **real screenshot pipeline**; and — new in Stage 6 —
> the **PrintScr MVP**: the daemon now maps real layer-shell surfaces).
> `shot --fullscreen` and `shot --region --geometry WxH+X+Y` now genuinely
> capture, encode, save, copy and print a path, **both ways**: through the
> daemon's `Screenshot` D-Bus method (which also emits `CaptureTaken`) and
> in-process via `--no-daemon`. Both go through the same two library calls,
> `capture::take_screenshot` → `storage::save_capture`. As of Stage 6, the
> daemon path additionally **flashes and toasts**: `src/modules/flash.rs` (a
> full-output ivory fade, live-verified in nested niri) and
> `src/modules/toast.rs` (the §6 notification card, stack of 3, click opens
> the — still-stub — editor). Both are wired end to end and are this
> repo's first surfaces to actually map pixels; see Conventions for two
> binding gotchas Stage 6 found the hard way.
> Still stubs, each answering with a clean error naming its stage:
> `StartRecording`/`StopRecording` (Stages 10–11), `PickColor` (Stage 16),
> `OpenWindow` and the `window` process (Stage 9), an interactive `--region`
> with no `--geometry` (Stage 7's overlay) and `--window` (Stage 8).
> PLAN.md is the staged build plan. Sections marked *(pending Stage N)* fill
> in as later stages land.
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

There is also a **hidden** verb, `saola-capture clipboard-serve --mime
image/png`, which reads bytes on stdin and serves them as the Wayland
selection until something else claims it. It is an implementation detail of
`--no-daemon` captures (a Wayland "copy" needs a live process to answer paste
requests, and a CLI verb exits immediately) — spawned detached by
`storage.rs`, never typed by hand, hidden from `--help`.

Every CLI verb above except `--no-daemon` shots is a real D-Bus client of the
daemon as of Stage 3 (auto-spawning it detached, retrying once, if the bus
name is unowned) — `busctl --user introspect io.saola.Capture1
/io/saola/Capture1` shows the live interface once a daemon is running. As of
Stage 5 `Screenshot` is real; the other served methods still answer with a
stub `Error` until their stage lands (`src/dbus.rs` names which).

Live-testing anything that maps overlay surfaces or grabs the keyboard
happens in a **nested niri** (see Conventions), never the real session.
Booting the daemon itself is safe in the real session — as of Stage 6 the
daemon does map real surfaces (the flash, permanently; the toast, while
captures are recent), but neither grabs the keyboard
(`KeyboardInteractivity::None` on both — see `main.rs`'s `flash_surface_settings`/
`toast_surface_settings`), so this is still the one surface-mapping daemon
behavior safe to boot in the real session. Stage 7's overlay is the first
surface that *will* need `Exclusive` keyboard, and that's exactly the one
that must never be live-tested outside nested niri.

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
  channel degrades to a logged warning, never a blocked D-Bus reply. Stage
  10's PipeWire-thread-to-daemon bridge is the next place this pattern
  almost certainly gets reused (or `tokio::sync`, if the pipewire thread
  isn't itself async — decide there, this note is just "the precedent
  exists").
- **Mechanical iced 0.14.2 gotchas, found in Stage 6** (cheap to relearn
  the hard way, cheaper to just know): `iced::widget::Space::new()` takes
  **zero** arguments in this crate's resolved version — size it with
  `.width(..)`/`.height(..)` builder calls, not `Space::new(w, h)` or a
  `Space::with_width(w)` associated function (neither exists here, despite
  looking plausible from memory of other iced versions).
  `iced::widget::image::Handle` derives `Clone`/`PartialEq`/`Eq` but **not**
  `Debug` (checked directly in `iced_core-0.14.0/src/image.rs`) — wrap it in
  a local newtype with a hand-written `Debug` before putting it in any type
  that needs to derive `Debug` (`main.rs`'s `Thumbnail` is the example; the
  same problem will recur the moment a `Frame`/pixel buffer needs to ride in
  a `Message`).
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
  `magick -format "%[pixel:p{X,Y}]" info: file.png` (ImageMagick, already
  installed) samples one pixel's color from a `grim` capture without opening
  it, cheap enough to script into a tight loop for a "did this render in
  time" check the way a single screenshot at an arbitrary offset can't
  answer reliably.
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
