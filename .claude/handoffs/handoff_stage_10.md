# Stage 10 handoff — ScreenCast session + PipeWire frames

Forward-facing context for **Stage 11** (`EncoderSink` + ffmpeg CLI), which
PLAN.md says consumes "the exact negotiated SPA format struct, stride/padding
gotchas, teardown ordering" *verbatim* — those are §1, §3 and §4 below. Also
relevant to Stage 12 (tray/recorder UX, region+window recording) and Stage 13
(audio).

New file: `src/capture/screencast.rs` (~2050 lines incl. 24 tests).
Touched: `Cargo.toml` (pipewire + survey essay), `src/capture/mod.rs` (one
`pub mod` line), `src/cli.rs` (`--dry-run`, `--window-id`, +4 tests),
`src/main.rs` (`run_record_dry_run`, `CliRunError::Cast`),
`src/modules/app.rs` (two new struct fields), `src/dbus.rs` (stub stage
labels only), `CLAUDE.md`, `docs/CAPTURE-RESEARCH.md` (new §2.5).
**Nothing committed** — working tree left for review, as every prior stage.

Gates: `cargo build`, `cargo clippy --all-targets -- -D warnings`,
`cargo fmt --check` clean. `cargo test`: **242 passed** (Stage 9: 216; +26 —
22 in `capture::screencast`, 4 in `cli`).

---

## 0. What is and isn't wired

**Is:** the whole video *capture* chain, plus a CLI diagnostic that drives it.
**Is not:** `StartRecording`/`StopRecording`, which are still stubs
(relabelled "Stage 11"). That is deliberate, not an omission — a
`StartRecording` that captured frames and threw them away would be a worse
lie than one that says it isn't implemented. Nothing in Stage 10 touches the
daemon, the tray, any surface, the clipboard, or the filesystem.

`record start --dry-run [--window-id N]` runs **entirely in the CLI process**
(`main.rs::run_record_dry_run` → `screencast::dry_run`) and never looks the
daemon up. Reasons in `dry_run`'s doc comment; the short version is that it
changes no recording state, its whole product is a log that belongs on the
invoking terminal, and staying out of the daemon is what makes it safe to run
while a real one (or, later, a real recording) is up.

---

## 1. The exact negotiated SPA format — what Stage 11 gets

```rust
pub struct NegotiatedFormat {
    pub width: u32,            // PHYSICAL px (2560 on Jordan's 1.5-scale output)
    pub height: u32,
    pub modifier: u64,         // read back, not assumed; 0 (LINEAR) in practice
    pub framerate: (u32, u32),      // (0, 1) against niri — variable
    pub max_framerate: (u32, u32),  // (60000, 1000)
}
impl NegotiatedFormat {
    pub fn even_dimensions(&self) -> (u32, u32);  // (w & !1, h & !1)
    pub fn packed_len(&self) -> usize;            // w * 4 * h, saturating
}
```

Measured live, 2026-08-09, monitor cast on eDP-1:

```
BGRx 2560x1600 modifier 0x0 framerate 0/1 maxFramerate 60000/1000
```

and on `--window-id 16`:

```
BGRx 2507x1457 modifier 0x0 framerate 0/1 maxFramerate 60000/1000
```

**Straight into ffmpeg's input args** (CAPTURE-RESEARCH §3.7, and what the
dry-run report already prints so the two can't drift):

```
-f rawvideo -pixel_format bgr0 -video_size {even_w}x{even_h} \
-use_wallclock_as_timestamps 1 -i pipe:0 -fps_mode vfr
```

- `bgr0` is exactly `VideoFrame::bytes`' layout: **B G R X, 4 bytes/px, no
  padding**. No swizzle needed at the encoder.
- Use `even_dimensions()`, not `width`/`height` — `hevc_vaapi` silently
  resizes odd inputs (§3.6). The live window cast is the real case: 2507×1457
  → **2506×1456**. Note this means an ffmpeg-side `crop` is still needed (the
  frames are full-size); `even_dimensions` tells you *what to crop to*, it
  does not crop.
- `framerate` is `0/1`. Do **not** feed 0 to `-framerate`; the wallclock
  timestamps are the rate (§4.2/D6). `max_framerate` (60/1) is the only
  meaningful number if a preset needs a cap (the AV1 30 fps cap, §3.4).

