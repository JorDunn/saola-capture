# Stage 3 handoff — config + CLI + D-Bus dispatch + daemon scaffold

Forward-facing context for Stage 4 (screencopy backend + storage). Nothing in
`src/` besides `config.rs`, `cli.rs`, `dbus.rs`, `main.rs` exists yet — Stage
4 adds `capture/mod.rs`, `capture/screencopy.rs`, `storage.rs`.

---

## The D-Bus interface as actually built

`busctl --user introspect io.saola.Capture1 /io/saola/Capture1`, captured
live against the Stage 3 daemon (`cargo build && ./target/debug/saola-capture
daemon`, real session, surfaceless — safe, no surfaces mapped):

```
NAME                                TYPE      SIGNATURE RESULT/VALUE FLAGS
io.saola.Capture1                   interface -         -            -
.OpenWindow                         method    s         -            -
.PickColor                          method    -         ddd          -
.Screenshot                         method    sa{sv}    s            -
.StartRecording                     method    sa{sv}    -            -
.StopRecording                      method    -         s            -
.CaptureTaken                       signal    ss        -            -
.Error                              signal    s         -            -
.RecordingFinished                  signal    s         -            -
.RecordingStarted                   signal    s         -            -
org.freedesktop.DBus.Introspectable interface -         -            -
.Introspect                         method    -         s            -
org.freedesktop.DBus.Peer           interface -         -            -
.GetMachineId                       method    -         s            -
.Ping                               method    -         -            -
org.freedesktop.DBus.Properties     interface -         -            -
.Get                                method    ss        v            -
.GetAll                             method    s         a{sv}        -
.Set                                method    ssv       -            -
.PropertiesChanged                  signal    sa{sv}as  -            -
```

Matches PLAN.md's Architecture section exactly (`Screenshot(kind s, options
a{sv}) -> s`, `StartRecording(kind s, options a{sv})`, `StopRecording() -> s`,
`PickColor() -> (ddd)`, `OpenWindow(mode s)`; signals `CaptureTaken(path s,
kind s)`, `RecordingStarted(kind s)`, `RecordingFinished(path s)`, `Error(message
s)`).

**Every method is a stub today.** `src/dbus.rs`'s `CaptureService` impl logs
the call to stderr and returns `zbus::fdo::Error::NotSupported`, e.g.:

```
$ busctl --user call io.saola.Capture1 /io/saola/Capture1 io.saola.Capture1 Screenshot "sa{sv}" "fullscreen" 0
Call failed: Screenshot is not implemented yet (Stage 4)
```

Stage 4's job on this file: replace `Screenshot`'s body (currently
`fn not_yet_implemented("Screenshot", "Stage 4")`) with a real call into
`capture/screencopy.rs` + `storage.rs`, and emit `CaptureTaken` on success via
the `#[zbus(signal)]` associated fns already declared on `CaptureService`
(`Self::capture_taken(&emitter, &path, &kind).await`) — the signal-emitting
pattern is documented in `saola-panel::modules::tray::watcher`'s
`Watcher::status_notifier_item_registered`, which `dbus.rs`'s module doc
comment points to.

