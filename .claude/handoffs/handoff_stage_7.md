# Stage 7 handoff — the region selection overlay

Forward-facing context for **Stage 8** (window capture + delayed capture).
New file: `src/modules/overlay.rs` (1824 lines incl. ~45 tests). Touched:
`src/capture/mod.rs`, `src/dbus.rs`, `src/main.rs`, `src/modules/mod.rs`,
`CLAUDE.md`. Nothing committed.

Verification: `cargo build`, `cargo clippy --all-targets -- -D warnings`,
`cargo fmt --check` clean; `cargo test` **189 passed** (139 from Stage 6 +
50 new). Live-tested end to end in a nested niri with **injected pointer and
keyboard input** — a full drag → resize → move → confirm round trip, plus
cancel by Escape, cancel by button, the Full-screen button, a concurrent
second request, a fumbled toolbar click, and an orphaned overlay. Details in
"What was live-tested" below.

**CLAUDE.md was updated in this stage**, per its own rule: the status block
(interactive `--region` is no longer a stub), the Commands block (`shot
--region` and the "can block for minutes / exits 1 on cancel" contract), two
Architecture bullets (the now-real freeze-first chain; the three surface
lifecycles), the D9/D10 multi-output posture, a Stage 7 theme-gap entry, six
new iced 0.14.2 gotchas, and four additions to the nested-niri test recipe.

---

## What Stage 8 can now assume works

`shot --region` with no `--geometry` is a real, complete flow through the
daemon: freeze → overlay → drag → crop → save → clipboard → flash → toast →
`CaptureTaken`. The scriptable `--geometry` path is untouched and still
skips the overlay entirely. `--no-daemon --region` with no geometry now
reports *why* it can't work rather than naming a future stage — there is no
later stage that gives a surfaceless process an overlay.

**Stage 8's two jobs both have their hook already in place:**

1. **`--window`.** The overlay's toolbar already renders a **"Window"**
   button, deliberately *disabled* (no `on_press`, so `button::rest`'s own
   `Status::Disabled` arm paints it). Wiring it is one `.on_press(..)` in
   `modules::overlay::toolbar` plus a new `Action` variant — the button, its
   layout, its styling and its §11 justification are done.
2. **`--delay`.** Honoured today as a plain sleep inside
   `capture::freeze_focused_output`, i.e. *before* the overlay maps, so the
   frozen frame shows the delayed state rather than delaying the selection.
   That is one defensible reading; if Stage 8's countdown surface wants the
   other (select first, then count down, then capture), that is a change to
   the *order of the three hops* in `dbus::CaptureService::interactive_region`
   and nothing else — but note it would require a **second** screencopy after
   the overlay drops, which is exactly what CAPTURE-RESEARCH §1.5 forbids
   while a surface is mapped. The safe version: unmap the overlay, run the
   countdown on its own surface, freeze again, crop the stored rect.

---

## The overlay input model (read this before adding any surface that mixes
## raw events with widgets)

**Raw events and a widget tree share one surface**, and the discriminator is
`iced::event::Status`.

```rust
// main.rs — gated to only run while an overlay is up.
fn overlay_event_subscription() -> Subscription<Message>   // event::listen_with
Message::OverlayEvent { id: window::Id, event: iced::Event, captured: bool }

// modules/overlay.rs — pure, unit-tested.
pub fn message_from_event(event: &iced::Event, captured: bool) -> Option<Message>
```

Four rules, each of which was a bug before it was a rule:

- **`listen_with` takes a `fn` pointer, not a closure** — it cannot capture
  the overlay's `window::Id` to filter on. The Id rides in the message and
  `Daemon::update` compares it against `self.overlay_surface`. Without that
  comparison, a click on a *toast card* would be read as an overlay drag,
  because pointer coordinates are surface-relative.
- **Presses respect `captured`; releases and motion do not.** A press
  captured by a toolbar button must not also start a drag. A release must
  always be forwarded (a drag that starts on the canvas and ends over the
  toolbar has to end), and motion must always be forwarded (the overlay
  needs to keep tracking the pointer while it is over a button).