**Timestamps: `VideoFrame::captured_at` is NOT a PTS.** D6 pins ffmpeg's
`-use_wallclock_as_timestamps 1`, so the PTS is taken when bytes hit the
encoder's stdin. `captured_at` exists for cadence diagnostics and Stage 12's
"is this recording still alive?" question. Do not mux from it. No
`SPA_META_Header` is requested at all, for the same reason.

---

## 2. New public interfaces, exact signatures

All in `crate::capture::screencast`.

```rust
pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;

pub enum CastTarget { Monitor { connector: String }, Window { id: u64 } }
pub enum CursorMode { Hidden = 0, Embedded = 1 }   // Mutter enum; see §6
impl CursorMode { pub fn from_cursor_option(cursor: bool) -> Self; }

pub enum CastError { Bus(String), ScreenCast { method: &'static str, err: String },
                     NoNodeId, ClosedEarly, PipeWire(String), Thread(String) }

pub struct StreamParameters { pub position: (i32, i32), pub size: (i32, i32) }  // LOGICAL

pub struct CastSession { /* private */ }
impl CastSession {
    pub async fn open(connection: &zbus::Connection, target: &CastTarget,
                      cursor: CursorMode) -> Result<Self, CastError>;
    pub fn node_id(&self) -> u32;
    pub fn parameters(&self) -> Option<StreamParameters>;
    pub fn target(&self) -> &CastTarget;
    pub fn closed(&mut self) -> bool;      // non-blocking poll of Session.Closed
    pub async fn close(self);              // Session.Stop, unless already closed
}

pub struct NegotiatedFormat { /* §1 */ }
pub struct VideoFrame { pub width: u32, pub height: u32, pub bytes: Vec<u8>,
                        pub sequence: u64, pub captured_at: std::time::Instant }
pub enum CastControl { Negotiated(NegotiatedFormat), Error(String), Ended }

pub struct PipeWireStream { /* private */ }
impl PipeWireStream {
    pub fn connect(node_id: u32) -> Result<Self, CastError>;   // returns immediately
    pub fn control(&self) -> &std::sync::mpsc::Receiver<CastControl>;
    pub fn frames(&self) -> &std::sync::mpsc::Receiver<VideoFrame>;
    pub fn dropped_frames(&self) -> u64;
    pub fn stop(self);                     // == Drop; idempotent
}

pub struct CastSummary { pub stream_id: u64, pub session_id: u64,
                         pub target: String, pub is_active: bool }
pub fn casts_snapshot() -> Option<Vec<niri_ipc::Cast>>;
pub fn cast_for_node(casts: &[niri_ipc::Cast], node_id: u32) -> Option<&niri_ipc::Cast>;
pub fn summarize_cast(cast: &niri_ipc::Cast) -> CastSummary;

pub struct DryRunReport { /* target, node_id, format, parameters, frames, dropped,
                            first_frame_after, gaps, observed_for, niri_cast, error */ }
impl DryRunReport { pub fn average_fps(&self) -> Option<f64>; }   // + Display
pub async fn dry_run(target: CastTarget, cursor: CursorMode,
                     duration: Duration) -> Result<DryRunReport, CastError>;
```

`cli.rs`: `RecordStartArgs` gains `dry_run: bool` and `window_id: Option<u64>`;
`RecordOptions` gains the same two. **Neither goes into
`RecordOptions::to_dbus_options`** — the `io.saola.Capture1` surface is
unchanged by this stage (asserted by a test). `--window-id` without
`--dry-run` is a *rejection*, not a silent ignore, with a message naming
Stage 12.

`main.rs`: `CliRunError::Cast(capture::screencast::CastError)`,
`const DRY_RUN_DURATION: Duration = 5s`, `fn run_record_dry_run(&CaptureConfig,
&cli::RecordOptions) -> Result<String, CliRunError>`.

---

## 3. Stride, padding and buffer gotchas (verbatim for Stage 11)

**Stage 11 should need none of this** — `VideoFrame::bytes` is already packed
at `width * 4` per row, which is exactly what `-f rawvideo` wants. It is
recorded so that nobody "optimises" the copy away without knowing what it
costs.

1. **`chunk->stride` is the row pitch, and it is not `width * 4`.** Live:
   negotiated 2507×1457, stride **10240** (= 2560 × 4, the *output* pitch).
   Deriving 10028 from the width would offset every row by 53 px and shear
   the frame. `copy_packed_rows` is the single place this is applied
   (unit-tested), and a temporarily instrumented build dumped a real
   2507×1457 frame to PNG to confirm visually that it is unsheared with
   correct colour.
