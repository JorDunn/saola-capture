# Stage 9 handoff — main app window (window process)

Forward-facing context for **Stage 10** (ScreenCast session + PipeWire
frames — parallel-safe with 9, per PLAN.md's `depends_on: [6]`, but reads
this anyway since it's the next stage subagent in sequence) and for whoever
eventually builds Stage 12 (recording UX, tray, the Record tab's real
hide-until-`RecordingFinished` wait this stage deliberately did not build).

Touched: `src/main.rs` (module doc comment, `run_window`, `run_open`),
`src/modules/mod.rs`, `src/dbus.rs` (`open_window`, new
`spawn_window_process`). New file: `src/modules/app.rs` (~970 lines incl. 9
tests). Nothing committed (per instructions — the working tree is left for
review).

---

## Verification

`cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt
--check` all clean. `cargo test`: **216 passed**, up from Stage 8's 207 (+9,
all in `modules::app`). One test
(`capture::screencopy::tests::read_with_retry_survives_a_file_that_appears_a_moment_later`)
flaked once during a full-suite run while a live-check `saola-capture
window` process was still rendering in the background — re-ran clean in
isolation and twice more as part of the full suite once that process was
closed. Pre-existing flakiness in a fixed-sleep-based timing test (the Stage
8 handoff already flagged this test's own tests as "prove the retry loop
works, not that 500ms is the right budget... under real timing" — this is
the same class of fragility, not a new one), not something Stage 9
introduced; not chased further since it isn't reproducible without an
unrelated CPU-contention source.

