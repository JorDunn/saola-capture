# Stage 5 handoff — screencopy backend + WebP/PNG save + clipboard

Forward-facing context for **Stage 6** (camera flash + toast stack → the
PrintScr MVP). New files: `src/capture/mod.rs`, `src/capture/screencopy.rs`,
`src/storage.rs`. Touched: `src/cli.rs`, `src/config.rs`, `src/dbus.rs`,
`src/main.rs`, `Cargo.toml`, `CLAUDE.md`. Nothing was committed.

Verification: `cargo build`, `cargo clippy --all-targets -- -D warnings`,
`cargo fmt --check` clean; `cargo test` **112 passed**, and green on 65
consecutive runs (see "The flaky test that failed attempt 1" below — attempt 1
shipped 110 tests, two of which raced each other about 1 run in 15).

---

## What Stage 6 can now assume works

`shot --fullscreen` and `shot --region --geometry WxH+X+Y` are **real, end to
end, both ways** — through the daemon and via `--no-daemon`. They capture,
encode (WebP by default), write atomically, copy PNG to the clipboard, append
to the history index, print the path to stdout, and (daemon path) emit
`CaptureTaken`. Stage 6 adds the *surfaces*; it does not need to touch the
capture pipeline at all.

**Stage 6's actual job on this code is two hooks**, both inside
`dbus::CaptureService::screenshot` (`src/dbus.rs`), which is the only place
the daemon runs a capture:

1. **Flash** must map *before* `run_blocking(...)` starts, or at least before
   the capture is issued. Screencopy composites layer-shell surfaces (§1.5),
   so a flash surface that is still mapped when the capture runs **appears in
   the screenshot**. Map → capture → unmap is wrong; the safe order is
   capture-then-flash, which is also what the camera metaphor wants.
2. **Toast** goes after the capture returns, gated on `options.toast` (the
   already-resolved `--no-toast`/`toasts` value — do not re-read the config).
   `saved.path` and `saved.png_sidecar` are what it needs; the thumbnail can
   be decoded from `saved.path` or, better, re-derived from the `Frame` before
   it is dropped (a `Frame` is plain RGBA8 — `iced::widget::image::Handle::from_rgba`
   takes it directly, which is the lockscreen's `wallpaper.rs` precedent for
   why you want RGBA and not a path).

---

## The trait and `Frame`, as actually landed

`src/capture/mod.rs`. Deviations from PLAN.md's sketch are noted.

```rust
pub struct Frame { /* private fields */ }        // no Eq: scale is f64
impl Frame {
    pub fn new(width: u32, height: u32, scale: f64, pixels: Vec<u8>) -> Option<Self>;
    pub fn width(&self) -> u32;
    pub fn height(&self) -> u32;
    pub fn scale(&self) -> f64;
    pub fn pixels(&self) -> &[u8];               // RGBA8, tightly packed
    pub fn stride(&self) -> usize;               // always width * 4
    pub fn crop(&self, rect: PixelRect) -> Option<Frame>;   // clamped, not rejected
}
```