2. **`maxsize` and `chunk->size` are dummies** on the dmabuf path (both `1`).
   Mapping length comes from `lseek(fd, 0, SEEK_END)`.
3. **The fd is bigger than `stride × height`** (14 921 728 vs 14 919 680 in
   the C probe) — allocator padding past the last row.
   `required_mapping_len(map_offset, stride, w, h)` is
   `map_offset + (h-1)*stride + w*4`, *not* `map_offset + stride*h`: the last
   row needs only its own bytes, and demanding a full stride past it would
   reject a tight allocation. Checked before every copy; a short mapping is a
   `CastControl::Error`, never an out-of-bounds read.
4. **`PW_STREAM_FLAG_MAP_BUFFERS` does not map dmabufs** (`data == null`
   always). The flag is still set — harmless, matches the probe — and the
   `mmap` is done by hand.
5. **`spa::buffer::Data::chunk()` panics** (`assert_ne!(chunk, null)`). The
   null check is done via `data.as_raw().chunk.is_null()` first, because the
   no-panic rule has no "cannot happen" exemption.
6. **Mappings are cached by fd** (`HashMap<RawFd, FrameMapping>`), released in
   `remove_buffer` and cleared on any `param_changed`, so a 16 MB
   `mmap`/`munmap` pair isn't paid per frame at 60 Hz. `FrameMapping` unmaps
   on drop; the listener owns the map, and the thread drops the listener
   explicitly before the stream/core go away.
7. **`DMA_BUF_IOCTL_SYNC` brackets every read** (`START|READ` … `END|READ`).
   The ioctl number is `0x4008_6200`, derived in a comment; `libc::Ioctl` is
   `c_ulong` on glibc and `c_int` on musl, hence the cast.
8. **Non-dmabuf buffers are a hard error**, not a fallback. There is no shm
   path and there must not be one (§2.1: refused at both the format and the
   allocation layer).

---

## 4. Teardown ordering (verbatim for Stage 11)

**Normal stop — consumer first, producer second:**

```
PipeWireStream::stop()      // send LoopCommand::Stop over pipewire::channel
                            //   → pw_main_loop_quit (runs ON the loop thread)
                            //   → pw_stream_disconnect
                            //   → drop listener (unmaps every dmabuf)
                            //   → send CastControl::Ended
                            //   → join the thread
CastSession::close().await  // Session.Stop
```

Reversing it makes the node vanish under a live consumer and logs a
`StreamState::Error` for what was a clean, user-requested stop.

**Never `Stop` a closed session** (D8). `CastSession` retains its
`Session.Closed` signal stream for its whole life; `closed()` polls it
non-blockingly (`StreamExt::next().now_or_never()`, latching) and `close()`
consults it first. Stage 11/12's recorder state machine should call `closed()`
whenever it wants to know whether the compositor pulled the rug out.

**Subscribe before `Start`.** `PipeWireStreamAdded` fires ~immediately after
`Start`; `Session.Closed` is subscribed even earlier. `open()` ends in a
three-way `tokio::select!` — node id / `Closed` / 5 s timeout — producing
three *different* errors (`ClosedEarly` deliberately does **not** call `Stop`).

**Stream error:** the pw thread sends `CastControl::Error`, quits its own
loop, sends `Ended`. The owner still calls `stop()` (idempotent) then
`close()`. `PipeWireStream`'s `Drop` runs the same shutdown, so a `?` in a
caller cannot leak the thread.

---

## 5. Threading and the channel contract (Stage 11 plugs in here)

One extra OS thread — the one CLAUDE.md sanctions. `MainLoopRc`, `ContextRc`,
`CoreRc`, `StreamBox` are `Rc`-based and **not `Send`**; all are created,
used and destroyed inside `stream_thread` and never escape.

Crossing outward, both `std::sync::mpsc`:

| channel | kind | rule |
| --- | --- | --- |
| `control` | unbounded `channel()` | **never dropped** — `Negotiated`, `Error`, `Ended` |
| `frames` | `sync_channel(4)` | `try_send` only; `Full` ⇒ drop + `AtomicU64` count + throttled log |

Crossing inward: `pipewire::channel::Sender<LoopCommand>`, whose receiver is
attached to the pw loop as an event source. **This is the only correct way to
poke a running pw loop from another thread** — calling `quit()` from outside
would race a non-`Sync` object.