**Live-tested** (real session — sanctioned per this stage's own
instructions: "opening a plain toplevel iced window in the real session is
safe... Prefer that over nested-niri"): `./target/debug/saola-capture
window` opened against Jordan's real, already-running (pre-Stage-9-binary)
daemon. Confirmed via `niri msg windows` (floating, focused, 440×430 —
440 = `sizes.popover_width`, 430 = this stage's own `window_height`
formula, exact match) and a `grim` capture cropped down: header ("Saola
Capture" / "Close"), divider, Mode/Target/Format/Delay segmented rows all
rendered with correct ivory-rest/terracotta-selected styling. **No button
was pressed** — the window was closed by killing the process
(`pkill -f "target/debug/saola-capture window"`), not by clicking Close or
any capture control, so nothing in this check touched the real clipboard,
filesystem, or triggered a real screenshot/region-overlay/recording. Window
confirmed gone from `niri msg windows` afterward; no stray process left
(`ps aux` checked). **Not live-tested**: the Capture button's hide→D-Bus→
re-show round trip, the Record tab, and the editor stub (`window edit
<path>`) — none of these were exercised against a real compositor this
stage; see "What's unverified" below.

---

## The window↔daemon protocol, as used

Nothing new on the wire. `modules::app` is a **plain `zbus` client** of the
same `io.saola.Capture1` interface every CLI verb already uses —
`Capture1Proxy::screenshot`/`start_recording`, both existing methods,
called with the same `a{sv}` options shape `cli::CaptureOptions::
to_dbus_options`/`cli::RecordOptions::to_dbus_options` already produce (no
new D-Bus surface). The one interface change is that **`OpenWindow` is now
real**: `dbus.rs::CaptureService::open_window` spawns `saola-capture window
[edit <path>]` detached (`spawn_window_process`, mirroring
`spawn_daemon_detached`'s shape) instead of returning the Stage-3-era stub
error. `main.rs::run_open` (the `open` CLI verb) now calls
`cli::WindowAction::dbus_mode(None)` instead of a hardcoded `"main"`
literal — same string, but now sourced from the one function that owns that
encoding, which also gave `WindowAction::dbus_mode` a real production
caller again (it had none between Stage 8 and this edit, since the only
prior caller was `run_window`'s now-replaced stub print — **worth knowing
if you ever see a "never used" warning on it again**: it needs at least one
non-test call site or clippy's dead_code lint fires on the plain `cargo
build`/`cargo clippy` (non-test) target, which does **not** get to see the
`#[cfg(test)]` module).

## Hide/reopen behavior, as built

Two different waits, because the two D-Bus methods behind them complete
differently — **this is the one design decision most worth understanding
before touching this module**:

- **Screenshot**: `Screenshot` blocks until the whole capture (including an
  interactive region drag, if chosen) is saved, and already emits
  `CaptureTaken` before returning. So `App::start_capture` hides the window
  (`window::set_mode(id, Mode::Hidden)`), awaits the method call directly
  (`Task::perform(request_screenshot(..), Message::ScreenshotFinished)`),
  and re-shows on the reply (`App::finish` →
  `window::set_mode(id, Mode::Windowed)`). No signal subscription needed —
  the method's own return is provably later than the signal.
- **Record**: `StartRecording` does **not** have that property once it's
  real (Stage 10/11) — it will return as soon as recording has *begun*, not
  when it *finishes*; only `RecordingFinished` (fired whenever the
  recording is later stopped, from anywhere) marks the real end. **This
  stage does not build that wait.** PLAN.md Stage 12 explicitly owns it
  ("the app window's Record tab now drives real recordings and hides while
  recording"), and there's a real technical reason it wasn't attempted
  early: `zbus::Connection` isn't `Hash`, which is what
  `iced::Subscription::run_with` needs to key a per-connection signal
  stream — Stage 12 needs its own answer to that regardless of what Stage 9
  did. What Stage 9 *does* build for Record: the same
  hide→await-the-reply→re-show shape as Screenshot, applied to
  `StartRecording`'s current stub `Error` reply. Today that means pressing
  "Start Recording" hides the window for a moment and re-shows it with the
  stub's own message inline — correct, honest stub behavior (CLAUDE.md's
  no-panic/no-silent-stub rule), not a placeholder that silently does
  nothing.

**Reopening a window that isn't hidden-alive, but flat-out not running** is
the `open` verb / toast click / tray menu path PLAN.md's task 2 names. All
three now converge on the same mechanism — spawn a fresh `saola-capture
window [edit <path>]` process:
- `open` CLI verb → `OpenWindow("main")` → `dbus.rs::spawn_window_process`.
- Toast click → `main.rs::spawn_editor` (unchanged since Stage 6 — it
  already spawned `window edit <path>` directly, without going through
  `OpenWindow` at all; left as-is, not routed through D-Bus, since it's
  already correct and simpler as an in-process `Command::spawn`).
- Tray menu (Stage 12, not built yet) gets `OpenWindow` for free once it
  exists — same call `open` makes.

**Known v0.1 gap, recorded on purpose, not a silent oversight**: nothing
tracks "is a window process already alive-but-hidden somewhere." A second
`open` while one capture is mid-flight (window hidden, awaiting a reply)
spawns a **second** window process rather than raising the first. Closing
this means the *daemon* tracking window-process liveness (a PID, a
"already open" flag) — genuinely more scope than this stage's task list
("D-Bus-call the daemon and hide the window... reopening via open
verb/toast click/tray menu") asks for. If this becomes annoying in
practice, the shape to build is: daemon tracks the window process's PID
(set via a new D-Bus call the window process makes on boot, cleared on
exit/signal), and `OpenWindow` checks it before spawning.

## The editor stub, exact shape (for Stage 14)

`window edit <path>` skips the main view entirely — `App::boot` builds
`ViewState::Editor { path, image }` directly from `WindowMode::Edit`, no
navigation between the two. `image: Result<iced::widget::image::Handle,
String>` is decoded **synchronously**, at boot, via `::image::open(path)`
(external `image` crate — see the `::` disambiguation note below) →
`.into_rgba8()` → `iced::widget::image::Handle::from_rgba`. Deliberately
*not* the lockscreen wallpaper's async-decode-before-iced pattern: that
precedent exists because a *slow* decode risks a blank first frame on a
surface whose whole point is looking right immediately; a screenshot-sized
file decodes in low single-digit milliseconds, so the synchronous path
costs nothing perceptible and keeps `App::boot`'s signature simple (no
extra `Task` beyond the D-Bus connect). A decode failure is caught and
rendered as inline text ("Could not open this image: ...") rather than
propagated — no-panic rule, and a missing/corrupt file is a reason to show
the user something, not to crash a window they could still close.

`editor_view` (the whole surface Stage 14 replaces): header (title = `Saola
Capture — <filename>`, Close button, same drag-by-header as the main view)
→ the image at `ContentFit::Contain` filling the remaining space → a
mono-font path caption → one line, "Editing tools land in Stage 14 — this
is a preview only." **No canvas, no tool palette, no save path, no undo
stack** — none of Stage 14's actual job (crop/arrow/rect/ellipse/freehand,
per PLAN.md) was attempted or stubbed beyond this note. If Stage 14 wants a
different entry shape (e.g. the canvas needs the *original* `Frame`
bytes/RGBA buffer rather than an already-built `iced::widget::image::
Handle`), `load_image`/`ViewState::Editor` is the one place to change — the
rest of the window (header, D-Bus connect, hide/show machinery) is
independent of what the editor's own content looks like.

