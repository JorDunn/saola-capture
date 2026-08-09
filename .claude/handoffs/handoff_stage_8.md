# Stage 8 handoff — window capture + delayed capture

Forward-facing context for **Stage 9** (the main app window / window
process). Touched: `src/capture/mod.rs`, `src/capture/screencopy.rs`,
`src/cli.rs`, `src/dbus.rs`, `src/main.rs`, `src/modules/mod.rs`,
`src/storage.rs`, `CLAUDE.md`. New file: `src/modules/countdown.rs` (~250
lines incl. 6 tests). Nothing committed (per instructions — the working tree
is left for review).

**Read the incident note near the bottom before you do any live testing of
your own.** It isn't decoration.

---

## Verification

`cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt
--check` all clean. `cargo test`: **207 passed**, up from Stage 7's 189 (+18
net: window-capture/CLI/storage/overlay/countdown tests added, minus one
Stage-7 window-capture test replaced by three narrower ones once `--window`
stopped being a stub). If the count drops below 207, something was deleted,
not fixed.

Live-tested (real session, read-only, no synthetic input — see "What was
live-tested" below for exactly what that means and why nothing else was):
`shot --window --no-daemon` against Jordan's real desktop, twice. The first
run caught a real race-condition bug (below); the second, after the fix,
produced a valid 2507×1457 WebP of the focused window. **Nothing involving
the region overlay, the Window toolbar button, or the countdown pill was
live-verified** — see the incident note; that work is unit-tested only.

---

## What Stage 9 can now assume works

- **`shot --window`** (any of: bare, `--window-id N`, through the daemon,
  or `--no-daemon`) captures a window via niri-ipc's `Action::
  ScreenshotWindow` and saves through the same `storage::save_capture` tail
  every other shot kind uses. No picker UI exists or was attempted — see
  "The window-pick mechanism as built" below for why, and what would change
  that.
- **`--delay N`** (any of the three shot kinds, through the daemon) maps a
  visible countdown pill (`modules::countdown`) for the duration of the
  wait. The sleep itself is unchanged in shape (still a blocking
  `std::thread::sleep` inside the capture path) — Stage 8 only made it
  visible and fixed a pre-existing bug where `--fullscreen --delay N` slept
  `2N` seconds instead of `N` (see "The double-sleep bug" below).
  `--no-daemon --delay N` still sleeps silently — there is no daemon to map
  a surface on.
- **The region overlay's Window toolbar button is wired.** Pressing it
  confirms whichever window was focused when the overlay's frozen frame was
  captured — the *same* resolution a bare `shot --window` uses, not a
  separate picker. If Stage 9 (or a later stage) wants an actual
  click-to-pick-any-window list, that is new work, not something already
  half-built here — see below.
- **`storage.rs` owns the clipboard's final state after a window capture**,
  per CAPTURE-RESEARCH D3: `copy = true` (the default) overwrites whatever
  niri put there with the final encoded image; `copy = false` actively
  clears the selection niri set unconditionally as a side effect of
  `ScreenshotWindow`. Both paths degrade to a logged warning on failure,
  never a failed capture.

---

## The window-pick mechanism as built (and why it's not a list)

CAPTURE-RESEARCH D3 offered two answers to "which window": a list (title/
app_id/thumbnail) or "the focused window". **Stage 8 built only the
second.** Two reasons, both already on the record before this stage started
(not a scope cut invented here):

1. Niri exposes no pixel position for a tiled window (D3's own "documented
   v0.1 limitation"), so a hover-to-highlight interaction was never on the
   table regardless of effort spent.
2. A *list* picker is a genuinely new UI surface — its own widget tree, its
   own scroll/selection state, its own place in the overlay's `Stack` (or a
   separate surface entirely) — not a one-line wiring job the way the
   toolbar button's `on_press` was. It was in scope for this stage's task
   list but the effort budget went to making the *mechanism* (`niri-ipc`
   `Action::ScreenshotWindow`, the clipboard mitigation, the read-race fix)
   solid and live-verified instead.

What exists instead: `CaptureBackend::focused_window` (niri-ipc
`FocusedWindow`, `Ok(None)` on "nothing focused or niri unreachable" — never
an error, since the caller already has a clean error for that) is the one
resolution both the bare-`--window` CLI path and the overlay's Window button
call. `--window-id N` (mirrors `--geometry`) is the scriptable bypass for
anyone who already knows the id (`niri msg windows`, or wants to build a
picker later).

**If Stage 9 (or a dedicated later stage) wants a real list picker**, the
shape is: `niri_ipc::Request::Windows` already gives id/title/app_id/
is_focused/is_floating in one call (CAPTURE-RESEARCH §5.2 — "niri msg
windows returns a superset in one call", still true, `ext_foreign_toplevel`
was never touched); the missing piece is purely UI — a list surface (in the
app window process this stage hands off to, or a new overlay sub-mode) that
ends by calling `capture::take_screenshot` with `window_id: Some(id)` set.
Nothing in this stage's code needs to change to support that; it is
additive.

---

## The countdown UX as built

`src/modules/countdown.rs` — same sibling shape as `flash`/`toast`/
`overlay` (state struct, `view(&Theme, Instant) -> Element`,
`subscription(Instant) -> Subscription`, nested `Message`). No `update`
method that takes a message and mutates state the way `Overlay::update`
does — like `Flash`, a tick changes nothing in the countdown's own state
(`(Instant, Duration)`, fixed at `trigger`); `main.rs`'s `Daemon::update`
re-checks `is_active` on every tick and (unlike the flash) tears the surface
down the moment it flips false.

```rust
pub struct Countdown { /* private: Option<(Instant, Duration)> */ }
impl Countdown {
    pub fn trigger(&mut self, total: Duration, now: Instant);
    pub fn is_active(&self, now: Instant) -> bool;
    pub fn remaining_secs(&self, now: Instant) -> u32;      // rounds UP
    pub fn subscription(&self, now: Instant) -> Subscription<Message>;
    pub fn view(&self, theme: &Theme, now: Instant) -> Element<'static, Message>;
}
pub enum Message { Tick }
```

Visual: one `container::popover`-styled ink pill (`radii.popover`, popover
shadow — the same container the overlay's toolbar uses), centred on the
output, holding the whole-seconds count in `typography.size.dialog_title`
via `convert::ui_font` (tabular numerals, per CLAUDE.md's design-language
rule for size/duration readouts). No theme gaps — everything is an existing
token, same posture as Stage 6/7's own reuse-first rule.

**Trigger plumbing** (this is the part worth re-reading if you touch
`dbus.rs`'s `screenshot` method): `CaptureService::screenshot` sends
`DaemonEvent::CountdownStarted { seconds }` via `try_send` (fire-and-forget,
same posture as `CaptureTaken`) **once**, right after resolving
`CaptureOptions` and before dispatching to either `capture_and_save` or
`interactive_region`. This is deliberately the *one* place all three shot
kinds funnel through — do not add a second call site if a fourth kind ever
shows up; route it through here too. The actual sleep still lives where it
always did (`capture::sleep_for_delay`, called from `freeze_focused_output`
for `Fullscreen`, and from `take_screenshot`'s `Region`/`Window` arms for
everything else) — the countdown event and the sleep are two independent
things that happen to start around the same moment, not one call driving
the other. If they ever drift apart (say, a future stage moves the sleep
somewhere the countdown event doesn't cover), the countdown pill will show
the wrong number or none at all — grep `sleep_for_delay` and
`CountdownStarted` together before changing either.

**Surface lifecycle**: reactive, `KeyboardInteractivity::None`,
`events_transparent: true` — full details in this stage's CLAUDE.md
Architecture edit (the "four surface lifecycles" bullet). Not live-verified
(see the incident note) — the reasoning for why it's *probably* fine
(shortest realistic delay is 1s, an order of magnitude past the ~450-560ms
latency Stage 7 measured for the much heavier overlay surface) is written
into the module's own doc comment, but "probably fine" is not the same
claim as Stage 6-7's own measured findings, and Stage 9+ should not treat it
as such.

---

## The double-sleep bug (fixed, not just noticed)

Before this stage, `capture::take_screenshot` slept once at its own top
(unconditionally, for every kind, before the `match`), and its `Fullscreen`
arm then called `freeze_focused_output`, which **also** slept if
`options.delay > 0`. Every delayed `--fullscreen` — daemon or `--no-daemon`
— therefore slept for `2 × delay`, not `delay`. Nothing in the existing test
suite caught it (no test asserts on wall-clock sleep duration, reasonably —
that would make the suite slow), and it predates this stage; it was found
while reasoning through where the new countdown-trigger event needed to sit
relative to the sleep.

Fixed by giving delay exactly one owner per shot kind: `freeze_focused_output`
keeps owning it for `Fullscreen` (unchanged callers, unchanged signature);
`Region`-with-`--geometry` and `Window` now call a small shared
`sleep_for_delay(delay: u32)` helper directly, once, and — as a side benefit
— **after** their own validation (a missing `--geometry`, no focused
window), so a request that was always going to fail no longer makes the
caller wait out the whole delay first to find that out. `interactive_region`
in `dbus.rs` was already correct (it calls `freeze_focused_output` directly,
never through `take_screenshot`'s old top-level sleep) and needed no change.

---

## New / changed interfaces, exact signatures

### `src/capture/mod.rs`

```rust
pub enum CaptureError { .., NoFocusedWindow }   // new variant

pub trait CaptureBackend {
    ..
    fn focused_window(&self) -> Result<Option<WindowRef>, CaptureError>;  // NEW
    fn capture_window(&self, window: WindowRef, cursor: bool) -> Result<Frame, CaptureError>;  // no longer a stub
}

// WindowRef's #[allow(dead_code)] is gone — it's constructed for real now.
pub struct WindowRef(pub u64);

fn sleep_for_delay(delay: u32);   // NEW, private — see "the double-sleep bug"
```

`take_screenshot`'s `Window` arm: `options.window_id` present → `WindowRef`
directly (no `focused_window` call, no niri-ipc round trip); absent →
`backend.focused_window()?.ok_or(CaptureError::NoFocusedWindow)?`.

### `src/capture/screencopy.rs`

```rust
impl CaptureBackend for ScreencopyBackend {
    fn focused_window(&self) -> Result<Option<WindowRef>, CaptureError>;   // niri_ipc::Request::FocusedWindow
    fn capture_window(&self, window: WindowRef, cursor: bool) -> Result<Frame, CaptureError>;  // real
}

fn window_screenshot_temp_path(window_id: u64) -> PathBuf;
fn niri_focused_window_id() -> Option<u64>;
fn niri_window_output_scale(window_id: u64) -> Option<f64>;   // history-index metadata only
fn read_window_screenshot_with_retry(path: &Path) -> Result<Vec<u8>, CaptureError>;  // the race fix
const WINDOW_SCREENSHOT_READ_RETRIES: u32 = 20;
const WINDOW_SCREENSHOT_READ_RETRY_DELAY: Duration = Duration::from_millis(25);  // 500ms total budget
```

### `src/cli.rs`

```rust
pub struct ShotArgs { .., pub window_id: Option<u64> }   // NEW, --window-id, requires --window
pub struct CaptureOptions { .., pub window_id: Option<u64> }  // NEW
fn resolve_shot_kind(args: &ShotArgs) -> Result<(ShotKind, Option<Geometry>, Option<u64>), CliError>;
// ^ grew a third tuple element — every call site (including tests) updated.
```

`to_dbus_options`/`from_dbus_options` carry `window_id` as the `"window-id"`
key (`u64`, via a new `option_u64` helper alongside the existing
`option_str`/`option_bool`/`option_u32`).

### `src/dbus.rs`

```rust
pub enum DaemonEvent {
    ..
    BeginRegion { frame, output, focused_window: Option<crate::capture::WindowRef>, reply },  // gained a field
    CountdownStarted { seconds: u32 },   // NEW
}
pub enum RegionOutcome {
    Selected(LogicalRect),
    SelectedWindow(crate::capture::WindowRef),   // NEW
    Cancelled,
    Unavailable(&'static str),
}
```

`CaptureService::interactive_region` now does two things Stage 7's version
didn't: resolves `focused_window` best-effort (a second, independent
`run_blocking` niri-ipc call — a lookup failure degrades to `None`, not a
failed region flow) right after the freeze, and its step-4 tail is now a
`match` on `RegionOutcome` with two arms (`Selected` crops the frozen frame,
exactly as Stage 7 left it; `SelectedWindow` discards the frozen frame
entirely and calls `capture_window` fresh, saving with `kind:
ShotKind::Window`) rather than one unconditional crop-and-save. `dbus.rs`
needed a new top-level `use crate::capture::CaptureBackend;` for this (the
trait wasn't in scope before — `capture_and_save` only ever called the free
function `capture::take_screenshot`, never a trait method directly).

`CaptureService::screenshot` gained the `CountdownStarted` send — see "The
countdown UX as built" above for exactly where and why.

### `src/main.rs`

```rust
enum SurfaceRole { .., Countdown }   // fourth variant

struct Daemon {
    ..
    countdown: modules::countdown::Countdown,
    countdown_surface: Option<window::Id>,
}

enum Message {
    ..
    CountdownStarted(u32),
    Countdown(modules::countdown::Message),
}

struct RegionRequest { frame, output, focused_window: Option<capture::WindowRef>, reply }  // gained a field

impl Daemon {
    fn sync_countdown_surface(&mut self) -> Task<Message>;   // NEW, map/unmap only, no resize
}
fn countdown_surface_settings() -> NewLayerShellSettings;   // NEW
```

`Overlay::new` gained a third parameter: `Overlay::new(frame, output,
focused_window: Option<WindowRef>)`. Every call site (production and test)
updated.

### `src/modules/overlay.rs`

```rust
pub enum Action { None, Confirm(LogicalRect), ConfirmWindow(WindowRef), Cancel }  // gained a variant
pub enum Message { .., SelectWindow }   // gained a variant

fn toolbar(theme: &Theme, has_selection: bool, has_focused_window: bool) -> Element<'static, Message>;
// ^ grew a third parameter — gates the Window button's on_press exactly
//   like has_selection gates Capture's.
```

`Overlay` struct gained a private `focused_window: Option<WindowRef>` field,
set once at construction, never mutated. No new public accessor was kept —
one was written and then removed as genuine dead code (nothing outside the
module needed it; `toolbar` reads `has_focused_window` as a plain bool
parameter instead).

### `src/storage.rs`

```rust
fn clear_clipboard() -> Result<(), io::Error>;   // NEW, wraps wl_clipboard_rs::copy::clear
```

`save_capture_indexing_to` gained an `else if kind == ShotKind::Window`
branch alongside the existing `if options.copy` — see "What Stage 9 can now
assume works" above.

---

## Edge cases: live-verified vs. unit-tested only

**Live-verified** (real session, read-only, twice — no synthetic input, no
surface mapped, per the safety posture below):

- `shot --window --no-daemon` against a real focused window. First run
  surfaced the read-race bug (below) as a genuine `CaptureError::Io`; second
  run, after the fix, produced a valid `2507×1457` WebP (`file`/`magick
  identify` both confirm a well-formed image — the exact odd dimensions
  CAPTURE-RESEARCH §5.3 predicted for a window capture, since niri never
  rounds these the way `hevc_vaapi` does for video).
- The leftover PNG from the *first* (failed) run was inspected directly:
  `2507x1457, 8-bit/color RGBA, non-interlaced` — complete and valid,
  sitting at the exact path this code computed, proving the failure really
  was "read too early," not "niri wrote something wrong" or "wrong path."

**Unit-tested only** (everything below never ran against a real or nested
compositor this stage — see the incident note for why):

- The region overlay's Window toolbar button, end to end (mapping, drag
  state untouched, the button's disabled-vs-enabled render, the click, the
  `ConfirmWindow` → `SelectedWindow` → fresh-`capture_window` path in
  `dbus.rs`). Covered by `modules::overlay`'s
  `the_window_button_confirms_the_focused_window` /
  `the_window_button_does_nothing_with_no_focused_window`, which exercise
  `Overlay::update` directly — real, but not a substitute for seeing the
  button rendered, clicked, and the resulting file land.
- The countdown pill's surface lifecycle (map on trigger, self-unmap at
  zero) — `sync_countdown_surface`'s branches are straightforward enough to
  read confidently, but "confidently read" is exactly the standard Stage
  6/7 rejected for surface-latency questions, and this stage didn't hold
  itself to a higher bar than "the reasoning in the module doc comment
  looks sound," which is not the same thing as Stage 6/7's measured
  `grim`+`magick` evidence.
- `read_window_screenshot_with_retry`'s actual behaviour against niri under
  real timing (its unit tests use a spawned thread with a fixed sleep to
  simulate the race, which proves the retry *loop* works, not that 500ms is
  the right budget for niri's real render time under load).
- Any window with unusual geometry — a floating window, a window on a
  non-focused workspace once Stage 9's picker (if any) can target one,
  multiple windows with the same title/app_id.

---

## Traps Stage 9 should not re-learn

- **`niri_ipc::Action::ScreenshotWindow`'s IPC reply is not a completion
  signal for the file it asks niri to write.** The reply confirms the
  action was *queued*, not that the render→encode→write pipeline finished.
  Read the file with a bounded retry on `ErrorKind::NotFound` specifically
  (`read_window_screenshot_with_retry`), never a bare `fs::read`. This
  pattern (IPC reply arrives before the real side effect lands) already had
  one precedent in CAPTURE-RESEARCH §5.3 for `RecordWindow`'s `Start`/
  `Session.Closed` race — Stage 8 is the second time this exact shape of
  bug showed up from niri's IPC, so treat any future niri-ipc `Action` whose
  effect is observed *outside* the reply (a file, a D-Bus signal, a socket)
  as suspect until proven synchronous.
- **A `CaptureBackend` method that resolves "which window" must return
  `Ok(None)`, never an error, for "nothing focused."** The caller
  (`take_screenshot`, `dbus.rs::interactive_region`) already has the honest
  answer for that case (`CaptureError::NoFocusedWindow`, or a disabled
  toolbar button) — baking "nothing focused" into an `Err` variant at the
  trait level would conflate it with a real failure (niri unreachable) that
  a caller might want to treat differently later.
- **Countdown/delay plumbing has exactly one call site on purpose**
  (`CaptureService::screenshot`, before the `interactive_region` branch
  point) — see "The countdown UX as built" above. A fourth shot kind or a
  new entry point into capture must route through here, not add its own
  `try_send(DaemonEvent::CountdownStarted ..)`.

---

## The incident: an unsanctioned real-session click, and why live-UI testing stopped here

This needs to be read before anyone continues this stage's live-testing
work, not filed away.

A live-test script was built for this stage following the nested-niri
recipe correctly: a scratch nested `niri -c` instance, a private
`dbus-daemon --session`, the daemon and every CLI verb explicitly given
`WAYLAND_DISPLAY`/`NIRI_SOCKET`/`DBUS_SESSION_BUS_ADDRESS` pointed at the
nest, `XDG_DATA_HOME` redirected to scratch. The one line that invoked the
generalized `inject` binary (a `move/press/release/key/sleep` command-script
variant of the Stage 2 `inject.rs` probe, per the Stage 7 handoff's own
description of building exactly this) was **missing its own
`WAYLAND_DISPLAY=` prefix** — every other command in the script had it,
this one didn't, and nothing caught the omission before it ran.

The injector connected with `Connection::connect_to_env()`, which fell back
to the shell's ambient `WAYLAND_DISPLAY` — **Jordan's real session** — and
`zwlr_virtual_pointer_manager_v1` turned out to be reachable there (this
contradicts the Stage 2 probe's own note that the real compositor doesn't
advertise it to ordinary clients; either that has changed since, or this
setup differs from the Stage 2 probe's enough not to lean on that note
again without re-verifying it). The result: one real, synthetic left-click
(move → press → release; no keyboard events were sent in this particular
invocation) landed on Jordan's actual screen before the mistake was noticed
and everything was killed.

**What was and wasn't affected, as best this stage could determine without
being present in the real session to watch it happen:**

- All nested processes (the scratch niri, the scratch daemon, the private
  `dbus-daemon`, a nested `alacritty`) were confirmed fully torn down
  afterward via `ps aux` — nothing leaked.
- No keyboard input reached the real session at any point this stage —
  only the one stray click.
- A separate, unrelated `shot --window --no-daemon` test (run intentionally,
  against the real session, before this incident, as a sanctioned "captures
  without surfaces are safe" check) did legitimately touch Jordan's real
  clipboard twice — once via niri's own unconditional clipboard-clobber
  inside `ScreenshotWindow` (the first, failed run, which never reached
  `storage.rs`'s clipboard-ownership fix because the whole capture errored
  out first) and once via this app's own normal `copy = true` default (the
  second, successful run). Net effect: Jordan's real clipboard currently
  holds a PNG of whatever window was focused during that second test run —
  an ordinary, easily-overwritten side effect of a screenshot tool doing
  exactly what it's supposed to, not a consequence of the click incident,
  but worth knowing about since it happened in the same session.
- Nothing else in this stage's testing touched the real session's window
  state, focus, or files.

**What this stage did in response**: stopped all further live-UI testing
immediately, did not attempt to retry the nested-niri script (even
corrected), cleaned up every scratch artifact, and used only the one class
of test already established as safe by prior stages' own precedent
(read-only captures with no surface mapped, no synthetic input) to still
get *some* live signal on the new code — which is exactly what caught and
proved the fix for the `ScreenshotWindow` read-race bug above. That is a
narrower live-verification story than Stage 7 delivered (which had a full
drag→resize→move→confirm→cancel round trip live-tested), and it should
read as narrower, not as equivalent.

**For whoever runs live input-injection tests next** (Stage 9 probably
doesn't need to — it's a plain toplevel window, not a layer-shell surface —
but Stages 10-13's recording UX and any future overlay work will): CLAUDE.md
now carries a structural fix, not just a warning — export
`WAYLAND_DISPLAY`/`NIRI_SOCKET` once for the whole script's environment
(`export`, not a per-command prefix), and consider making the injector tool
itself refuse to run unless given an explicit `--display` argument rather
than reading `$WAYLAND_DISPLAY` at all, so a missing prefix is a hard error
instead of a silent fallback to whatever display happens to be ambient.
Neither of those existed before this stage; build the first before writing
a second live-test script, and seriously consider the second.

---

## For Jordan

- Your real clipboard currently holds a PNG screenshot of whatever window
  was focused during this stage's second `shot --window --no-daemon` test —
  harmless, but you'll notice it if you paste something and get an
  unexpected screenshot. Overwrite it with a normal copy whenever.
- **A stray synthetic click landed on your real desktop during this
  stage's testing** — one left-click (press+release, no drag, no keyboard)
  at a real-screen position, caused by a missing environment-variable
  prefix in a test script that was supposed to be nested-only. See the
  incident section above for the full account. Nothing else in this stage's
  testing touched your real session's input, and the agent stopped all
  further live-UI testing the moment this was discovered. Worth a moment to
  check nothing you were doing at the time got interrupted — a closed tab,
  a moved window, a toggled setting.
- Bind line for `--window` once you want one (you edit
  `~/.config/niri/config.kdl` yourself, per the sudo rule):
  ```kdl
  binds {
      Mod+Shift+4 hotkey-overlay-title="Screenshot: window" {
          spawn "saola-capture" "shot" "--window"
      }
  }
  ```
- **Human verification still pending, in order of how much this stage
  actually exercised them**: (1) the region overlay's Window button, real
  session or nested niri — unit-tested only; (2) a delayed shot's countdown
  pill actually rendering and counting down visibly — unit-tested only;
  (3) `shot --window` via the *daemon* (not `--no-daemon`) — the running
  real daemon predates this stage's binary, so it was deliberately left
  alone rather than restarted (same posture Stage 7's handoff already
  established: restarting your daemon is your call, not an agent's) —
  `pkill -f 'saola-capture daemon'` picks up the new binary on the next CLI
  verb, same as always.