- **A disabled `button` does not capture its press.** iced only captures
  when `on_press` is `Some`. So the toolbar card, the gaps between its
  pills, and the disabled Window button all fell through to "start a new
  drag". Fix: the whole bar is wrapped in
  `mouse_area(..).on_press(Message::ToolbarPressed)` — a deliberate no-op
  message whose only job is to make iced capture the event. iced skips the
  `mouse_area` entirely when a child button already captured, so real button
  presses are unaffected. **Live-verified**: press-and-drag on the toolbar
  background leaves the existing selection untouched.
- **The size readout deliberately does *not* swallow presses.** It floats
  over screen the user may well want to select; starting a drag there is
  correct.

Keyboard: Escape → cancel, Enter → confirm, both ignoring `captured`
(nothing on this surface takes keyboard focus, and CAPTURE-RESEARCH §6.6's
advice is "handle Escape identically on every surface").

### The drag state machine

`Interaction` is `Idle | Creating { anchor } | Moving { grab, origin } |
Resizing { handle }`. Two non-obvious choices:

- **`Moving` stores the rectangle as it was at press time** and translates
  *that* by the total delta on every motion, never accumulating per-frame
  deltas. Rounding and clamping therefore cannot make a move drift.
- **`resize` returns a possibly-*flipped* handle**, and the caller stores
  it. Dragging the left edge past the right turns the rectangle inside out;
  the honest answer is that the pointer now holds the *right* edge. Handles
  are modelled internally as an `(HEdge, VEdge)` pair precisely so flipping
  is `handle.flipped()` per axis rather than an eight-way match.

### Snapping and rounding

Two pipelines, and mixing them up is a real bug:

- `settle_shape` (create/resize) — `snap_edges_to_bounds` → `snap_to_pixels`
  → `clamp_shape`. Each edge moves independently; the size *may* change.
- `settle_position` (move) — `snap_position_to_edges` → round the origin →
  `clamp_position`. The size is preserved exactly. Using the shape pipeline
  for a move would resize a rectangle whenever one edge came near a screen
  edge.

`snap_to_pixels` rounds **each edge independently** and takes the size as
the difference — the same rule `capture::logical_to_pixel_rect` uses, for
the same CAPTURE-RESEARCH §1.4 reason (adjacent selections tile with no
seam), and because a readout that rounds differently from the crop is a
readout that lies.

---

## New / changed interfaces, exact signatures

### `src/modules/overlay.rs`

```rust
// Geometry core — pure, no Theme, no iced widgets, no clock.
pub struct Rect { pub x: f32, pub y: f32, pub width: f32, pub height: f32 }
impl Rect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self;   // clamps size >= 0
    pub fn from_corners(a: Point, b: Point) -> Self;               // normalizes
    pub fn right(&self) -> f32;  pub fn bottom(&self) -> f32;
    pub fn contains(&self, point: Point) -> bool;                  // edge-inclusive
    pub fn translated(&self, dx: f32, dy: f32) -> Self;
}

pub enum Handle { TopLeft, TopRight, BottomRight, BottomLeft, Top, Right, Bottom, Left }
impl Handle {
    pub const ALL: [Handle; 8];              // corners FIRST — hit_test returns the first match
    pub fn center(self, rect: Rect) -> Point;
}
pub enum HitTarget { Handle(Handle), Inside, Outside }

pub fn hit_test(selection: Option<Rect>, point: Point, hit_radius: f32) -> HitTarget;
pub fn resize(rect: Rect, handle: Handle, pointer: Point, bounds: Rect) -> (Rect, Handle);
pub fn clamp_position(rect: Rect, bounds: Rect) -> Rect;     // slide, keep size
pub fn clamp_shape(rect: Rect, bounds: Rect) -> Rect;        // intersect, may shrink
pub fn snap_edges_to_bounds(rect: Rect, bounds: Rect, distance: f32) -> Rect;
pub fn snap_position_to_edges(rect: Rect, bounds: Rect, distance: f32) -> Rect;
pub fn snap_to_pixels(rect: Rect) -> Rect;
pub fn settle_shape(rect: Rect, bounds: Rect) -> Rect;
pub fn settle_position(rect: Rect, bounds: Rect) -> Rect;

// State.
pub struct FrozenFrame(/* private image::Handle */);   // Clone + hand-written Debug
impl FrozenFrame { pub fn new(handle: image::Handle) -> Self; }

pub struct Overlay { /* private */ }
impl Overlay {
    pub fn new(frame: FrozenFrame, output: capture::OutputInfo) -> Self;
    pub fn selection(&self) -> Option<Rect>;
    pub fn update(&mut self, message: Message) -> Action;
    pub fn view(&self, theme: &Theme) -> Element<'static, Message>;
    // no subscription() — this surface has no motion at all (see §11 item 8)
}

pub enum Action { None, Confirm(capture::LogicalRect), Cancel }
pub enum Message {
    CursorMoved(Point), Pressed, Released,
    Confirm, Cancel, SelectFullOutput, ToolbarPressed,
}
pub fn message_from_event(event: &iced::Event, captured: bool) -> Option<Message>;
```