---

## New / changed interfaces, exact signatures

### `src/modules/app.rs` (new)

```rust
pub enum WindowMode { Main, Edit(PathBuf) }
pub fn window_mode_from_action(action: Option<&cli::WindowAction>) -> WindowMode;
pub fn run(mode: WindowMode) -> iced::Result;   // builds + runs the iced::application, blocks
```

Everything else in the module (`App`, `Message`, `CaptureMode`,
`AudioChoice`, `ViewState`, `segmented_row`, `header`, `capture_button`,
`resolve_capture_options`, `record_options`, `load_image`, ...) is private
— the module's only public surface is the three items above, which is
exactly what `main.rs::run_window` needs.

`resolve_capture_options` is the one piece of real logic worth knowing
about if you touch `cli::ShotArgs`: it builds a **synthetic**
`cli::ShotArgs` from the UI's target/delay/cursor/format state and calls
the existing `cli::CaptureOptions::resolve` on it, rather than
hand-rolling a second flags-over-config fold. It starts from
`ShotArgs::default()` and only sets the fields the UI actually drives, so a
new `ShotArgs` field doesn't need a matching edit here to keep compiling —
but it also means a *new* flag some future stage adds to `ShotArgs` won't
be reachable from the app window's UI until someone deliberately wires a
control for it.

### `src/dbus.rs`

```rust
async fn open_window(&self, mode: String) -> zbus::fdo::Result<()>;   // real, not a stub, as of Stage 9
fn spawn_window_process(mode: &str) -> Result<(), std::io::Error>;    // NEW, private
```

`spawn_window_process` is `spawn_window_process("edit:/path")` →
`saola-capture window edit /path`; anything not starting with `"edit:"`
(in practice always exactly `"main"`) → bare `saola-capture window`. It's
the wire-format inverse of `cli::WindowAction::dbus_mode` — the two sides
don't share a parser, only an agreed string shape, same as every other
`a{sv}` value in this crate.

### `src/main.rs`

```rust
fn run_window(action: Option<&cli::WindowAction>) -> ExitCode;   // now calls modules::app::run, real
fn run_open() -> Result<String, CliRunError>;   // now sends WindowAction::dbus_mode(None), not "main"
```

`spawn_editor` (the toast-click handler) is **unchanged**.

---

## Design decisions and gotchas worth recording

- **`Message` needed `#[derive(Clone)]`**, unlike the daemon's `Message`
  (which needs it for a different reason — `#[to_layer_message(multi)]`).
  Here it's because plain `iced::widget::button`/`mouse_area` require their
  message type to be `Clone` when built via the `row!`/`container` helper
  macros in this crate's resolved iced version — found by just trying to
  build and reading the compiler's own suggestion, not by reasoning it out
  in advance. Every field in `Message` (a `zbus::Connection`, which
  *is* `Clone`+`Debug` — confirmed by reading `zbus-5.18.0`'s own source
  rather than assuming) already supported it, so this cost nothing.
- **`iced::widget::image` (the widget module/fn) shadows the external
  `image` crate's name** the moment you `use iced::widget::image;` for
  `image::Handle` — both a module and a crate share the name `image`, and
  the local `use` wins for unqualified path lookups in that file. Calling
  the external crate's `open`/`DynamicImage` needs a **leading `::`**
  (`::image::open(path)`) to force crate-root resolution. No existing file
  in this crate needed both at once before (`toast.rs` only ever needed the
  iced widget half; `storage.rs` only ever needed the external crate half),
  so this is a new gotcha worth adding to CLAUDE.md's iced-0.14 list if a
  future stage hits it again in a different file.
