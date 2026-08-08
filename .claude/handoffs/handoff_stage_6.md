# Stage 6 handoff — flash + toast: the PrintScr MVP

Forward-facing context for **Stage 7** (region selection overlay). New files:
`src/modules/mod.rs`, `src/modules/flash.rs`, `src/modules/toast.rs`. Touched:
`src/main.rs`, `src/dbus.rs`, `CLAUDE.md`. Nothing committed.

Verification: `cargo build`, `cargo clippy --all-targets -- -D warnings`,
`cargo fmt --check` clean; `cargo test` **139 passed** (112 from Stage 5 + 27
new), 10 consecutive clean runs. Live-tested in nested niri (recipe below) —
**not** just unit-tested; two real bugs were found and fixed by that live
check (§"Two live-found bugs" below), so read that section before touching
surface lifecycle code.

---

## What Stage 7 can now assume works

`shot --fullscreen` via the daemon flashes, saves, copies, toasts, and emits
`CaptureTaken` — the whole PrintScr flow, live-verified in nested niri (grim
screenshots + pixel sampling, not just "it compiled"). Stage 7's job is
`--region` with no `--geometry`: mapping the overlay, freezing the frame,
letting the user drag, cropping, and feeding the result into the exact same
`capture::take_screenshot` → `storage::save_capture` → (now) flash+toast tail
this stage wired up. **You do not need to touch the flash/toast code at
all** — only add a third `SurfaceRole` variant and wire the overlay's own
spawn/remove into the same `Daemon` struct.

---

## The surface registry, as it now works (read this before adding the overlay)

`main.rs`'s `SurfaceRole` enum has two variants today: `Flash`, `Toast`. Add
`Overlay(...)` (or similar) as a third. The machinery Stage 3 promised
("the SurfaceRole registry in place") is now actually exercised:

```rust
fn spawn_surface(&mut self, role: SurfaceRole, settings: NewLayerShellSettings)
    -> (window::Id, Task<Message>);   // mints the Id via Message::layershell_open,
                                       // registers the role synchronously — no
                                       // window in which `view` sees an
                                       // unclassified Id
fn remove_surface(&mut self, id: window::Id) -> Task<Message>;  // unregisters + RemoveWindow
```

Both are copied verbatim from `saola-panel::main::Panel::spawn_surface`/
`remove_surface` — same shapes, same reasoning, reuse them for the overlay.

**Two different surface-lifecycle patterns exist now, and the overlay
probably wants a third:**

1. **Flash: spawned once, at `Daemon::boot`, never torn down.** See "Two
   live-found bugs" below for *why* — a surface created fresh per capture
   can lose its entire visible window to Wayland/GPU setup latency. The
   flash toggles only its own opacity; the surface itself is permanent,
   click-through (`events_transparent: true`), and invisible at rest.
2. **Toast: spawned on the first card, unmap-then-**respawn** (never a live
   resize) whenever the card count changes, unmapped when the stack empties.**
   `Daemon::sync_toast_surface`'s doc comment has the full reasoning: a
   layer-shell surface with `events_transparent: false` takes pointer input
   across its *entire declared area*, reserved or not — sizing it for 3
   cards while showing 1 would silently eat clicks meant for whatever's
   underneath the blank space. `main.rs` tracks `toast_surface_count` (the
   size the *currently mapped* surface was built for) separately from
   `toasts.len()` (what's needed *now*) to detect the mismatch.
3. **The overlay is neither.** It's spawned reactively (on `shot --region`
   with no `--geometry`) like the old flash draft, but unlike the flash it
   needs `KeyboardInteractivity::Exclusive` from the moment it maps and
   cannot be pre-warmed at boot the way the flash now is (an
   always-mapped, always-exclusive-keyboard surface would steal focus
   permanently, which is obviously wrong). So the "surface takes time to
   actually render after being requested" risk this stage found is still
   live for you — see the next section for what to actually watch for.

---

## Two live-found bugs (both fixed; both matter for Stage 7)