**Three `Frame` invariants, enforced by `Frame::new` (the only constructor):**
`pixels.len() == width*height*4` exactly (**no stride padding** — the
compositor's padding is stripped below the boundary), channel order **RGBA**,
row 0 is the **top** row. Alpha is always `0xff` — screenshots are opaque and
the X byte is never propagated. Dimensions are **physical** pixels.
A non-finite or non-positive `scale` is clamped to `1.0` rather than rejected.

```rust
pub struct LogicalRect { pub x: i32, pub y: i32, pub width: u32, pub height: u32 }
pub struct PixelRect   { pub x: u32, pub y: u32, pub width: u32, pub height: u32 }

pub struct OutputInfo {
    pub name: String,           // "eDP-1" — the id every trait method takes
    pub logical: LogicalRect,   // position + size in the logical desktop
    pub scale: f64,             // FRACTIONAL (1.5), not wl_output's integer
    pub physical_width: u32,    // from wl_output's current mode
    pub physical_height: u32,
}

pub trait CaptureBackend {
    fn outputs(&self) -> Result<Vec<OutputInfo>, CaptureError>;
    fn focused_output(&self) -> Result<OutputInfo, CaptureError>;      // 5th method, see below
    fn capture_output(&self, output: &str, cursor: bool) -> Result<Frame, CaptureError>;
    fn capture_region(&self, output: &str, region: LogicalRect, cursor: bool)
        -> Result<Frame, CaptureError>;
    fn capture_window(&self, window: WindowRef, cursor: bool) -> Result<Frame, CaptureError>;
}

pub fn take_screenshot(backend: &dyn CaptureBackend, options: &CaptureOptions)
    -> Result<Frame, CaptureError>;
pub fn logical_to_pixel_rect(region: LogicalRect, output: &OutputInfo) -> Option<PixelRect>;
pub fn output_for_region(outputs: &[OutputInfo], region: LogicalRect) -> Option<&OutputInfo>;
```

**Deviation 1 — a fifth trait method, `focused_output()`.** Something has to
answer "which output does a bare `shot --fullscreen` mean?", and the only
honest answers are compositor-specific. Putting it behind the trait keeps that
knowledge below the boundary; the alternative (a caller reaching into
niri-ipc) is what CLAUDE.md's "behind them, never around them" rule forbids.
Falls back to the alphabetically-first output when niri can't be reached.

**Deviation 2 — the `id` in `capture_output(id, ...)` is the output *name***
(`&str`), not an opaque handle. `OutputInfo::name` carries it, and it is the
same string `iced_layershell`'s `OutputOption::OutputName` wants in Stage 7.

**Deviation 3 — `OutputInfo` carries `physical_width`/`physical_height`**,
which PLAN.md didn't list. It is not `round(logical * scale)`: niri reports
eDP-1's 2560×1600 panel as logical **1706×1066** at scale 1.5, and
`round(1706 × 1.5) = 2559`. Clamping a `--geometry` against the derived value
would shave the last column off a region dragged to the right edge.

`capture_window` and `WindowRef` carry `#[allow(dead_code)]` — implemented
(returns `Unsupported`), called by nobody until Stage 8 adds a window picker.

---

## Formats and y-invert: predictions vs reality

| Stage 2 predicted (D1/§1.1–1.4) | Stage 5 observed |
| --- | --- |
| Exactly one shm format, `Xrgb8888`, stride `width*4` | ✅ Exactly as described. The code still walks the offer list and still reads `stride` from the event. |
| Memory order B, G, R, X → swizzle to RGBA | ✅ Verified live: a `#ff8040` terminal background came out `(255,128,64)`, not `(64,128,255)`. |
| Alpha absent; set A = 255 | ✅ Done unconditionally. |
| `y_invert` flag always 0 on niri | ✅ Still 0, including on the nested output. The inverted branch is unit-tested only. |
| Region `400x300+100+100` at scale 1.5 → physical `600x450` | ✅ **Exactly** — live, through the daemon. |

### ⚠️ The one thing Stage 2 did *not* predict — and it was a real bug

**A screencopy buffer is the output's framebuffer, not what the user sees.**
On an output whose `wl_output.geometry.transform` is not `Normal`, the
captured pixels need the **inverse of that transform** applied. This is
entirely separate from `y_invert` (which stayed 0 throughout, exactly as §1.2
said).

Caught only because the live check ran in nested niri: the winit output
reports `Transform: flipped vertically` (`Flipped180`), and the first draft's
captures were **vertically mirrored** versus `grim` — 41,807 differing pixels,
with terminal text legibly upside-down in a rendered crop. `grim` does this
correction in its own renderer, which is why it was right and this code was
wrong. Jordan's real eDP-1 is `Normal`, so **the real session would never have
revealed it.**

Fixed in `screencopy::undo_output_transform`. After the fix, at output scale 1,
our PNG and grim's are **byte-identical (AE = 0)**.

**Verification status, honestly**: `Normal` and `Flipped180` are live-verified.
All four `Flipped*` transforms are self-inverse, so no choice exists to get
wrong there. The `_90`/`_270` pair is the one place the inverse direction
matters and it is **untested** — `niri msg output winit transform 90` is
silently ignored by the winit backend (verified: the transform stayed
`flipped vertically`), so a rotated output cannot be produced in nested niri.
If a rotated monitor ever shows a screenshot rotated the wrong way, swap the
two arms of `invert_transform`. The unit tests pin `_90` and `_270` as exact
inverses of each other, so only the *labelling* can be wrong, never the maths.

### Also: `grim` is not a byte-exact oracle at fractional scale

grim composites into a surface sized `logical × scale` and resamples; at scale
1.5 (`825 × 1.5 = 1237.5`) ~0.03% of pixels differ by ≤5/255, all at edges and
in gradients. **Set the nested output to scale 1 before demanding AE = 0.**
Same class of artefact §1.4 already flagged for grim's `-g` cropping.

---

## Clipboard behavior

A Wayland "copy" is not a write into a buffer: the copying client keeps a
`wl_data_source` alive and serves the bytes on every paste. That drives
`storage::ClipboardOwner`:

- **`ThisProcess`** (what the daemon uses) — `wl-clipboard-rs`'s default,
  which spawns a **thread** (not a `fork`; checked in the crate source, this
  matters because forking the daemon would be dangerous). It lives as long as
  the daemon or until something else takes the selection.
- **`DetachedHelper`** (what `--no-daemon` uses) — spawns
  `saola-capture clipboard-serve --mime image/png` detached and pipes it the
  PNG on stdin. Verified live: the helper survives the CLI's exit.
  `clipboard-serve` is a **hidden** clap subcommand (`#[command(hide = true)]`).

**The clipboard always gets PNG**, whatever was saved — `image/webp` on the
clipboard is a compatibility trap. PNG bytes are reused when
`image-format = "png"` or `png-also = true`, otherwise encoded once more.

**Not verified**: an actual paste round-trip. `wl-clipboard` is not installed
on this machine (Stage 1's survey already recorded that), so no paste client
exists to test with. Worth one manual check from Jordan: `shot --fullscreen`,
then paste into any app.

Clipboard failures **warn on stderr and continue** — the file is already on
disk by then.

---

## Storage and the history index

`src/storage.rs`. Save dir: `--output` > `save-dir` > `~/Pictures/Captures`,
created on demand. Filenames `Screenshot_YYYY-MM-DD_HH-MM-SS.<ext>` in
**local** time (via `libc::localtime_r`; no colons, so they survive a copy to
FAT/SMB), with `-1`, `-2`, … on collision — and the collision check covers
**both** `.webp` and `.png` so a capture and a later sidecar can never share a
stem. Every write is `.name.part` + `rename` (atomic within a filesystem).

```rust
pub struct SavedCapture {
    pub path: PathBuf,                 // print this; carry it in CaptureTaken
    pub png_sidecar: Option<PathBuf>,  // Some only when png-also && format == webp
    pub width: u32, pub height: u32, pub bytes: u64,
}
pub fn save_capture(frame: &Frame, options: &CaptureOptions, kind: ShotKind,
                    clipboard: ClipboardOwner) -> Result<SavedCapture, StorageError>;
```

`save_capture` is a **thin wrapper**: it resolves `history_path()` (the only
`$XDG_DATA_HOME` read on the path) and delegates to the private

```rust
fn save_capture_indexing_to(frame, options, kind, clipboard,
                            history: Option<&Path>) -> Result<SavedCapture, StorageError>;
```

which does all the work and never touches the environment. `None` = no data
directory at all (no `$XDG_DATA_HOME`, no `$HOME`): warns, still saves. Call
`save_capture` from production; call the inner one from tests. Same split as
`resolve_save_dir`/`default_save_dir` and `history_path`/`history_dir`, and it
is now a **binding convention in CLAUDE.md** — see the last section.

**Index format (stable; Stage 16's library is the consumer).** Append-only
JSON Lines at `$XDG_DATA_HOME/saola/capture/history.jsonl`, default
`~/.local/share/saola/capture/history.jsonl`. One object per line, newest
last, earlier lines never rewritten:

```json
{"bytes":39050,"format":"webp","height":1457,"kind":"fullscreen",
 "path":"/…/Screenshot_2026-08-08_18-35-12.webp","scale":1.0,
 "unix":1786228514,"v":1,"width":1238}
```

Keys: `v` (format version, `1`), `unix`, `path`, `png` (**absent**, not null,
when there's no sidecar), `kind`, `format`, `width`, `height`, `scale`,
`bytes`. **Readers must ignore unknown keys and skip unparseable lines** — a
line half-written by a machine that lost power is the expected failure and is
always the last one. That rule is what lets later stages add keys without a
migration. Index failures warn and continue.

---

## The D-Bus `Screenshot` method, as it now behaves

`Screenshot(kind s, options a{sv}) -> s`. Wire signature **unchanged** (Stage
3's introspection output is still accurate). Behavior:

- Decodes the options map with the new `cli::CaptureOptions::from_dbus_options`
  (the decode half of `to_dbus_options`). Deliberately forgiving — a missing or
  wrong-typed key falls back to the same hardcoded default `CaptureConfig`
  uses, because those values were already resolved on the caller's side. The
  one hard error is an unrecognized `kind` (→ `InvalidArgs`).
- Runs the blocking capture on `tokio::task::spawn_blocking`, guarded by
  `Handle::try_current()` (`spawn_blocking` **panics** outside a runtime;
  CLAUDE.md's no-panic rule doesn't exempt "can't happen"). See
  `dbus::run_blocking` — Stage 8 and Stage 16 will want the same wrapper.
- Emits `CaptureTaken(path, kind)` on success via a
  `#[zbus(signal_emitter)] emitter: SignalEmitter<'_>` parameter (the zbus 5
  spelling — it is *not* `signal_context`). Verified live with
  `busctl --user monitor`. A failed emission is logged, not propagated.
- Returns the saved path. Failures come back as `zbus::fdo::Error::Failed`
  with the `CaptureError`/`StorageError` `Display` text.

`options` map gained one key: **`webp-quality`** (`u32`).

---

## Config: one new knob

`webp-quality = 90` (integer `1..=100`, default 90, WebP only — PNG is
lossless). Same per-knob resilience as every other knob: out-of-range,
fractional or wrong-typed warns and defaults. `CaptureConfig` gained
`webp_quality: u8`; `CaptureOptions` gained the same field. Everything else in
the Stage 4 schema is unchanged.

---

## Gotchas Stage 6 (and 7) will hit

1. **`spawn_blocking` panics outside a runtime.** Use `dbus::run_blocking`.
2. **zbus 5 spells it `#[zbus(signal_emitter)]`**, and the parameter type is
   `SignalEmitter<'_>`. It sits after `&self` and before the wire arguments,
   and does not appear in the introspected signature.
3. **`webp::Encoder::from_rgba` *panics*** if the buffer is shorter than
   `width*height*4`, and `Encoder::encode`/`encode_lossless` are
   `.unwrap()`-ing wrappers. `storage::encode_webp` re-checks the length and
   uses `encode_advanced` with a hand-built `WebPConfig` to keep the whole path
   `Result`-shaped. Don't "simplify" it back to `encode()`.
4. **`wl_output`'s `scale` event is an integer** and reports the *ceiling*
   (2 for a 1.5-scale output). The fractional scale and the logical layout
   come from niri's IPC, inside `screencopy.rs`. If the niri socket is
   unreachable the module warns once per call and degrades to the integer
   scale — fullscreen capture doesn't use scale at all, so it never costs a
   screenshot.
5. **Every backend method opens a fresh Wayland connection** and drops it.
   That is intentional (no lock, no poisoned connection); it also means output
   metadata is re-read on every call, so a monitor hot-plugged between calls is
   picked up for free.
6. **`Session::pump` has a 10 s deadline** and uses `prepare_read` +
   `libc::poll` rather than `blocking_dispatch`, which has no timeout and would
   park forever on a wedged compositor.
7. **`take_screenshot` sleeps for `options.delay`** on the calling thread
   (inside `spawn_blocking` in the daemon). Stage 8 replaces that with a real
   countdown surface; don't add a second delay in the meantime.
8. **The nested-niri live-check recipe that actually worked** (reuse it):
   `niri -c /tmp/nested-niri.kdl &` (no `--session`), export the nested
   `NIRI_SOCKET` from its own log line, run with `WAYLAND_DISPLAY=wayland-N`,
   `niri msg output winit scale 1` for byte-exact grim comparison,
   `magick compare -metric AE a.png b.png null:` for the diff, and
   `magick montage` + reading the PNG to *look* at orientation — that last
   step is what actually found the transform bug; the numbers alone only said
   "2.8% different".

---

## The flaky test that failed attempt 1 — don't re-create it

Attempt 1's `cargo test` exited 1 on roughly **1 run in 15** (measured: 3
failures in 40 runs), always the same way:

```
storage::tests::save_capture_writes_encodes_and_indexes
panicked at src/storage.rs:786: the index was created: Os { code: 2, kind: NotFound }
```

Cause: `save_capture_writes_encodes_and_indexes` and
`png_format_with_png_also_does_not_write_a_duplicate` each called
`std::env::set_var("XDG_DATA_HOME", <its own temp dir>)`. `cargo test` runs
tests on **parallel threads of one process**, so whichever test set the
variable last won, the other's `save_capture` appended its index line into the
wrong temp directory, and the reader then found nothing at its own path. The
old doc comment claimed this was safe because `$XDG_DATA_HOME` is "read in
exactly one place in this crate" — that reasoning is wrong; one *reader* is
irrelevant when there are two *writers*.

Fixed by removing process-env mutation from the tests entirely (the
`save_capture_indexing_to` split above), not by serializing them with a mutex
and not with `--test-threads=1`. Two tests were added that the env-based design
could not express at all: the no-data-directory branch
(`a_capture_still_saves_when_there_is_nowhere_to_index_it`) and
`append_history_creates_the_index_directory_on_demand`. 110 → 112 tests.

`append_history` changed shape as part of this: it is now private and takes
the destination, `fn append_history(path: &Path, entry: &HistoryEntry) ->
io::Result<()>`. The on-disk index format is **unchanged**.

**Rule for Stage 6 onward: no `std::env::set_var` in tests.** If a new module
needs `$HOME`/`$XDG_*`, read it in a thin wrapper and give the logic an
argument. (It's `unsafe` in edition 2024 for this reason; this crate is on
2021, so nothing will stop you.)

---

## What changed in CLAUDE.md

Status blurb (Stages 1–5, with the precise real-vs-stub split); Commands
(the hidden `clipboard-serve` verb, `Screenshot` now real); Architecture (two
new "most often re-invented wrong" bullets: the output-transform finding and
grim's fractional-scale resampling); Conventions (the `libc`/`serde_json`
survey outcomes, the `webp-quality` knob in the schema, a new "Saved captures
and the history index" bullet, the `spawn_blocking` note under "One runtime",
and the nested-niri rule now cites the transform bug plus the two nested-niri
gotchas found).

Attempt 2 added one more, under **Conventions → Testing**: the binding
"never `std::env::set_var` in a test" rule, naming the
wrapper-reads-env / logic-takes-arguments shape in both modules that use it
(`storage`, `config`).