**`Overlay` has no `subscription()`** and that is deliberate, not an
omission: §11 item 8 asks whether anything animates, and this is the first
Saola surface where the answer is "nothing". The selection follows the
pointer — direct manipulation, not animation.

### `src/capture/mod.rs` (two new functions)

```rust
pub fn freeze_focused_output(backend: &dyn CaptureBackend, options: &CaptureOptions)
    -> Result<(Frame, OutputInfo), CaptureError>;   // honours --delay; the Fullscreen
                                                    // arm of take_screenshot now calls this
pub fn crop_frozen_frame(frame: &Frame, output: &OutputInfo, region: LogicalRect)
    -> Result<Frame, CaptureError>;                 // logical_to_pixel_rect + Frame::crop
```

`take_screenshot`'s `Region`-without-`geometry` error text changed (it no
longer names Stage 7; it names `--no-daemon` and `--geometry`). One existing
test was renamed and rewritten to match.

### `src/dbus.rs`

```rust
pub enum DaemonEvent {
    CaptureTaken { path: String, thumbnail: image::Handle },
    BeginRegion {                                        // NEW
        frame: image::Handle,                            // a COPY of the frozen pixels
        output: crate::capture::OutputInfo,
        reply: iced::futures::channel::mpsc::Sender<RegionOutcome>,
    },
}

pub enum RegionOutcome {                                 // NEW
    Selected(crate::capture::LogicalRect),
    Cancelled,
    Unavailable(&'static str),
}

// private, in a PLAIN impl block (not the #[zbus::interface] one):
async fn CaptureService::interactive_region(&self, options: CaptureOptions)
    -> Result<(SavedCapture, Frame), String>;
```

The **wire contract is unchanged**: `Screenshot(kind, options) -> s` and
`CaptureTaken(path, kind)` are exactly what Stage 5 shipped. A cancel is a
`zbus::fdo::Error::Failed("the region selection was cancelled")`.

### `src/main.rs`

```rust
enum SurfaceRole { Flash, Toast, Overlay }               // third variant

struct RegionRequest { frame: modules::overlay::FrozenFrame,
                       output: capture::OutputInfo,
                       reply: mpsc::Sender<dbus::RegionOutcome> }

enum Message { …, BeginRegion(RegionRequest),
               OverlayEvent { id: window::Id, event: iced::Event, captured: bool },
               Overlay(modules::overlay::Message) }

impl Daemon {
    fn begin_region(&mut self, request: RegionRequest) -> Task<Message>;
    fn update_overlay(&mut self, message: modules::overlay::Message) -> Task<Message>;
    fn finish_overlay(&mut self, outcome: dbus::RegionOutcome) -> Task<Message>;
}
fn overlay_surface_settings(output: &str) -> NewLayerShellSettings;
fn overlay_event_subscription() -> Subscription<Message>;
```