Both were invisible to `cargo test` and only surfaced by live-testing in
nested niri per CLAUDE.md's binding rule — read this section before writing
any new surface-mapping code.

### 1. A surface spawned reactively can lose its whole visible window to setup latency

First draft spawned the flash surface fresh on every `CaptureTaken` and tore
it down once `Flash::is_active` went false (mirroring the toast). Live check
(`niri msg layers` immediately after `shot --fullscreen`, plus ten
consecutive `grim` captures fired the instant the CLI returned, pixel-sampled
with `magick -format "%[pixel:p{50,50}]" info:`): the *toast* reliably
appeared (it stays mapped up to 6.35 s — plenty of slack), but the **flash
never once rendered a visible frame** in ten tries. The chain from
"`CaptureService::screenshot` sends a `DaemonEvent`" to "a pixel is on
screen" crosses several independent scheduler hops (events channel →
`dbus_worker_stream`'s forward → iced's message queue → `NewLayerShell` →
the compositor's configure round trip → first `wgpu` frame), and on the test
machine that chain alone ate a meaningful fraction of the flash's ~140 ms
fade budget before anything was ever composited.

**Fix**: the flash surface is now spawned once, at `Daemon::boot`, and never
torn down (`Daemon::boot`'s doc comment has the full account).
`Flash::trigger` only has to change an *already-mapped* surface's opacity,
never create one. Re-verified after the fix: `srgb(255,255,240)` (full ivory)
in 7 of 8 rapid `grim` captures immediately after a `shot`, correctly back to
plain background at +300 ms and +1000 ms.

**Relevance to Stage 7**: the overlay has the same "must appear the instant
it's requested" pressure the flash had, and *cannot* use the same fix (it
needs exclusive keyboard on map, which a permanently-mapped surface can't
have without permanently stealing focus). If the overlay is slow to appear
live in nested niri, this is the first thing to suspect — check
`niri msg layers` timing and a burst of `grim` captures the way this stage
did, don't just trust that the code "looks right."

### 2. The app-wide surface background must be set transparent, explicitly

