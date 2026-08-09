# Stage 11 handoff — EncoderSink + ffmpeg CLI: recording end-to-end

Forward-facing context for **Stage 12** (tray item, region/window recording,
the app window's Record tab driving real recordings) and **Stage 13** (audio).

New files: `src/encode/mod.rs` (1186 lines incl. 28 tests),
`src/encode/ffmpeg_cli.rs` (685 incl. 3), `src/modules/recorder.rs` (1007
incl. 20). Touched: `src/dbus.rs` (+~760, incl. a new `mod tests` — 5),
`src/main.rs`, `src/cli.rs`, `src/config.rs`, `src/storage.rs`,
`src/modules/toast.rs`, `src/modules/app.rs`, `src/modules/mod.rs`,
`CLAUDE.md`. **Zero new dependencies** — `std::process` plus one
`libc::kill`. **Nothing committed**, as every prior stage.

Gates: `cargo build`, `cargo clippy --all-targets -- -D warnings`,
`cargo fmt --check` clean. `cargo test`: **304 passed** (Stage 10: 242).

One post-verification fix rides along in the tree (orchestrator-applied,
not part of the stage's own work): the Stage 8 test `read_with_retry_
survives_a_file_that_appears_a_moment_later` had a latent ~1-in-5 flake —
its writer thread used a bare `fs::write`, whose create-then-write window
the polling reader could catch as an empty file. Now write-`.part`-then-
rename (atomic, the same idiom `storage.rs` uses). Verified: 10× isolated
+ 5× full-suite runs, zero failures.

> **▶ FOR JORDAN — the human-verify item PLAN.md assigns to you:**
> `/home/jordan/Pictures/Captures/Recording_2026-08-09_09-12-24.mkv`
> (1.2 MB, 10.12 s, HEVC 2560×1600). Play it and confirm it looks like your
> screen at the right speed. **`mpv` is not installed on this machine** —
> `sudo pacman -S mpv`, or use any HEVC-capable player. Nothing else needs
> installing; ffmpeg n9.0 is already there.

---

## 0. What is and isn't wired

**Is:** `record start|stop|toggle` for the **focused output**, end to end,
through the daemon — negotiate → first frame → allocate a path → spawn
ffmpeg → pump → flush → report. All three presets. Failure paths surfaced as
a D-Bus `Error` signal *and* a toast.

**Is not:** region/window *recording* targets (Stage 12 — the D-Bus method
rejects any `kind` but `"fullscreen"` rather than silently recording the
whole monitor), audio (Stage 13 — `--audio` is **refused**, not ignored), the
tray item, the elapsed-time chip, a success toast, and
`PropertiesChanged` on the new `Recording` property.

`PickColor` is now the only remaining stub in `dbus.rs`.

---

## 1. The `EncoderSink` trait, as landed

```rust
pub trait EncoderSink: Send {
    fn write_video(&mut self, bytes: &[u8]) -> Result<(), EncodeError>;
    fn poll_health(&mut self) -> Result<(), EncodeError>;   // cheap waitpid(WNOHANG)
    fn finish(self: Box<Self>) -> Result<PathBuf, EncodeError>;
    fn abort(self: Box<Self>);                              // infallible
    fn output_path(&self) -> &Path;
}
```

Three shape decisions Stage 12/13 should not try to "fix" (each argued at
`encode/mod.rs`'s head):

- **No `pts` argument**, though Architecture sketches one. D6 pins
  `-use_wallclock_as_timestamps 1`, so ffmpeg stamps a frame when its bytes
  arrive; a `pts` would be a parameter every implementation must ignore.
- **No `write_audio`**, though PLAN.md sketches one. D7 settled audio as an
  `-f pulse` input ffmpeg opens *itself* — no PCM crosses this trait. §4.4's
  escalation (PCM on `pipe:3`, taken only if the clap test shows a
  *non-constant* offset) is the stage that should add it, with a real body.
- **`start` is an inherent constructor, not a trait method** — a constructor
  cannot be dispatched through `dyn`. `FfmpegSink::start(&RecordSpec,
  EncoderChoice)`.

`finish`/`abort` take `self: Box<Self>` so "finished" is a type-level fact.

Supporting types, all in `crate::encode`:

```rust
pub enum EncodePreset { Hevc, Av1, H264 }      // .extension() .muxer() .as_str()
pub enum VideoEncoder { HevcVaapi, H264Vaapi, Av1Vaapi, LibSvtAv1 }
pub struct EncoderChoice { pub encoder: VideoEncoder, pub vaapi_device: Option<PathBuf> }
pub struct VideoSpec { pub width: u32, pub height: u32 }   // .frame_len() .even_dimensions()
pub struct NegotiatedGuard(VideoSpec);                     // .check(&NegotiatedFormat)
pub struct AudioSpec { pub source: String, pub itsoffset: Option<f64> }   // Stage 13, never built yet
pub struct RecordSpec { video, audio, preset, path }
pub enum EncodeError { FfmpegMissing, Spawn, NoVaapiDevice, Died, Write, EmptyOutput }
pub fn select_encoder(preset, &[PathBuf], impl FnMut(&Path, VideoEncoder) -> bool)
        -> Result<EncoderChoice, EncodeError>;
pub fn filter_chain(VideoEncoder, &VideoSpec) -> String;
pub fn ffmpeg_args(&RecordSpec, &EncoderChoice, loglevel: &str) -> Vec<String>;
```

`ffmpeg_cli`: `ensure_ffmpeg_available()`, `render_nodes() -> Vec<PathBuf>`,
`choose_encoder(preset, Option<&Path>) -> Result<EncoderChoice, _>`,
`FfmpegSink`.

---

## 2. The preset argument tables, as emitted

Common input (`W`/`H` = the **negotiated** size, not the even one):

```
-hide_banner -nostats -loglevel {warning} -y
-f rawvideo -pixel_format bgr0 -video_size {W}x{H} -use_wallclock_as_timestamps 1 -i pipe:0
[Stage 13: -itsoffset {sec} -f pulse -i {source}]
[-vaapi_device {node}]  -fps_mode:v vfr  -vf {chain}  -c:v {encoder} {codec args}
[Stage 13: -c:a libopus -b:a 96k -shortest]  [-movflags +faststart]  -f {muxer} {path}
```

| preset | encoder | container | `-vf` chain | codec args |
| --- | --- | --- | --- | --- |
| `hevc` | `hevc_vaapi` | matroska | `crop=W&~1:H&~1:0:0,hwupload,scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=tv` | `-b:v 20M` |
| `h264` | `h264_vaapi` | mp4 (+faststart) | same | `-b:v 20M` |
| `av1` (hw, hypothetical) | `av1_vaapi` | matroska | same | `-b:v 20M` |
| `av1` (software, reality) | `libsvtav1` | matroska | `crop=…,fps=30,format=yuv420p` | `-preset 10 -crf 35 -g 120` |

Four refinements over §3.7's literal table, each with a reason:

1. **`-fps_mode:v`, stream-qualified.** The bare form would also hit Stage
   13's audio stream, where it means nothing. Identical for video-only.
2. **`-video_size` is the negotiated size; the even size is a `crop`
   filter.** Swapping them shears the video progressively instead of
   failing — hence two separate concepts (`VideoSpec::even_dimensions`).
   `crop` is pointer-adjusting, so an already-even frame pays ~nothing.
3. **`-y` is mandatory.** ffmpeg's stdin *is* the video pipe, so a `-n`
   overwrite prompt could never be answered. The path was just allocated by
   `storage`, so a collision is one this process created.
4. **The AV1 30 fps cap is an `fps=30` filter**, per D6's "cap or document
   the drops".

`-loglevel` defaults to `warning`; `SAOLA_CAPTURE_FFMPEG_LOGLEVEL=verbose`
on the **daemon** is what prints `Using VAAPI entrypoint
VAEntrypointEncSlice (6).` — the only proof a recording was hardware-encoded.

---

## 3. The device probe: behaviour, and what it chose live

`select_encoder` is pure and takes an **oracle closure**, not a capability
table, because every `supports` call is a fork+exec (~150–180 ms). Rules:

- **`av1` preset** → first node that opens `av1_vaapi`; else software
  `libsvtav1`, which needs no device and therefore **cannot fail**. This is
  why "no usable VAAPI node" is only fatal for the other two presets.
- **`hevc`/`h264`** → pass 1: a node that opens *both* `av1_vaapi` **and**
  the preset's encoder (so a future AV1-capable dGPU wins the tie rather than
  whichever node enumerated first); pass 2: any node that opens the preset's
  encoder; else `EncodeError::NoVaapiDevice`, which names `--preset av1` and
  the `vaapi-device` knob.

`render_nodes()` sorts, so ties are deterministic and `renderD128` (the iGPU
driving the panel — its encoder reads the frames without a cross-device copy)
wins. `probe_encoder` caches per `(device, encoder)` for the process's life.

**Measured live 2026-08-09**, Jordan's machine, **one render node only**
(`/dev/dri/renderD128`; the dGPU was absent that day — the node count is a
runtime fact, which is the whole point of D6's amendment):

| probe | result |
| --- | --- |
| `av1_vaapi` on renderD128 | **no** (179 ms) |
| `hevc_vaapi` on renderD128 | **yes** (150 ms) |
| `h264_vaapi` on renderD128 | **yes** (125 ms) |
| `hevc` preset resolution, cold daemon | `hevc_vaapi on /dev/dri/renderD128` (330 ms) |

So `av1` resolves to software `libsvtav1`, exactly as §3.3 predicted — and
now confirmed by a *trial encode*, not by `vainfo` (which advertises
`VAProfileAV1Profile0` and is decode-only; the probe answers the better
question of what this ffmpeg build can actually open).

**`PROBE_SIZE = 256`, and it must not shrink.** The first draft probed at
64×64 and *every* codec on *every* node "failed" with `Hardware does not
support encoding at size 64x64 (constraints: width 130-8192 height
128-4352)` — a size rejection indistinguishable from a missing entrypoint if
you only check the exit status. A test guards the constant.

`capture.toml`'s new `vaapi-device` knob: a node that **exists** is used on
its own (a real override, not a reordering); one that doesn't warns and falls
back to full discovery. `config.rs` deliberately does not validate it —
"is this a usable render node" is a hardware question.

---

## 4. The recorder: state machine + pump

```rust
pub enum Phase { Idle, Starting, Recording, Stopping }
pub struct RecorderState<H> { … }     // generic over the parked handle, so tests use u32
impl<H> RecorderState<H> {
    fn phase(&self) -> Phase;
    fn is_active(&self) -> bool;             // Stopping counts as active — see below
    fn elapsed(&self, now: Instant) -> Option<Duration>;   // from the START REQUEST
    fn last_error(&self) -> Option<&str>;
    fn active_mut(&mut self) -> Option<&mut H>;
    fn begin_start(&mut self, now) -> Result<(), RecorderError>;   // the ONE concurrency guard
    fn started(&mut self, handle: H) -> StartOutcome;   // Recording | StopImmediately | Cancelled
    fn start_failed(&mut self, why);
    fn request_stop(&mut self) -> Result<StopOutcome, RecorderError>; // Stopping | QueuedDuringStart
    fn encoder_died(&mut self, why);         // → Stopping, NOT Idle
    fn finished(&mut self, Result<(), String>) -> Option<H>;
}
pub fn pump_frames(&Receiver<VideoFrame>, &Receiver<CastControl>, &mut dyn EncoderSink,
                   &AtomicBool /*stop*/, &AtomicU64 /*written*/, &NegotiatedGuard) -> PumpOutcome;
pub enum PumpOutcome { StopRequested, StreamEnded, StreamError(String), EncoderFailed(String) }
```

Rules worth carrying:

- **`Stopping` counts as active.** A toggle pressed during a stop must not
  start a *second* recording on top of one still finalizing; answering `true`
  routes it to `StopRecording`, which refuses cleanly.
- **`encoder_died` goes to `Stopping`, not `Idle`** — the teardown still has
  to happen and still takes time, and a `StartRecording` arriving during it
  must be refused, not raced.
- **`elapsed` runs from the start *request*** — what a human means by "how
  long have I been recording". Stage 12's tray tooltip wants exactly this.
- **The pump does nothing between `recv` and `write_all`.** Every
  millisecond there is PTS error, because the wallclock stamps a frame when
  its bytes reach ffmpeg.
- **`POLL_INTERVAL = 250 ms` only bounds how long a stop waits**, never how
  long a quiet period may last. D8: window casts are damage-driven and go
  seconds between frames (Stage 10 measured 5 frames in 5 s). Silence is
  never failure. `HEALTH_INTERVAL = 1 s` is what notices a dead encoder
  during such a quiet stretch.

Backpressure is enforced **upstream**, in `capture::screencast`'s
`sync_channel(4)` + `try_send`, and must stay there. `recorder.rs`'s
`a_full_frame_queue_drops_instead_of_blocking_the_producer` reproduces that
exact channel shape against a deliberately slow sink and asserts no single
offer blocks.

**Measured drop behaviour, live:** *zero* dropped frames in every run,
including a 22.6 s one. Cadence on an idle desktop was 10.6–14.9 fps against
a queue of 4 and an encoder running at ~1× realtime, so the queue never
filled. The drop path therefore remains **exercised only by tests** — a busy
screen (video playback, fast scrolling) at 60 fps is the case that would
exercise it, and Stage 12 is the first stage likely to produce one.

---

## 5. The D-Bus surface as it now stands

| member | shape | status |
| --- | --- | --- |
| `StartRecording(kind s, options a{sv})` | `→ ()` | **real**, `kind == "fullscreen"` only |
| `StopRecording()` | `→ s` (the saved path) | **real** |
| `Recording` | property `b`, read-only | **new in Stage 11** |
| `RecordingStarted(kind s)` | signal | emitted after handover |
| `RecordingFinished(path s)` | signal | emitted by the supervisor on a clean end |
| `Error(message s)` | signal | emitted by the supervisor on any unclean end |

`options` keys (`cli::RecordOptions::to_dbus_options` ⇄ `from_dbus_options`):
`preset`, `audio`, `cursor`, `output` (= `save-dir`), `vaapi-device`.
`dry_run`/`window_id` deliberately never travel. The **CLI process** reads
`capture.toml` (so `--config-dir` works for `record` exactly as for `shot`)
and the daemon loads no config of its own.

**Why the property was added.** `record toggle` cannot be implemented
correctly without it: the alternatives were "always call `StartRecording` and
let it fail" (which loses the saved path on the stop half) or "guess from a
`StopRecording` error string" (a parser for prose). It is additive — the
frozen part of `io.saola.Capture1` is its signals and the five methods — and
**nothing emits `PropertiesChanged` yet**. Stage 12's tray item is the first
consumer that will want that, and adding it is one `emitter` call in the two
places the phase changes.

**Timing contract:** `StartRecording` returns when the recording is *live*
(≈330 ms measured, cold; most of it the device probe). `StopRecording`
**blocks** until ffmpeg has flushed (≈216 ms measured for a 10 s Matroska;
an MP4 is slower because `+faststart` rewrites the file, hence
`FINISH_WAIT = 30 s` before the SIGINT escalation).

### The start sequence and its teardown order

`CaptureService::spin_up`: ffmpeg-present → focused output (through the same
`CaptureBackend` a screenshot uses) → `CastSession::open` →
`PipeWireStream::connect` → **await the negotiated format *and* the first
frame** (`FIRST_FRAME_TIMEOUT = 5 s`) → allocate the path → `choose_encoder`
→ `FfmpegSink::start` → **write that first frame immediately**.

Waiting for the first frame is forced twice over: `-video_size` needs the
negotiated size, and D7 mitigation 1 says spawn ffmpeg only after the first
video frame so neither input idles at the start. Stage 13 gets that for free.

**Teardown is consumer → encoder → producer**, extending Stage 10's rule:
`PipeWireStream::stop()`, then `sink.finish()`, then
`CastSession::close().await`. The first two are blocking and run on the
pump's `spawn_blocking` task; the third is a D-Bus call on the async
supervisor that awaits it — which is why a recording is **two tasks**.

**There is exactly one finalization site** (the supervisor), because a
recording can end with nobody asking. A waiting `StopRecording` parks on a
oneshot registered *under the same lock that sets the stop flag*, so the
supervisor cannot finish in between and conclude nobody was listening.

---

## 6. The A/V timing model, prepared for Stage 13

Everything the audio stage needs is already shaped, emitted and tested; what
is missing is *resolution*, not design.

- **Already true:** ffmpeg spawns after the first video frame (mitigation 1).
  `ffmpeg_args` already emits `-itsoffset {x} -f pulse -i {source}` before
  the output options and `-c:a libopus -b:a 96k -shortest` after — with a
  test (`an_audio_input_lands_in_the_order_ffmpeg_expects`) asserting both
  inputs precede any output option, so the ordering cannot rot.
- **`-shortest` is mitigation 2's first half.** `-f pulse` never EOFs, so
  closing stdin will *not* end an ffmpeg with a live audio input — the second
  half is `FfmpegSink::finish`'s SIGINT escalation, which already exists and
  is why `libc::kill` is in the file at all (`Child` only offers SIGKILL,
  which for ffmpeg means an unfinalized file). Writing `q` to stdin, the
  other documented graceful stop, is impossible here: stdin *is* the video
  pipe.
- **Mitigation 3 is `AudioSpec::itsoffset`** — measure with the clap test and
  bake it in. §4.3 measured that both ffmpeg's default normalisation and
  `-copyts -start_at_zero` discard the real offset; `-itsoffset` survives.
- **What Stage 13 must add:** resolve the source name from `pactl list short
  sources` at record time (never hardcode — they are hardware-path-derived),
  build the `AudioSpec`, and **delete the `options.audio.is_some()` refusal**
  at the top of `start_recording`.
- **`VideoFrame::captured_at` is still not a PTS.** Do not mux from it.

---

## 7. Verified live (real session, 2026-08-09)

No daemon owned `io.saola.Capture1` at the time (checked first), so the test
daemon ran on the **real session bus** with `XDG_DATA_HOME` and `save-dir`
pointed at a temp dir. A private `dbus-daemon` bus is **not** an option for
this path: niri serves `org.gnome.Mutter.ScreenCast` on the session bus and
the daemon reaches it over the same `Connection`, so isolating the bus
isolates the cast away too.

| run | result |
| --- | --- |
| **10 s fullscreen, `hevc`** (the keeper) | `Recording_2026-08-09_09-12-24.mkv`, 1 210 276 B, **hevc / matroska / 2560×1600 / yuv420p / bt709 / tv**, duration **10.12 s** for **10.01 s** of wall clock, 107 frames (10.6 fps, idle desktop), first PTS 0.000 last 10.080 monotonic, `ffmpeg -f null -` decodes clean, log shows `VAEntrypointEncSlice (6)` |
| 22.6 s fullscreen, `hevc` | 336 frames, 14.9 fps, 4.4 MB, `time=00:00:22.56` vs 23.3 s of daemon-measured wall clock, **0 dropped** |
| `--preset av1` 5 s | `libsvtav1`, MKV, 5.066 s, decodes clean |
| `--preset h264` 5 s | `h264_vaapi` (`VAEntrypointEncSlice`), **MP4** with `+faststart`, 5.080 s, bt709/tv, decodes clean |
| `record toggle` ×2 | started, then stopped and printed the path |
| second `record start` while recording | `a recording is already in progress — stop it first (saola-capture record stop)`, exit 1 |
| `record stop` while idle | `nothing is recording`, exit 1 |
| **stop-while-starting** | stop → `the recording had not finished starting — it has been cancelled, and nothing was saved`; the start call → `the recording was stopped while it was still starting — nothing was saved`; **no file, no leaked cast, state back to Idle** |
| **encoder death** (`SIGKILL` the ffmpeg child mid-recording) | detected in <1 s, `EncoderFailed`, `Error` signal + toast, state → Idle, a later `record stop` still explains *why* there is nothing recording; the next recording started normally |
| **missing ffmpeg** (daemon with a `PATH` without it) | `ffmpeg is not installed (or not on $PATH) — recording needs it: sudo pacman -S ffmpeg`, exit 1, **and no screencast session was ever opened** — the check really is up front |
| non-`fullscreen` kind (raw `busctl`) | `recording kind "window" is not supported yet — only "fullscreen" works today (region and window recording land in Stage 12)` |
| `--audio mic` | `NotSupported: audio recording lands in Stage 13 — start the recording without --audio` |
| `Recording` property | `b false` idle, and `false` again after every teardown |
| teardown, after every run | `niri msg casts` → "No screencasts"; `pw-dump` shows only the two webcams; `pgrep -x ffmpeg` empty; isolated `XDG_DATA_HOME` contains **no `history.jsonl` at all** (recordings are deliberately not indexed) |

### Two defects this stage found in its own first draft, and fixed

1. **A SIGKILLed ffmpeg quoted an irrelevant line as its cause.**
   `EncodeError::Died`'s `Display` appended `tail.last()` unconditionally,
   producing `ffmpeg was killed by a signal: [out#0/matroska @ 0x…] Starting
   thread...`. ffmpeg prints its reason and *then* exits, so on a coded exit
   the last line really is the cause — on a signal death there is no
   ffmpeg-authored explanation at all. The tail is now quoted only for a
   coded exit; a signal says what it means for the file instead. Test:
   `a_signal_death_does_not_quote_whatever_line_was_last`.
2. **The failure message was said twice.** `sink.finish()` usually fails for
   the same reason the pump did (the child is already gone), and
   `recording_result` concatenated the two — doubling the sentence in both
   the toast and the `Error` signal. It now appends only genuinely new
   information. Tests: the new `dbus::tests` module (5 tests; `dbus.rs` had
   none before).

Also removed: a block in `dbus::serve` marked `TEMPORARY STAGE 11 LIVE-TEST
SCAFFOLDING — REVERT BEFORE HANDOFF`, which let a daemon serve without owning
the well-known name under `SAOLA_CAPTURE_TEST_SHARE_BUS`. It was unusable
anyway (clients address the daemon by well-known name) and would have been a
live hazard.

---

## 8. Gotchas and open items for Stage 12

- **`pkill -f <pattern>` kills the test script itself**, because the script's
  own command line contains the pattern. Cost two runs that looked like daemon
  hangs. Build the pattern at runtime (`KILLPAT=$(printf 'hevc_%s' 'vaapi')`)
  or match by name (`pkill -x ffmpeg`). Same class as Stage 8's missing
  `WAYLAND_DISPLAY` prefix.
- **"Matroska survives truncation" only helps once bytes reach the disk.**
  ffmpeg's AVIO buffer flushed roughly every ~250 KB in the measured run (18
  writeouts over 336 frames), so a recording SIGKILLed after 3.8 s / 38
  frames had written **nothing**, and `FfmpegSink`'s zero-byte cleanup
  correctly removed the empty file. A recording that dies *early* is lost;
  one that dies late keeps most of itself.
- **The `av1` output is tagged `color_space=unknown`**, where both VAAPI
  presets are tagged `bt709`/`tv` (ffprobe, live). The shipped software chain
  is §3.7's verbatim `format=yuv420p`, so this is left alone rather than
  re-litigated — but a four-colour round trip showed the fix is available and
  sticks if Stage 12/13 wants it: `scale=out_color_matrix=bt709:out_range=tv`
  before `format=yuv420p`, plus `-colorspace bt709 -color_range tv` (the
  `-color_primaries`/`-color_trc` tags did **not** survive into AV1-in-MKV).
  Measured colour difference between the two chains was ≤1/255 on a symmetric
  round trip, so this is a tagging gap, not a demonstrated colour error.
- **Region recording** is a monitor cast cropped in the filter chain
  (`crop=W:H:X:Y` before `hwupload` — note `filter_chain` already emits a
  `crop` as its first element, so region is a change to *that* rectangle, not
  a new filter). **Window recording** is `CastTarget::Window` + the
  `RecordWindow` failure handling Stage 10 already built. `from_dbus_options`
  is where the `kind` rejection lives.
- **`Stream.Parameters` is meaningless for window casts** (Stage 10: `size
  (1,1)`), so region/window crop math must take geometry from niri-ipc.
- **The app window's Record tab** (`modules::app`) still only waits for the
  stub reply. It now passes `cursor`/`output_dir`/`vaapi_device` from the
  config it loaded at boot, so it starts an *identical* recording to the CLI —
  what it lacks is the "hidden until `RecordingFinished`" wait, which needs a
  connection-independent subscription key (`zbus::Connection` isn't `Hash`) or
  a hand-rolled `iced::stream::channel` worker like
  `main.rs::dbus_worker_stream`. PLAN.md assigns that here.
- **The finish toast is Stage 12's task 3.** Stage 11 added only the
  *failure* toast (`ToastKind::Notice` — same card, same timing, same stack
  rule, no thumbnail, click does nothing). A notice currently renders a plain
  ivory 36 px tile because this crate still has no `src/icons.rs`; whichever
  stage first needs a real icon set fills that in.
- **A long recording is untested.** The longest run was 22.6 s. The fd-keyed
  dmabuf mapping cache, the drop counter and ffmpeg's memory over minutes are
  still unexercised — as is any recording on a *busy* screen, which is the
  only thing likely to exercise the frame-drop path.
- **Not tested, no safe way to provoke:** a genuinely full disk, a mid-stream
  `StreamState::Error`, a compositor-closed cast during a recording, more than
  one output.