`Daemon` gained `overlay: Option<Overlay>`, `overlay_surface:
Option<window::Id>`, `overlay_reply: Option<mpsc::Sender<RegionOutcome>>`.

---

## The surface lifecycle chosen, and why

There are now **three** patterns in `SurfaceRole`, and Stage 8+ should pick
one consciously rather than copying whichever is nearest:

| | Flash | Toast | **Overlay (new)** |
|---|---|---|---|
| spawned | once, at boot | on the first card | on demand, per request |
| torn down | never | when the stack empties | the moment the user acts |
| resized | never | unmap + respawn | never (always full output) |
| keyboard | `None` | `None` | **`Exclusive`** |
| input | click-through | clickable | swallows everything |

**The overlay cannot use the flash's pre-warm trick**, which is the whole
reason it is a third pattern: an always-mapped `Exclusive`-keyboard surface
would hold the keyboard forever. That re-exposes Stage 6's bug #1
(surface-creation latency), and the reason it is survivable here is
**lifetime, not luck**: the flash's entire existence was ~140 ms, so latency
ate the whole thing; the overlay stays up until the user acts, so latency
only delays the moment it becomes visible.

**Measured, not assumed** (nested niri, 30-frame `grim` burst timed from
process launch): the overlay's first composited frame lands at **~450–560 ms**
after `shot --region` starts. Nearly all of that is process start + D-Bus +
the ~0.3 s screencopy freeze; the surface map itself is a small share. Two
independent runs agreed.

**Ordering inside `finish_overlay` matters**: reply first, unmap second.
The blocking crop-and-save on the D-Bus side then starts while the surface
is being torn down, so the flash and toast land on a screen the overlay has
already left. Verified live — the flash reads `srgb(255,255,240)` (full
ivory) at +150 ms after Enter.

**One selection at a time.** A second `shot --region` while one is up gets
`RegionOutcome::Unavailable` immediately. Two surfaces both holding
`Exclusive` keyboard is a state CAPTURE-RESEARCH §6.6 flags as unspecified
by the protocol; refusing is strictly better than discovering what happens.

**Known, accepted behaviour**: if the *CLI client* is killed while the
overlay is up, the overlay stays mapped until the user presses Escape (there
is no tick subscription to notice the dropped receiver, and adding one would
break "this surface has no motion"). Verified live: Escape recovers cleanly
and the daemon stays healthy. If this ever needs fixing, `mpsc::Sender::
is_closed()` is the hook, and it would want a cheap gated poll.

---

## Multi-output posture (unchanged from D10, now with code shaped for it)

**Single-output for v0.1, deliberately** — PLAN.md Stage 7 task 3 allows it
and CAPTURE-RESEARCH D10 explains why: per-output surface creation is
source-verified but has never been run (this machine has one output; niri's
headless backend has no CLI surface). So:

- The overlay maps on the **focused** output only (whichever
  `capture::freeze_focused_output` picked), targeted with
  `OutputOption::OutputName(name)`.
- A selection cannot leave that output — `bounds` is the output's own
  logical size and every pointer position is clamped to it.