Why `std::sync::mpsc` and not the `iced::futures::channel::mpsc` the daemon
uses: the pw thread is a C event loop calling synchronous callbacks (no
executor, no `Waker`), and Stage 11's consumer is a blocking write into
ffmpeg's stdin, so both ends of a futures channel's reason to exist are
unused. Zero new crates either way. **The two-channel split is load-bearing**:
merging them would let "the queue was briefly full" swallow the negotiated
format.

`FRAME_QUEUE_CAPACITY = 4` ≈ 66 ms of slack at 60 fps, and bounds memory at
`4 × w × h × 4` ≈ **64 MB** at 2560×1600. Raising it trades latency and memory
for stall tolerance; it does not eliminate drops.

**Observed backpressure working:** during the instrumented run, a PNG encode
in the consumer stalled it enough to report `dropped 1` on a 41-frame window,
with no block on the producer. That is the rule behaving exactly as PLAN.md
specifies.

---

## 6. Design decisions worth knowing before you change something

- **`CursorMode` has only `Hidden`/`Embedded`.** Mutter's wire enum has a
  third value (`2 = metadata`) that delivers the pointer as metadata; honouring
  it would mean drawing the cursor ourselves, and the `cursor` knob is a
  boolean, so there is no way to ask for it and no code that could serve it. A
  variant nothing can construct would be a trap (and would trip `-D warnings`).
  The doc comment records the full wire enum so nobody re-derives it wrong.
  **It is the Mutter enum, not the portal bitmask** — a probe rejected 3 and 4.
- **SPA pods are built from the safe `Value`/`Object`/`Property` tree +
  `PodSerializer`**, not the `unsafe` `spa_pod_builder`. Small pods, built once
  per stream; readability wins over the allocation. Two flag namespaces are
  easy to conflate and are not the same: `PropertyFlags`
  (`MANDATORY|DONT_FIXATE`) sit on the *property*; `ChoiceFlags` sit on the
  *choice* and are empty (the C probe passed 0).
- **The cargo feature gate `v0_3_33` is a floor, not a version stamp.** It is
  the minimum providing `PropertyFlags::DONT_FIXATE`
  (`#[cfg(feature = "v0_3_33")]` in libspa). `v1_2_0` would build on this
  machine but would raise the required system libpipewire everywhere for API
  this crate never calls. A test asserts both flags exist, so a gate change
  breaks a test instead of a negotiation. **Bump it only when a stage needs a
  specific newer item, and say which.**
- **`libspa` is not a direct dependency** — reached as `pipewire::spa`, raw
  constants as `pipewire::spa::sys::*`. One line, no possible version skew.
- **`Stream.Parameters` is logical, the SPA format is physical, and Parameters
  is meaningless for window casts** (live: `size (1, 1)` for a window,
  `(1706, 1066)` for the monitor). Stage 12's region/window crop math must take
  window geometry from niri-ipc, not from this property.
- **`CastsChanged` awareness is pull, not push.** `casts_snapshot()` +
  `cast_for_node()` + `summarize_cast()`, used by the dry run to cross-check
  the handshake; `is_active` there means "a consumer is attached" (D11), so
  watching it flip to `true` proves *our* consumer reached the compositor. A
  push subscription (`Event::CastsChanged { casts }` /
  `Event::CastStopped { stream_id }` over `Request::EventStream`) is
  deliberately not built: niri's event socket is blocking, so it needs its own
  long-lived thread, and it would add no authority — `Session.Closed` is what
  decides whether `Stop` may be called. **Stage 12 is where a push
  subscription earns a thread**, and those two variants are what it wants.
- **`record start --dry-run --window-id N`** exists so the `RecordWindow`
  failure path is reachable by a diagnostic. It is dry-run-only; the flag is
  rejected (not ignored) otherwise.

---

## 7. Verified vs. not

**Verified headless:** build / clippy `-D warnings` / fmt / 242 tests. The 22
new `screencast` tests are all pure: `required_mapping_len` (last-row-not-full-
stride, map offset, zero height, overflow), `copy_packed_rows` (padding
dropped, offset applied), `even_dimensions` (the real 2507×1457 case),
`packed_len`, both pods serialize into valid `Pod::from_bytes` round-trips,
the buffers pod's dataType mask is exactly `1 << SPA_DATA_DmaBuf` and excludes
MemFd/MemPtr, `DONT_FIXATE` exists under the chosen feature gate, the Mutter
cursor enum, `cast_for_node` (incl. a cast with no node yet), `summarize_cast`,
`average_fps`, two `DryRunReport` `Display` shapes, and that every `CastError`
names something to try.