- **`iced::Subscription::run_with<D: Hash>` cannot take a
  `zbus::Connection` as its keying data** (`Connection` isn't `Hash`) —
  this is *why* the Record tab's real "wait for `RecordingFinished`"
  behavior isn't built here; see the hide/reopen section above. Whoever
  builds Stage 12 will need either a different subscription shape (a
  connection-independent key, like the recording's own id/handle) or a
  hand-rolled `iced::stream::channel` worker the way `main.rs::
  dbus_worker_stream` already does for the daemon side — that's the
  precedent to copy, not `run_with`.
- **`window::open_events()` is enough to learn this process's one window's
  `Id`** — no need for `window::latest()`/`window::oldest()` round trips
  the way a multi-window app might use. A plain `iced::application` opens
  exactly one window at startup, so the first (and only) event this
  subscription ever delivers is the one that matters.
- **`.decorations(false)` + `.transparent(true)` + an explicit `.style()`
  returning a transparent clear color, all three together** — mirrors
  CLAUDE.md's Stage 6 finding for `iced_layershell` surfaces (documented
  there as binding "on every future surface"), applied here defensively to
  a *plain* `iced::application` even though that finding was specifically
  about the layer-shell renderer path. The live check above shows correctly
  rounded corners with the wallpaper/desktop showing through outside them,
  which is consistent with the defensive trio working — but this was not
  isolated (i.e., no A/B check with the `.style()` override removed to
  confirm it's actually load-bearing here the way it provably was for the
  flash surface). Treat "this generalizes to plain applications" as
  probable-but-unconfirmed, the same epistemic status the Stage 8 handoff
  gave the countdown pill's surface-latency reasoning.
- **A window's default height has no style-guide token** — `Sizes` covers
  popover/launcher/notification-card *widths*, never a settings-style
  window's height. `window_height()` derives one from `sizes.window_header
  + 8 × sizes.list_row + 4 × sizes.popover_padding` (documented at its one
  definition site, same posture as `modules::toast::card_height`'s own
  undocumented-token derivation) — live-verified to compute **exactly**
  430px, matching the real window's reported `430` in `niri msg windows`
  down to the pixel, which at least confirms the arithmetic is right, if
  not that 430px is the *correct* height for every possible content state
  (the Record tab's Audio row makes that tab one section taller than
  Screenshot's; both fit inside the fixed height today because of the
  `scrollable` wrapper, not because the estimate accounts for both tabs
  distinctly).

## What's verified vs. not

**Verified** (headless, the hard contract): `cargo build`/`clippy --all-targets -D
warnings`/`fmt --check`/`test` all clean, 216 tests including 9 new pure
unit tests (`resolve_capture_options`'s target/cursor/delay/format
resolution, `record_options`, `AudioChoice` round-trip,
`window_mode_from_action`, `load_image`'s clean-error path on a missing
file, `window_height`'s sanity bound).

**Verified live** (best-effort, real session, read-only): the main view
renders correctly — chrome, header, all four segmented-control rows, exact
window size math — with **no synthetic input, no button press, no daemon
call attempted**.

**Not verified at all, this stage**:
- The Capture button's actual hide→D-Bus→re-show round trip (pressing it
  would have taken a real screenshot via Jordan's already-running,
  pre-Stage-9 daemon binary — a legitimate, safe side effect in principle,
  same as Stage 8's own sanctioned `shot --window --no-daemon` check, but
  not attempted here to keep this stage's live-testing footprint to the
  minimum the instructions asked for: "close it before finishing", not
  "exercise every control").
- The Record tab's UI and its D-Bus round trip.
- The editor stub (`window edit <path>`) — no live capture file was passed
  to it. `load_image`'s decode path is only exercised by the unit test
  against a *missing* file, never a real image.
- Window dragging via the header (`Message::DragWindow` → `window::drag`) —
  plausible from reading the code and the established `mouse_area`-skips-
  when-a-child-button-captures precedent (CLAUDE.md, Stage 7), not
  independently confirmed by dragging the actual window.
- Close-button click (the process was killed instead, specifically to avoid
  any synthetic input — see the incident note this repo's CLAUDE.md carries
  from Stage 8).
- Multiple concurrent window processes (the "known v0.1 gap" above) — not
  reproduced, only reasoned through.

---

## For Jordan

Nothing needs your action before the next stage starts. Three things worth
knowing:

1. **`saola-capture window` now actually opens a window** — try it
   yourself (`cargo build && ./target/debug/saola-capture window`) whenever
   convenient. It renders against your currently-running daemon (PID
   187728 as of this stage, predating this stage's binary — same situation
   Stage 8's handoff already flagged). Pressing Capture with it will take a
   **real** screenshot (flash/toast/save/clipboard, exactly like `Print`)
   since `Screenshot` has been real since Stage 5 — nothing new or risky
   there, but it *is* a real capture, not a preview.
2. **`saola-capture open` now really opens the window** (previously logged
   a stub error) — worth trying too, and it's the same code path a future
   tray "Open Saola Capture" menu item will use.
3. **The Record tab's button will currently show the `StartRecording`
   stub's own error message** ("StartRecording is not implemented yet
   (Stage 10/11)") after hiding and re-showing the window — expected, not a
   bug; Stage 10/11 make it real.
