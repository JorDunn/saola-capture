# Stage 12 handoff — tray item, region/window recording

Forward-facing context for **Stage 13** (audio — touches `RecordSpec`/
`spin_up`/`RecordOptions` this stage also touched) and **Stage 16** (the
history library, which the tray's "Open Saola Capture" and the finish
toast's "open containing dir" both sit next to).

New file: `src/modules/tray.rs` (~700 lines incl. 15 tests). Touched:
`src/dbus.rs`, `src/cli.rs`, `src/encode/mod.rs`, `src/main.rs`,
`src/modules/app.rs`, `src/modules/toast.rs`, `src/modules/mod.rs`,
`Cargo.toml` (added `serde`, survey inline), `CLAUDE.md`. One new
dependency: `serde = { version = "1", features = ["derive"] }` —
**zero net new crates** (already resolved transitively via `serde_json`
and zbus's own `zvariant`), needed only for the dbusmenu server's
`RawMenuNode` wire struct. **Nothing committed**, as every prior stage.

Gates: `cargo build`, `cargo clippy --all-targets -- -D warnings`,
`cargo fmt --check` clean. `cargo test`: **335 passed** (Stage 11: 304).

---

## 0. What is and isn't wired

**Is:** the SNI tray item + dbusmenu (idle/recording icon, Stop recording/
Open Saola Capture/Quit daemon), `record start --region [--geometry]` and
`record start --window [--window-id]` for real (both interactive-overlay and
scriptable paths), the finish toast (click opens the containing directory),
the app window's Record tab target picker and its real hide-until-finished
wait.

**Is not:** the elapsed-time layer-shell chip (deliberately not built — see
§3), audio (still Stage 13's job — `StartRecording` still refuses
`--audio`), a real mouse-click test of the tray icon or the toast (both
would need synthetic input against the real desktop; see §7's live-test
recipe for what *was* exercised without it).

---

## 1. SNI registration — what to know before touching `modules/tray.rs` again

- **Registration string**: the daemon passes its own well-known bus name,
  `io.saola.Capture1`, to `RegisterStatusNotifierItem` — not an object path.
  That resolves to the protocol's default path, `/StatusNotifierItem`, which
  is exactly where the item is exported. No registration-string ambiguity to
  handle on this side (that's the *watcher*'s problem, and this app is never
  a watcher).
- **Menu path**: `/StatusNotifierMenu`, a sibling of the item path, not
  nested under it. `Menu` property points there.
- **Best-effort, retried on watcher-appearance, never blocking.**
  `dbus::serve` `tokio::spawn`s `tray::install` the moment the bus name is
  claimed — it does not await it. `install` exports both objects, tries
  `RegisterStatusNotifierItem` once, and if `org.kde.StatusNotifierWatcher`
  isn't owned yet, watches `NameOwnerChanged` for it and retries exactly
  then. **No polling, ever**, for the registration itself.
- **Live-verified against Jordan's real `saola-panel`** (already running,
  already owning the watcher name — nothing was started for this): the
  item registered immediately (`RegisteredStatusNotifierItems` grew from 3
  entries to 4, `io.saola.Capture1/StatusNotifierItem` among them) and
  deregistered itself cleanly on daemon exit with **no explicit unregister
  call** — the protocol's own "the watcher notices the bus name is gone"
  recovery, confirmed by the list shrinking back to 3 after `kill`ing (via
  the tray's own "Quit daemon" menu item, no less).
- **No panel-side quirks found that needed working around.** The panel's
  `item.rs`/`watcher.rs` (read first, per this stage's own brief) already
  documents the two famous SNI wrinkles — the registration-string ambiguity
  and the no-`PropertiesChanged`-only-`NewX`-signals rule — and this item's
  shape (well-known bus name, `NewIcon`/`NewTitle` on transition) is exactly
  what that reading predicted a well-behaved item should do. One thing
  *not* exercised: `ItemIsMenu` / a pure-menu item with no `Activate` at all
  (this item always answers `Activate` — raises the app window — so
  `ItemIsMenu` is `false`).
- **`GetLayout`/`Event` verified directly over `busctl`** (no mouse needed —
  see §7): `GetLayout(0, -1, [])` returns the three rows with "Stop
  recording" correctly `enabled: false` while idle; `Event(1, "clicked", …)`
  while idle logs `"nothing is recording"` and does nothing destructive;
  `Event(2, "clicked", …)` spawns `saola-capture window` for real;
  `Event(3, "clicked", …)` stops a live recording *and* shuts the daemon
  down cleanly, in that order (`stop_recording_now` first via `tokio::spawn`
  isn't actually awaited before the Quit path — see the gotcha in §7 about
  ordering if that ever matters to a future stage).
- **`RawMenuNode` is a second, independent declaration of the same wire
  shape** `saola-panel::modules::tray::menu::RawMenuNode` decodes — not a
  shared type, not imported from anywhere. That's deliberate (the two ends
  of a D-Bus protocol only need to agree on the wire signature, never a
  source dependency), but it does mean a future protocol-shape change has to
  be made in both places by hand if it's ever needed.

## 2. The recorder-state sharing decision

`dbus::SharedRecorder` (was private, now `pub(crate)`) and `ActiveRecording`
(was private, now `pub(crate)` — only the *type name* needed to be nameable,
fields stay private) are shared directly between `CaptureService` and
`modules::tray`: `dbus::serve` builds one `SharedRecorder` and clones it into
both. The tray reads it with `dbus::lock_recorder` (also now `pub(crate)`)
exactly like `CaptureService` does — no D-Bus round trip to itself, no
second source of truth. **This is why the `Recording` D-Bus property never
got a `PropertiesChanged` emitter in this stage** — the one consumer PLAN.md
expected to want it turned out to run in-process and not need the wire at
all. A future **saola-notifications** (an external process) is still the
reason to eventually wire that signal; nothing in this stage closes that
door, it just wasn't this stage's problem.

`dbus::stop_recording_now(recorder: &SharedRecorder) -> Result<String, String>`
is the extracted, shared body of `StopRecording` — both the D-Bus method and
the tray's "Stop recording" click call it, so there is still exactly one
finalization/waiter contract (`ActiveRecording::waiter`'s own doc comment),
not a menu-only shortcut that could race it differently.
`dbus::spawn_window_process` is now `pub(crate)` for the same reason — the
tray's "Open Saola Capture" calls it directly rather than looping back
through `OpenWindow` over the bus (see CLAUDE.md's updated Stage 9 note on
why).

## 3. The chip decision (PLAN.md task 2)

**Not built. Recorded as future work, with a reasoned "why not" rather than
a placeholder.** PLAN.md's own condition was "add it only if Stage 2/7
evidence says a layer-shell pill is cheap" — Stage 7's own latency
measurement (~450-560 ms surface-creation cost, survivable because the
overlay stays mapped until the user acts) is about a *short-lived* reactive
surface, not a *permanently-mapped, per-second-redrawing* one for the
potentially-minutes-long duration of a recording. Nothing in the existing
evidence measures that cost, and this stage had no safe way to gather it
live (mapping a new layer-shell surface is squarely the nested-niri rule's
territory, and this stage's live-testing deliberately never needed nested
niri at all — see §7). Elapsed time lives on `Title`/`ToolTip` instead (§4).
Whichever stage wants the chip next should budget a nested-niri session
specifically to measure per-second-redraw surface cost before building it.

## 4. The elapsed-time source

`RecorderState::elapsed(Instant::now())` (Stage 11's own addition, doc
comment literally says "for the tray tooltip Stage 12 adds"), read fresh on
every `Title`/`ToolTip` property **get** — no caching, no push. Formatted by
`tray::format_elapsed` (`M:SS`, pure, tested). **No `NewTitle`/`NewToolTip`
emitted per second** — verified against the panel's own source that it has
no rendering path for either property at all today (`Tray::view` never
reads `Title`; grepping `tray/` for "tooltip" finds nothing), so a
per-second push would be D-Bus chatter with no observer. What *is* pushed:
`NewIcon`+`NewTitle` exactly once, on the idle↔recording transition, via
`tray::watch_and_emit`'s 750 ms poll of `is_active()` — the one intentional
poll in this stage, documented at both the function and the module's own
doc comment as the honest answer given `RecorderState` has no change-notify
channel of its own (a possible future refinement: give it one, and replace
this poll with a `recv` — not done here, out of scope for this stage's
budget).

## 5. Region/window recording — the shape that landed

- **`cli::RecordKind`** (`Fullscreen`/`Region`/`Window`) is a **separate
  type** from `ShotKind`, not a reuse — see its own doc comment for why.
  `RecordOptions` gained `kind: RecordKind` and `geometry: Option<Geometry>`;
  `window_id` is now dual-purpose (dry-run's own independent target *and*
  Stage 12's real `Window`-kind target — `RecordOptions::resolve`'s
  validation allows either).
- **`encode::VideoSpec` gained `crop: Option<crate::capture::PixelRect>`**
  (reusing the screenshot-region type, not a new one) and a `with_crop`
  builder. `filter_chain` cropped `(0,0,even_w,even_h)` by default; a
  `Some(rect)` crop rounds `rect`'s **width/height** to even (not its
  offset — offsets are a pointer adjustment, not a chroma-subsampling
  constraint). `-video_size` is **always** the full negotiated frame; only
  the `-vf crop` rectangle narrows for a region. Live-verified:
  `--geometry 640x480+100+100` at scale 1.5 produced a decoded **960×720**
  file, byte-for-byte the same rounding `capture::logical_to_pixel_rect`'s
  own §1.4 tests already assert.
- **`dbus::CaptureService::resolve_record_target`** is the one new
  dispatch point in `spin_up` — `Fullscreen`/`Window` are one `run_blocking`
  lookup each (mirroring `main.rs::run_record_dry_run`'s own target
  resolution, still not shared code with it — different async contexts);
  `Region` either computes a crop from `options.geometry` directly, or (no
  geometry) calls `begin_interactive_region`, which is a **second, smaller
  version** of `interactive_region`'s freeze→`BeginRegion`→wait sequence,
  deliberately not shared with it — the two callers diverge in what they do
  with the frame afterwards (save vs. discard-after-showing-the-overlay) and
  in which options type they have in scope. If a third caller of "freeze +
  ask the overlay" ever appears, that duplication is worth revisiting.
- **The crop is computed against the screenshot backend's own
  `OutputInfo.physical_width/height`, not re-validated against the cast's
  actual negotiated size.** Every live measurement so far (Stage 10/11/12)
  has them agree exactly; documented in `spin_up`'s own comment as an
  accepted simplification — a future mismatch fails as an ffmpeg `crop`
  filter error (visible in the tail), not silent corruption.
- **A window recording never gets a crop at all** — `CastTarget::Window`'s
  own negotiated frame already is just that window (re-confirmed live,
  Stage 10's finding still holds).

## 6. The app window's Record tab

- Grew a `Target` segmented control (`cli::RecordKind`, same three values as
  Screenshot's own), state field `record_target`. `record_options()` now
  takes a `target` parameter; still never sets `geometry`/`window_id` (the
  app always goes through the overlay for region, always resolves the
  focused window — same posture the Screenshot tab already had).
- **Real hide-until-finished, finally.** `StartRecording` succeeding sets
  `recording_pending = true` and leaves the window hidden (no `finish()`
  call); a *new* standing subscription, `modules::app::record_signal_stream`
  — a zero-argument `fn` exactly like `main.rs::dbus_worker_stream`, **not**
  `Subscription::run_with`, since `zbus::Connection` still isn't `Hash` —
  listens for `RecordingFinished`/`Error` on its own independent connection
  and un-hides on whichever arrives. A signal that arrives while nothing is
  pending (the CLI or the tray stopped something this window didn't start)
  is silently ignored — correct, since there's nothing to reopen for.
- **Not live-tested**: opening the real app window, starting a region
  recording from it, and watching it stay hidden/reopen — this needs mouse
  interaction with a real GUI window and was left for Jordan (the PLAN.md
  human-verify item). Everything *below* the GUI layer (the D-Bus calls,
  the crop math, the signal plumbing) was live-verified via the CLI/`busctl`
  instead — see §7.

## 7. Verified live (real session, 2026-08-09) — and the recipe worth reusing

**No daemon owned `io.saola.Capture1`** (checked first); `saola-panel` was
already running and already owned `org.kde.StatusNotifierWatcher` — used
as-is, not started for this. Test daemon ran on the real session bus (niri's
ScreenCast requires it) with `XDG_DATA_HOME` and `capture.toml`'s `save-dir`
pointed at a scratch dir, exactly Stage 11's recipe.

**The genuinely new finding**: none of this stage's surfaces (the SNI item,
the dbusmenu, the crop math, the finish toast) needed nested niri *or*
synthetic input at all. A dbusmenu "click" is a plain `busctl` method call
(`Event(id, "clicked", "", 0)`) — no pointer event, no Wayland surface of
its own. That made it possible to drive the tray menu, verify the crop
resolution, and watch the finish toast render, all from a shell script
against the real session, safely. CLAUDE.md's Testing section now documents
this recipe for future stages that touch the tray.

| check | result |
| --- | --- |
| SNI registration | `RegisteredStatusNotifierItems` 3→4→3 (register, then clean deregister on quit, no explicit unregister) |
| `GetLayout` via `busctl` | 3 rows, ids 1/2/3, "Stop recording" `enabled: false` while idle |
| `record start --region --geometry 640x480+100+100` | HEVC, **960×720** exactly (`crop=960:720:150:150` in the logged ffmpeg argv) |
| `record start --window --window-id <id>` | negotiated and recorded that window's own frame, `crop=1238:1456:0:0` (no narrowing) |
| finish toast | rendered on screen — `grim` + a cropped `magick` capture caught "Recording saved" / "saola-capture" / the real filename, ink card, on the correct output |
| tray "Stop recording" while idle | logged `"nothing is recording"`, no crash |
| tray "Quit daemon" | stopped a live recording, shut the daemon down, released the bus name — `ShutdownReason::TrayQuit`'s own log line printed |
| teardown, every run | `niri msg casts` → "No screencasts"; `pgrep -x ffmpeg` empty; no stray PipeWire `Video/Source`; watcher's item list back to its pre-test 3 |

### The one real bug this stage's own live-testing caught

**`main.rs::run_record`'s real-record branch still hardcoded `"fullscreen"`**
as `StartRecording`'s `kind` argument — a leftover from Stage 11 that
`cargo test` cannot see (it's a cross-process D-Bus argument, not something
any unit test exercises). Every `--region`/`--window` CLI invocation was
silently recording the whole monitor, uncropped, until the first live region
test came back at 2560×1600 instead of the requested 960×720. Fixed to
`options.kind.as_str()` (matching `modules::app::request_recording`, which
*was* correct from the start). **Grep discipline this bug argues for**: after
adding a new `kind`/enum value anywhere in this codebase, `grep -rn
'"fullscreen"'` across `src/` before calling it done — this stage did that
afterwards and found no other stray literals, but doing it *before* the live
test would have caught this one for free.

## 8. Gotchas and open items for Stage 13

- **`RecordSpec.audio` is still `None` everywhere in `spin_up`** — Stage 13
  is the one that resolves an `AudioSpec` and threads it through. Nothing in
  this stage's `resolve_record_target`/crop work touches that path; they're
  orthogonal (a region/window recording with audio is just both features
  composed, not a new case).
- **`spin_up`'s three-way `RecordKind` match is the new place a Stage 13
  audio failure-degradation would need to reach into**, if "audio device
  missing" needs to degrade *before* the cast opens rather than after
  (PLAN.md Stage 13 task 3: "degrades to video-only ... never a dead
  pipeline"). Read `resolve_record_target`'s doc comment for the exact
  step order before adding to it.
- **The tray's "Stop recording" `Event` handler doesn't await
  `stop_recording_now`** — it `tokio::spawn`s it and returns `Ok(())`
  immediately (dbusmenu's `Event` has no meaningful payload to report
  success/failure through, and the panel ignores it anyway). This means a
  `busctl` script (or a future test) that sends `Event(1, "clicked", …)`
  then immediately checks `Recording` will race the actual stop — wait on
  `niri msg casts` or `pgrep -x ffmpeg` instead of the property if that
  matters.
- **`RawMenuNode` duplication** (§1's last bullet) — if the menu's shape
  ever needs to change (a submenu, more rows), remember the panel's own copy
  exists independently and won't notice.
- **The crop-vs-negotiated-size gap** (§5) is real but unexercised by any
  live run so far (every measured cast has agreed with the screenshot
  backend's own physical size). Worth a test if a future machine/output
  config ever disagrees.
- **A close reading of `serde`'s addition**: it is used *only* for
  `modules::tray::RawMenuNode`'s `#[derive(Serialize)]`. `no
  #[derive(Deserialize)]` anywhere in this crate yet (this module only
  *serves* the type, never decodes one) — matches `config.rs`'s
  hand-walked posture the `serde_json` survey already established.