**Live-verified behaviors** (real session, surfaceless daemon, safe per
CLAUDE.md's nested-niri rule — no surface was ever mapped):
- Single-instance: a second `saola-capture daemon` invocation while one is
  running prints `another instance already owns io.saola.Capture1 — exiting`
  and exits **0**.
- SIGTERM: the daemon logs `received SIGTERM/SIGINT — shutting down` and
  exits 0 (via `iced::exit()` → `Action::Exit` → `ReturnData::RequestExit` →
  the layershellev loop stopping — confirmed the process actually returns,
  not just logs and hangs).
- CLI auto-spawn: with no daemon running, `saola-capture pick-color` spawned
  one detached (`std::process::Command`, stdio to `/dev/null`, not waited on),
  polled for bus ownership, and got a real `PickColor is not implemented yet
  (Stage 15)` error back — round-trip in ~164ms. The spawned daemon kept
  running after the CLI process exited (confirmed via `busctl --user status
  io.saola.Capture1` after the CLI verb returned).

---

## The config schema, verbatim

`~/.config/saola/capture.kdl` (resolution: `--config-dir` >
`$SAOLA_CONFIG_DIR` > `$XDG_CONFIG_HOME/saola` > `~/.config/saola`), wrapped
in a top-level `capture { }` node (mirrors the panel's `panel { }`):

```kdl
capture {
    save-dir "~/Pictures/Screenshots"  // string, default: unset
    image-format "webp"                // "webp" | "png", default "webp"
    png-also #false                    // bool, default #false
    video-preset "hevc"                // "hevc" | "av1" | "h264", default "hevc"
    cursor #true                       // bool, default #true
    delay 0                            // non-negative integer seconds, default 0
    toasts #true                       // bool, default #true (saola-notifications kill-switch)
    copy #true                         // bool, default #true
}
```

**`save_dir: Option<PathBuf>` is deliberately unresolved.** Stage 3 never
invents `~/Pictures/Captures` — that fallback belongs to Stage 4's
`storage.rs` per PLAN.md's own division of labor. `config.rs`'s
`CaptureConfig::save_dir` is `None` when the knob is absent; `~` is expanded
against `$HOME` before that (`config::expand_tilde`), so `storage.rs` never
has to deal with tildes, only `None` vs. an already-absolute `Some(PathBuf)`.

**Gotcha for anyone hand-editing a `capture.kdl` or writing Stage 16's
README**: `kdl = "6.7.1"` defaults to **KDL v2**, where booleans are
`#true`/`#false`, not bareword `true`/`false`. A bareword boolean is a parse
error (rejected as "not identifier string" at the whole-document level, so it
takes the *whole file* down to defaults with a warning — not a per-knob
fallback, since it's a syntax error, not a bad value). This bit Stage 3's own
test fixtures once; CLAUDE.md's Config bullet now says so.

Resilience (unit-tested, `src/config.rs`): no file → defaults, silently.
Malformed KDL → one `eprintln!` + full defaults. A single bad knob value
(wrong type, unrecognized string, negative/fractional `delay`) → warn + that
knob's default, rest of the document still applies.

---

## Surfaces: how they'll get spawned at runtime (nothing spawns yet)

The daemon boots via `iced_layershell::build_pattern::daemon` with
`LayerShellSettings.start_mode: StartMode::Background` — confirmed in
`layershellev-0.19.1/src/lib.rs`: `Background` creates a bare `wl_surface`
with **no shell role**, and is the one `StartMode` whose run loop stays alive
with zero real surfaces mapped (`window_state.units.is_empty() &&
!is_allscreens() && !is_background()` is the *only* condition that stops the
loop — `Active` would have exited immediately with nothing to show).

`Message` is declared `#[to_layer_message(multi)]` (not the single-surface
form) specifically so Stage 5/6 don't have to migrate: `multi` mode is what
injects `Message::layershell_open(NewLayerShellSettings) ->
(window::Id, Task<Message>)` and friends — see
`saola-panel::main::Message`'s doc comment (copied into `main.rs`'s own
`Message` doc comment) for the full injected-variant list and the
`TryInto<LayerShellCustomActionWithId>` mechanics.

`SurfaceRole` (in `main.rs`) is declared but **uninhabited**:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceRole {}

struct Daemon {
    windows: std::collections::HashMap<window::Id, SurfaceRole>,
}
```

`Daemon::view` matches on `self.windows.get(&id)`: `None` renders
`Space::new()`; `Some(role) => match *role {}` is an exhaustive zero-arm match
that documents the pattern without being reachable yet (the map is always
empty). **Stage 5's job**: add real variants (e.g. `Flash`, `Toast`) to
`SurfaceRole`, spawn a surface with `NewLayerShellSettings { output_option:
OutputOption::OutputName(name), .. }` (per CAPTURE-RESEARCH D9 — one per
output from `niri msg outputs`), capture the returned `window::Id` in
`Daemon::update`'s handling of whatever message wraps
`Message::layershell_open`'s result, and insert `(id, SurfaceRole::Flash)`
into `windows` before that Id's first `view` call. `Daemon::view`'s `match`
arm list is where the compiler will force every new variant to be handled —
that's the entire point of keeping the enum exhaustive from day one.

`Daemon::subscription` batches two workers today
(`Subscription::run(dbus_worker_stream)`,
`Subscription::run(shutdown_signal_stream)`); Stage 9's PipeWire thread is
**not** one of these — CLAUDE.md's "one runtime" exception is explicit that
the PipeWire main loop is a bare OS thread bridging in via a bounded channel,
not another `iced::Subscription`.

---

## The resolved `CaptureOptions`/`RecordOptions` shape

`src/cli.rs`. Flags always win; an absent flag falls through to
`CaptureConfig`'s already-defaulted value — see `CaptureOptions::resolve`/
`RecordOptions::resolve`, both pure functions unit-tested directly against
hand-built `ShotArgs`/`RecordArgs` (no argv, no clap parsing needed for the
precedence tests).

```rust
pub struct CaptureOptions {
    pub kind: ShotKind,              // Fullscreen | Region | Window
    pub geometry: Option<Geometry>,  // only meaningful for Region; WxH+X+Y, logical coords
    pub format: ImageFormat,         // Webp | Png
    pub png_also: bool,
    pub output_dir: Option<PathBuf>, // None => storage.rs's ~/Pictures/Captures fallback (Stage 4)
    pub delay: u32,
    pub cursor: bool,
    pub copy: bool,
    pub toast: bool,
    pub no_daemon: bool,
}

pub struct RecordOptions {
    pub action: RecordActionKind,    // Start | Stop | Toggle
    pub preset: VideoPreset,         // Hevc | Av1 | H264
    pub audio: Option<AudioSource>,  // None | Mic | System | Both — no config-file knob, CLI-only
}
```

`CaptureOptions::to_dbus_options()` / `RecordOptions::to_dbus_options()`
build the `a{sv}` map the `Screenshot`/`StartRecording` proxy calls send —
keys: `format`, `png-also`, `output` (only if set), `delay`, `cursor`,
`copy`, `toast`, `geometry` (only if `Region` + `--geometry`) for capture;
`preset`, `audio` (only if set) for recording. **The daemon-side stub
doesn't decode this map yet** — it only logs the option count — so Stage 4
is free to define its own decode-side reading of these exact keys without
worrying about an existing consumer to keep in sync with.

**`ShotKind` default (a judgment call, not spelled out in PLAN.md)**: no
target flag at all (`saola-capture shot` bare) resolves to `Fullscreen`, not
an interactive region pick — chosen because it needs no daemon-side picker to
produce a result, which matters for `--no-daemon` and for scripts that forget
the flag. `Print`'s own bind is `shot --fullscreen` explicitly either way, so
this default is never actually load-bearing for the real keybind.

**`--geometry`'s parser** (`cli::Geometry::parse`) splits on the first two
literal `+` characters (`raw.splitn(3, '+')`), which is correct even for
negative offsets (`800x600+-50+-30`) because the format always writes the
`+` separator literally regardless of the sign of what follows it — verified
by a unit test (`geometry_parses_negative_offsets`).

---

## Gotchas Stage 4 needs

1. **`kdl` 6.7.1 booleans are `#true`/`#false`** (see above) — if Stage 4
   adds any bool knob (there's no obvious one, but worth flagging), don't
   write bareword `true`.
2. **`OwnedValue`'s primitive `From` impls are infallible but the API is
   still `TryFrom`/`From`-per-type, not generic.** `zvariant`'s `to_value!`
   macro only covers a fixed list of primitives (`u8/bool/i16/u16/i32/u32/
   i64/u64/f64/Str<'a>/ObjectPath<'a>`) — notably **not** `String`/`&str`
   directly, only `Str<'a>`. `cli.rs`'s `fixed_str(impl Into<String>) ->
   zbus::zvariant::Str<'static>` is the helper that bridges this; reuse it
   (or its shape) rather than reaching for `OwnedValue::try_from(Value::
   from(a_string))`, which is fallible and would need a `.expect()`-free
   escape hatch under the no-panic rule.
3. **`zbus::fdo::DBusProxy::name_has_owner` returns `zbus::fdo::Result`,
   not `zbus::Result`** — its error type is `zbus::fdo::Error`, which needs
   an explicit `zbus::Error::from(err)` (there's a `From<fdo::Error> for
   zbus::Error` impl) before it fits anywhere expecting `zbus::Error`. Bit
   this stage once; see `dbus::name_has_owner`'s `map_err` for the pattern.
4. **The daemon never decodes its own `a{sv}` options today.** When Stage 4
   makes `Screenshot` real, it has to add the decode side
   (`HashMap<String, OwnedValue>` → whatever `capture/screencopy.rs` and
   `storage.rs` need) — nothing currently reads any key out of that map on
   the served-method side, only the CLI's encode side (`cli.rs`) exists.
5. **`iced_layershell`'s `daemon()` builder never had `.theme(..)`/`.style(..)`
   called on it this stage** (nothing renders, so there was nothing to
   theme) — type inference resolved `Theme = iced::Theme` / `Renderer =`
   the default renderer from `Element<'_, Message>`'s defaulted generics.
   Stage 5 will need to decide whether to wire `saola_theme::to_iced_theme`
   through the same way the panel does (`.theme(Panel::theme)`) the moment
   a real surface exists to theme.
6. **`Daemon::update`'s catch-all `_ => Task::none()` is load-bearing, not
   decorative.** `#[to_layer_message(multi)]` injects ~15 more variants into
   `Message` (`NewLayerShell`, `SizeChange`, `RemoveWindow`, …) that the
   runtime intercepts before `update` ever sees them in practice, but the
   `match` still has to be exhaustive over the *type*, so the wildcard has
   to stay even though (today) `Message` only has the one real variant
   (`Shutdown`) plus the injected ones.
7. **Nothing in `src/` yet touches Wayland/niri-ipc directly** — Stage 3
   added zero new runtime dependencies beyond `tokio` (now a direct
   dependency; see the Cargo.toml essay dated 2026-08-08 for the feature
   list and why `Runtime::new()` specifically needs `rt-multi-thread`).
   `capture/mod.rs`'s `CaptureBackend` trait and `screencopy.rs`'s
   `wayland-client` usage are both still greenfield for Stage 4.

---

## What changed in CLAUDE.md

Status block updated to "Stages 1–3 landed" with the stub-vs-real split
spelled out; Commands section gained the four new verbs
(`pick-color`/`open`/`window edit`/`--config-dir`) plus a note that CLI
verbs are now real D-Bus clients and that booting the daemon is safe in the
real session (surfaceless); the Config bullet in Conventions gained the full
`capture.kdl` schema verbatim and the KDL v2 `#true`/`#false` gotcha.