- `Action::Confirm` already carries a **desktop-logical** `LogicalRect`
  (surface coordinates plus the output's own logical origin), so the value
  crossing the seam is already multi-output-correct; a test pins this against
  an output at origin (1706, 40).
- The multi-output version is: loop `overlay_surface_settings` over
  `backend.outputs()`, keep one `Overlay` shared across surfaces (iced's
  `view(&self, id)` already dispatches per surface), and translate each
  surface's local coordinates into desktop-logical on the way in. Not a
  rewrite. Cross-output drags and multi-`Exclusive` Escape arbitration stay
  untested until Jordan has a second output.

---

## Theme: what existed, what didn't

**Existed, used verbatim** (checked in `saola-tokens/src/`, not assumed from
the style guide): `scrim.capture` (§2's "Capture overlay (outside
selection)" row — 62% ink, and the composited pixel measured
`srgb(32,31,24)` over the nested gray, matching `0.62 × ink` exactly),
`radii.selection` (6 px, §4's own "Capture selection" row),
`palette.accent`, `container::popover` (the floating toolbar's opaque-ink
30 px card + popover shadow), `container::bar_pill` (the readout),
`button::rest` including its `Status::Disabled` arm, `sizes.hit_target_touch`
(44 px button height), `sizes.popover_padding` (20 px), `sizes.island_gap`,
`sizes.panel_margin_islands`, `typography.size.body`, `convert::ui_font`
(tabular by default).

**Derived, documented**: `sizes.window_border` (2 px) is the selection
edge's stroke width — it is the system's one *thin decorative line*
thickness. Measured live at exactly 2 px, exactly `#C67139`.

**Three genuine gaps**, all `modules::overlay` constants, all documented at
their one definition site (Stage 6's posture; no tag bump):
`HANDLE_RADIUS` (5 px — §7 says "round terracotta handles" and never sizes
them), `DASH_SEGMENTS` (`[6, 4]` — §7 is the only place in the guide that
asks for a dashed anything; measured live as exactly 6-on/4-off),
`READOUT_WIDTH` (136 px).

**Explicitly *not* gaps**: `HANDLE_HIT_RADIUS` (12), `EDGE_SNAP_DISTANCE`
(8), `MIN_SELECTION` (4). These describe pointer behaviour, not appearance.
A design system has no opinion on how close to an edge a drag should snap;
upstreaming them would miscategorise interaction as style. Stage 8 should
apply the same test to anything it is tempted to call a gap.

**§11 walked in full** for the overlay in `main.rs`'s module-level doc
comment (grep `§11`). The two answers worth knowing: the **one terracotta
element is the selection** (edge + handles together, which is why the
Capture button is ivory `button::rest` and *not* the terracotta primary,
despite being the live action), and the toolbar is **text pills, no icons**
— `src/icons.rs` still does not exist and four buttons that read perfectly
well as words are the wrong first reason to create it.

---

## What was live-tested vs. unit-tested only

**Live** (nested niri on `wayland-2`, `niri -c <scratch>/nested-niri.kdl &`
without `--session`, output forced to scale 1, a **private D-Bus session
bus** so nothing touched Jordan's real `io.saola.Capture1`, input injected
only inside the nest via `zwlr_virtual_pointer_v1` /
`zwp_virtual_keyboard_v1`, everything torn down after and verified gone):

- Overlay maps with `Keyboard interactivity: exclusive` under namespace
  `saola-capture-overlay` (`niri msg layers`).
- Scrim: `srgb(32,31,24)` outside the selection = `0.62 × ink` over the
  nested background, exactly; `srgb(64,64,64)` (undimmed frozen frame)
  inside it.
- Edge: exactly 2 px wide, exactly `#C67139`, dash pattern exactly 6 on /
  4 off. Handles: `#C67139` discs centred pixel-exactly on the rectangle.
- Drag → 500×500 at (300,400) from an injected (300,400)→(800,900) drag,
  pixel-exact. Readout read "500 × 500".
- Resize by the BottomRight handle → 600×600. Move by an inside drag →
  same 600×600, origin (350,450), edges verified pixel-exact.
- Confirm by **Enter** → exit 0, path on stdout, a **600×600** WebP whose
  pixels are the undimmed frozen frame (no scrim, no chrome).
- Confirm by the **Full screen** button → a 1238×1457 WebP (the whole
  output).
- Cancel by **Escape** and by the **Cancel** button → exit 1, message
  `the region selection was cancelled`, nothing saved, overlay unmapped,
  no exclusive-keyboard surface left behind.
- **Fumbled toolbar click**: press-and-drag on the toolbar card background
  left the existing selection untouched (the `ToolbarPressed` swallow).
- **Concurrent request**: second `shot --region` exited 1 with `a region
  selection is already in progress`; exactly one overlay surface existed;
  the first selection continued and saved correctly (240×180).
- **Orphaned overlay**: `kill -9` on the CLI client left the overlay mapped;
  Escape recovered it; the daemon kept serving (a following `--fullscreen`
  succeeded).
- **Stage 6 tail intact**: flash at full ivory `srgb(255,255,240)` +150 ms
  after Enter, mid-fade `srgb(226,226,214)` at +300 ms, gone by +450 ms; the
  §6 toast card rendered with the right filename and thumbnail.
- **Regressions**: `--region --geometry 400x300+100+100` saved 400×300 and
  mapped **no** overlay; `--fullscreen` through the daemon still works;
  `--no-daemon --region` printed the new error and exited 1.
- Latency burst (above): first composited overlay frame at ~450–560 ms.

**Unit-tested only**: every geometry function's edge cases (flips past both
axes, clamping on all four sides, snap-vs-clamp separation, NaN/inverted
inputs to `clamp_f32`, independent edge rounding), the desktop-logical
translation for an output not at the desktop origin, the physical-pixel
readout at scale 1.5, and `message_from_event`'s captured/uncaptured
handling. The **1.5-scale readout and crop were not exercised live** — the
nested output was pinned to scale 1 so `grim` comparisons stay byte-exact
(CLAUDE.md's rule). The conversion itself is `logical_to_pixel_rect`, which
Stage 5 verified byte-exact against niri at 1.5, so the risk is low, but
"a region shot on Jordan's real 1.5-scale eDP-1" is the obvious thing for
him to try first.

---

## Traps Stage 8 should not re-learn

- **The daemon's environment decides where files land, not the CLI's.** The
  save happens daemon-side, so `XDG_DATA_HOME` / `save-dir` must be
  overridden on the *daemon* process during testing. Overriding them on
  `shot` does nothing and silently appends test rows to
  `~/.local/share/saola/capture/history.jsonl`. (`--output` is the exception
  — it travels over the bus.) This stage did exactly that once and cleaned
  the row back out.
- **Give a nested test session its own D-Bus bus** (`dbus-daemon --session
  --print-address --fork`). Otherwise the test daemon claims the real
  `io.saola.Capture1` and Jordan's `Print` key starts rendering onto a
  nested display.
- **The Stage 6 handoff's `magick` invocation is wrong.**
  `magick -format "…" info: file.png` fails with "no decode delegate"; the
  file comes **first**: `magick file.png -format "%[pixel:p{X,Y}]" info:`.
- **Finding a widget to click**: dump one scanline
  (`magick shot.png -crop WIDTHx1+0+Y +repage txt:`) and look for the runs
  of ivory. That gives exact button centres without guessing from layout
  arithmetic — and it caught that the Capture button is *absent* from the
  ivory runs when no selection exists, which is itself the disabled-state
  check.
- **A `grim`+`magick` sample loop costs ~230 ms per frame** in this
  environment. That is coarser than a 140 ms flash, so catching the flash
  needs `sleep <offset>` before a tight 3-shot burst rather than a uniform
  loop (offsets of 0.15/0.30/0.45/0.60 s bracketed it).
- **`cargo test` count is now 189.** If it drops, something was deleted, not
  fixed.

---

## For Jordan

A `saola-capture daemon` from **before this stage** (PID 172537, started
20:23) is still running in the real session and owns `io.saola.Capture1`
there. It is running the pre-Stage-7 binary image, so `shot --region` will
get the old "needs the interactive selection overlay, which lands in Stage
7" error until it is restarted:

```sh
pkill -f 'saola-capture daemon'    # the next CLI verb auto-spawns a fresh one
```

Bind line, once you want it (this replaces the Stage 6 handoff's commented
placeholder — you edit `~/.config/niri/config.kdl` yourself):

```kdl
binds {
    Mod+Shift+S hotkey-overlay-title="Screenshot: region" {
        spawn "saola-capture" "shot" "--region"
    }
}
```

**Human verification still pending**: a region shot on real eDP-1 at scale
1.5 — the one path this stage could only unit-test, because the nested
output was pinned to scale 1. Expect the readout to show *physical* pixels
(a 400×300 drag reads "600 × 450"), which is a deliberate choice: it is the
size of the file you get. Say if you'd rather it showed logical.