Once the flash surface was made permanent (fix #1), a *second* bug became
visible that fix #1's own tests would never have caught on their own: at
rest, the flash surface rendered as a **solid ink-colored rectangle covering
the entire output**, permanently — `srgb(12,10,0)` (exactly `palette.ink`)
sampled a full second after a capture, when the flash should long since have
faded to nothing.

**Root cause**: `main.rs` never called `.style(...)` on the `daemon(...)`
builder. Without it, iced clears every surface to `to_iced_theme`'s
`background` field (`palette.ink`) before drawing anything; the flash's own
semi-transparent ivory container was correct in isolation but was being
alpha-blended *over that opaque ink base*, not over true Wayland
transparency. At opacity `0.0` that composites to solid ink.

This was invisible with the *reactive* flash (fix #1's original shape): the
opaque-ink surface only existed for the ~140–300 ms the surface was mapped,
gone again before anyone looked. It only became a permanent, obvious bug
once fix #1 kept the surface mapped forever.

**Fix**: `Daemon::style` — copied verbatim from
`saola-panel::main::Panel::style`:

```rust
fn style(&self, theme: &iced::Theme) -> iced::theme::Style {
    iced::theme::Style { background_color: iced::Color::TRANSPARENT, ..iced::theme::default(theme) }
}
```

wired via `.style(Daemon::style)` in `run_daemon`. Re-verified: idle pixel
sample is `srgb(64,64,64)` (plain nested-output gray), not ink, both before
any capture and after a flash has faded.

**Relevance to Stage 7**: `.style(Daemon::style)` is already wired for the
whole daemon (every surface, present and future, inherits it) — you do not
need to add this again. But if the overlay's frozen-frame background or the
scrim look wrong (an opaque ink layer where you expected the frozen frame or
transparency to show through), this is the mechanism to know about — it
would mean something in the overlay's own widget tree isn't covering 100% of
its surface with an explicit style, and the app-wide transparent default is
showing through in a spot you didn't intend.

---

## The interfaces landed, exact signatures

### `src/modules/flash.rs`

```rust
pub fn fade(theme: &Theme) -> Duration;   // theme.motion.hover — see gap note below

#[derive(Debug, Default, Clone, Copy)]
pub struct Flash { /* private: started: Option<Instant> */ }
impl Flash {
    pub fn trigger(&mut self, now: Instant);
    pub fn is_active(&self, now: Instant, fade: Duration) -> bool;
    pub fn opacity(&self, now: Instant, fade: Duration) -> f32;      // 1.0 -> 0.0 linear
    pub fn subscription(&self, now: Instant, fade: Duration) -> Subscription<Message>;
    pub fn view(&self, theme: &Theme, now: Instant, fade: Duration) -> Element<'static, Message>;
}

#[derive(Debug, Clone, Copy)]
pub enum Message { Tick }   // no payload — see the dead-field note below
```

No `dismiss()`/reset method — deliberately removed once the surface stopped
being torn down (nothing left to reset between captures; `is_active`/
`opacity` are correct for arbitrarily large elapsed time on their own, unit
test `a_long_faded_flash_stays_inert_without_ever_being_reset` pins it).

### `src/modules/toast.rs`

```rust
pub fn thumbnail_handle(frame: &capture::Frame, max_dim: u32) -> image::Handle;
pub fn card_stack_height(theme: &Theme, count: usize) -> u32;  // surface height for `count` cards

#[derive(Debug, Default)]
pub struct ToastStack { /* private: toasts: Vec<Toast>, next_id: u64 */ }
impl ToastStack {
    pub fn is_empty(&self) -> bool;
    pub fn len(&self) -> usize;
    pub fn push(&mut self, path: PathBuf, thumbnail: image::Handle, theme: &Theme, now: Instant);
    pub fn update(&mut self, message: Message, now: Instant, theme: &Theme) -> Action;
    pub fn subscription(&self) -> Subscription<Message>;
    pub fn view(&self, theme: &Theme, now: Instant) -> Element<'static, Message>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action { None, Open(PathBuf) }   // main.rs turns Open into spawn_editor(&path)

#[derive(Debug, Clone, Copy)]
pub enum Message { Tick, Hovered(u64), Unhovered(u64), Clicked(u64) }
```

### `src/dbus.rs` (signature changes)

```rust
pub enum DaemonEvent { CaptureTaken { path: String, thumbnail: iced::widget::image::Handle } }

pub async fn serve(
    connection: &Connection,
    events: iced::futures::channel::mpsc::Sender<DaemonEvent>,  // NEW parameter
) -> zbus::Result<ServeOutcome>;
```

`capture_and_save` now returns `Result<(SavedCapture, Frame), String>` (the
`Frame` used to be dropped after `save_capture`; Stage 6 needs it in hand to
build the toast thumbnail without a second file read + decode).
`CaptureService` gained an `events` field (the `DaemonEvent` sender). The
**wire contract is unchanged** — `Screenshot(kind, options) -> s` and the
`CaptureTaken(path, kind)` signal are exactly what Stage 5 shipped;
`DaemonEvent` is a purely in-process bridge, invisible on the bus.

### `src/main.rs` (new pieces)

```rust
enum SurfaceRole { Flash, Toast }   // Stage 7 adds a third
fn spawn_surface(&mut self, role, settings) -> (window::Id, Task<Message>);
fn remove_surface(&mut self, id) -> Task<Message>;
fn flash_surface_settings() -> NewLayerShellSettings;             // called once, from boot
fn toast_surface_settings(theme, count) -> NewLayerShellSettings; // called on every resize
fn spawn_editor(path: &Path) -> std::io::Result<()>;              // `window edit <path>`, detached
```

`Daemon` gained `theme: Theme`, `flash: modules::flash::Flash`,
`toasts: modules::toast::ToastStack`, `toast_surface: Option<window::Id>`,
`toast_surface_count: usize`. **No `flash_surface` field** — the flash's Id
lives only in the `windows` registry now, since nothing ever needs to look
it up again (it's never removed, never resized).

---

## D-Bus → iced bridge (the mechanism Stage 7 doesn't need, but should know exists)

`dbus_worker_stream` (`main.rs`) used to just hold the D-Bus connection open
forever (`std::future::pending::<()>().await`). It now also owns a small
bounded `iced::futures::channel::mpsc::channel::<DaemonEvent>(8)`: the
sender half is moved into `dbus::serve` (and from there into
`CaptureService`), the receiver half is looped on
(`while let Some(event) = events_rx.next().await`), translating each
`DaemonEvent` into a `Message` and forwarding it into the daemon's own
subscription channel. **`CaptureService::screenshot` uses `try_send`, never
`.send().await`** — a full channel (or a dead receiver) degrades to a logged
warning, never a blocked D-Bus reply. Picked `iced::futures::channel::mpsc`
over adding `tokio`'s `sync` feature specifically to keep this at **zero net
new crates/features** (matches this repo's habitual bar — see the `libc`/
`serde_json` surveys in `Cargo.toml` for the same posture). If a future
stage needs a second such bridge (Stage 10's PipeWire thread → daemon, most
likely), this is the pattern to copy.

---

## Gotchas (mechanical, not architectural — worth having in hand)

- **`iced_widget::Space::new()` takes zero arguments** in this crate's
  resolved iced/iced_widget version (0.14.2) — `Space::new(w, h)` and
  `Space::with_width(w)` (both plausible-looking APIs from memory/other iced
  versions) don't exist here. Use `Space::new().width(w).height(h)`.
- **`iced::widget::image::Handle` derives `Clone`/`PartialEq`/`Eq` but not
  `Debug`** (verified directly in `iced_core-0.14.0/src/image.rs`). If a
  `Handle` needs to ride inside a type that derives `Debug` (here,
  `main.rs`'s `Message`, which `#[to_layer_message(multi)]` requires to
  derive it), wrap it in a newtype with a hand-written `Debug` — see
  `main.rs`'s `Thumbnail`.
- **`futures::channel::mpsc::Sender::try_send` needs `&mut self`.** Calling
  it from a `&self` context (a zbus interface method) means cloning first:
  `self.events.clone().try_send(...)` — the clone is a cheap refcount bump,
  and Rust's temporary-borrow rules let a `&mut self` method run on that
  owned temporary directly.
- **`niri msg layers`** (not `windows`, not `outputs`) is the introspection
  command for layer-shell surfaces — lists namespace + keyboard
  interactivity per output/layer, no size/position (grim + pixel sampling
  is how this stage checked geometry/timing instead).
  `magick -format "%[pixel:p{X,Y}]" info: file.png` is a fast, scriptable
  way to assert a pixel's color without eyeballing every screenshot — used
  throughout this stage's live checks.
- **niri's own default config already binds `Print { screenshot; }`**
  (`/usr/share/doc/niri/default-config.kdl:611`) — the bind line below
  *replaces* that line, it doesn't sit alongside it.

---

## saola-theme gaps hit (documented, not restyled locally, not upstreamed this stage)

Same posture `saola-lockscreen::modules::reveal` established: derive from
existing tokens with the derivation spelled out, flag the gap, don't
hardcode a bare literal and don't edit `saola-theme` for what might be a
one-off need. No tag bump this stage.

1. **No dedicated flash/shutter duration.** `modules::flash::fade` reuses
   `motion.hover` (140 ms) — closest existing token to the style guide's
   "~150 ms", and semantically the right family (a bare colour/opacity
   transition, unlike `motion.popover`'s 160 ms translate+scale entrance).
2. **No opaque-ink "card" style helper.**
   `saola_theme::style::container::card(theme, Surface::Ink)` does the
   *opposite* of what §6's notification card wants — it paints an **ivory**
   card (content floating on ink), not an ink card. `modules::toast::
   ink_card_style` composes the right thing locally from `palette.ink`,
   `on_ink.primary`, `radii.card`, `shadows.popover` (whose `0 18px 48px
   rgba(12,10,0,.5)` is, gratifyingly, byte-for-byte §6's spec value — no
   gap there).
3. **No `icon_tile` size token.** §6's "36px icon tile" has no `Sizes`
   field (`icon_bare` is 32–34 for the power menu, `list_row` is 38 —
   neither is it). `modules::toast::ICON_TILE_SIZE` is the spec's literal
   value, named and documented at its one definition site.
4. **No life-rule thickness token.** §6's "3px life rule" — same posture,
   `modules::toast::LIFE_RULE_HEIGHT`.

If Stage 7's overlay needs `radii.selection` (6 px — this one **does**
exist, verified in `saola-tokens/src/tokens.rs`) or other overlay-specific
tokens, check `saola-tokens/src/tokens.rs`/`palette.rs` directly rather than
assuming from the style guide prose; `scrim.capture` also already exists
(`Scrim::capture`, verified) so the "outside the selection" dimming needs no
new token either.

---

## What was live-tested vs. unit-tested only

**Live-tested** (nested niri, `niri -c /tmp/nested-niri.kdl &`, no
`--session`, `WAYLAND_DISPLAY`/`NIRI_SOCKET` overridden explicitly, torn
down after — verified no nested-niri process remained and the real
session's `eDP-1` output was unaffected):

- Daemon boot with the flash surface pre-mapped (`niri msg layers` showed it
  immediately, before any capture).
- `shot --fullscreen` end-to-end through the daemon (D-Bus, not
  `--no-daemon`), repeated ~8 times across the debugging session.
- The flash's full visible cycle: full ivory at trigger, correctly faded to
  fully transparent (not ink — see bug #2) by +300 ms and +1000 ms.
- The toast's full visible cycle: slide-in-adjacent content visible by
  ~550 ms, stack of exactly 3 after a 4th capture (oldest dropped, newest on
  top, each life rule at a visibly different fill), correct title/app-name/
  filename/thumbnail-tile layout, all matching §6 by eye.
- Surface count transitions in `niri msg layers` (2 surfaces → 1 as the
  flash's tick-driven logic used to unmap it, pre-fix; now flash+toast
  coexist and only the toast count changes).

**Unit-tested only, not live-input-tested**: toast hover-pause and click
(`Message::Hovered`/`Unhovered`/`Clicked` — no virtual-pointer injection was
set up this stage; Stage 2's `inject` tool, per its probes, is how a future
stage would do this live). The click path's *other* half — `spawn_editor`
actually invoking `saola-capture window edit <path>` — is exercised by
`run_window`'s existing Stage 3 stub behavior (prints and exits 0) but was
not fired from a live toast click.

**§11 checklist** (both surfaces) walked in a code comment inside
`main.rs`'s module doc comment area — grep `§11` in `src/main.rs` if it
moves.

---

## For Jordan (bind lines — he edits `~/.config/niri/config.kdl` himself)

niri's own default config already has `Print { screenshot; }`
(`/usr/share/doc/niri/default-config.kdl:611`) — **replace that line**, not
add alongside it:

```kdl
binds {
    Print hotkey-overlay-title="Screenshot: fullscreen" {
        spawn "saola-capture" "shot" "--fullscreen"
    }
    // Stage 7 lands the overlay; once it does:
    // Mod+Shift+S hotkey-overlay-title="Screenshot: region" {
    //     spawn "saola-capture" "shot" "--region"
    // }
}
```

**Human verification is pending and out of this worker's hands**: press
`Print` in the real session and confirm flash + toast + a WebP on disk. This
stage's nested-niri checks prove the mechanism works and found/fixed two
real bugs, but Jordan's real eDP-1/680M hardware is faster than the test
environment and has never run this code — the flash in particular should be
watched closely given bug #1's root cause (surface-creation latency, which
could in principle vary by hardware/compositor load in ways the sandboxed
nested-niri test didn't explore).