**Verified live, real session** (2026-08-09; nested niri is impossible here —
see below). Nothing written, no surface, no keyboard, no injected input,
daemon PID 187728 untouched throughout:

| run | result |
| --- | --- |
| `record start --dry-run` ×4 | `BGRx 2560x1600 modifier 0x0 framerate 0/1 maxFramerate 60000/1000`; logical `1706x1066 at 0,0`; 41–62 frames / 5 s (8.2–12.4 fps, idle desktop); first frame 123–286 ms; `niri sees: … consumer attached: true`; exit 0 |
| `--dry-run --window-id 16` | `BGRx 2507x1457`, stride 10240; **5 frames in 5 s**, ~1 s gaps (cursor blink — §2.3 gotcha 4 in the wild); `logical geometry: 1x1 at 0,0` |
| `--dry-run --window-id 999999` | `the compositor closed the screencast session immediately …`, exit 1, sub-second, driven by `Session.Closed`; **no `Stop` sent**; no leaked cast |
| instrumented frame dump (reverted) | both a 2560×1600 monitor frame and a 2507×1457 window frame decoded to PNG: **pixel-perfect, unsheared, correct BGRx→RGB colour, cursor embedded** |
| teardown, after every run | `niri msg casts` → "No screencasts"; `pw-dump` shows no leftover `Video/Source` node beyond the two webcams; no leaked process |
| nested niri + private bus | `busctl --user list` shows **no** `org.gnome.Mutter.*` at all; dry run fails with `ServiceUnknown` and exit 1 — which also verified the absent-service degradation path (clean, actionable, no panic, no hang) |
| CLI surface | `--window-id` without `--dry-run` → clean error, exit 1; `record stop --dry-run` → clap rejects; plain `record start` → unchanged Stage-11 stub from the live daemon |

**NOT verified:**

- **▶ THE HUMAN-VERIFY ITEM PLAN.md ASSIGNS TO JORDAN IS STILL OPEN.** PLAN.md
  Stage 10 task 4 says "dry-run in the real session shows **steady frames at
  the negotiated size**". The size half is verified (2560×1600, four runs).
  The *steady frames* half is only verified for an **idle** desktop (8–12 fps,
  gaps 61–203 ms). There is no safe way for an agent to generate sustained
  screen damage without injecting input into the live session, so **a run with
  something actually moving — scroll a page, play a video — is Jordan's to
  do.** Expect the cadence to climb toward the 60 fps cap and the gaps toward
  ~16 ms. A *low* number on an idle screen is correct behaviour, not a fault
  (§2.3 gotcha 4).
- Multi-output: single-output machine, same gap D10 already records.
- A non-`Normal` output transform: the cast frames came out right-way-up on
  Jordan's `Normal` eDP-1, so whether the cast path needs screencopy's
  `undo_output_transform` equivalent is **untested**. Nested niri (whose winit
  output is `Flipped180` and which caught this for Stage 5) cannot serve
  ScreenCast, so this cannot be tested here at all today.
- PipeWire absent/refusing (`CastError::PipeWire` from `connect`), and a
  mid-stream `StreamState::Error` — both are code paths with no safe way to
  provoke them on a healthy machine.
- Long runs: the longest observation was 5 s. The fd-keyed mapping cache and
  the drop counter are unexercised over minutes, which is exactly what Stage
  11's first real recording will do.
- `>4` concurrent casts, or a second `dry_run` while one is live.

---

## 8. For Jordan

1. **Please run `saola-capture record start --dry-run` once with something
   moving on screen** (scroll a long page, play a video) and confirm the
   cadence climbs toward 60 fps at `2560x1600`. That is PLAN.md's own
   human-verify item and the one thing this stage could not do for itself. It
   writes nothing, saves nothing and does not touch your running daemon.
2. `record start --dry-run --window-id <id from niri msg windows>` casts one
   window instead. Expect very few frames from an idle window — that is
   correct (damage-driven).
3. Nothing needs installing. `clang` (already there) covered the bindgen
   build; `pipewire` links your system PipeWire, which is already running.
4. **Packaging note for Stage 17:** `pipewire` becomes a real PKGBUILD
   `depends` entry (not just `makedepends`), alongside `clang` in
   `makedepends` and `libclang-dev` in CI.
