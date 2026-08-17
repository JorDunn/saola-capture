# Stage 13 handoff — audio capture → Opus track

Forward-facing context for **Stage 14** (annotation editor — touches nothing
here, but read §6 if you ever build a video preview) and **Stage 17**
(packaging/README — §2's config schema and §1's "no new runtime dependency"
result are yours).

New file: `src/audio.rs` (~640 lines incl. 20 tests). Touched: `src/config.rs`,
`src/cli.rs`, `src/encode/mod.rs`, `src/dbus.rs`, `src/main.rs`,
`src/modules/recorder.rs`, `src/modules/app.rs`, `docs/CAPTURE-RESEARCH.md`
(new §4.5), `CLAUDE.md`. **No new dependency and no new runtime binary.**
**Nothing committed**, as every prior stage.

Gates at hand-off: `cargo build`, `cargo clippy --all-targets -- -D warnings`,
`cargo fmt --check` clean. `cargo test`: **373 passed** (Stage 12: 335).

---

## 0. What is and isn't wired

**Is:** `--audio none|mic|system|both` for real (Opus, one track), from the
CLI, from `capture.toml`, and from the app window's Record tab; four new
config knobs; the pure device-selection/degradation planner with fakes;
video-only degradation with a warning toast; the measured `-itsoffset`
correction and its knob.

**Is not:** PCM on `pipe:3` (D7's escalation — its trigger was checked and
**not met**, see §3, so `EncoderSink` still has no `write_audio`); per-app
audio capture (the pulse shim cannot express it); two *separate* audio tracks
for `--audio both` (§4); any mid-recording audio degradation (§5); the
video-PTS drift fix (§3, the one real open item).

---

## 1. The transport as shipped

Exactly D7: one or two `-f pulse` inputs that **ffmpeg opens itself**, `-c:a
libopus -b:a 96k`, `-shortest`. No PCM crosses `EncoderSink`. A real argv,
verbatim from the live run:

```
ffmpeg -hide_banner -nostats -loglevel warning -y \
  -f rawvideo -pixel_format bgr0 -video_size 2560x1600 -framerate 1000 \
  -use_wallclock_as_timestamps 1 -i pipe:0 \
  -itsoffset 0.13 -f pulse -i alsa_output.pci-0000_07_00.6.analog-stereo.monitor \
  -vaapi_device /dev/dri/renderD128 -fps_mode:v vfr \
  -vf crop=2560:1600:0:0,hwupload,scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=tv \
  -c:v hevc_vaapi -b:v 20M -c:a libopus -b:a 96k -shortest -f matroska <path>
```

**One deliberate deviation from D7's wording, with evidence** (CAPTURE-RESEARCH
§4.5): device names come from **ffmpeg**, not `pactl`.
`ffmpeg -sources pulse` / `-sinks pulse` print `name [description] (media
types)` with the server default marked `*` — every fact the selection rule
needs, ~85 ms per call. Three reasons, in order of weight: it asks the question
*through the libavdevice backend that will open the device* (a `pactl`-resolved
name on an ffmpeg without `--enable-libpulse` resolves fine and then fails at
spawn — the worst ordering); it keeps ffmpeg the sole external CLI, which
CLAUDE.md already states for `vainfo`; and it adds nothing to the PKGBUILD
(**Stage 17: no `libpulse` in `depends`** — a `pactl` design would have needed
it). Pulse's `@DEFAULT_SOURCE@`/`@DEFAULT_MONITOR@` magic names were confirmed
to work through PipeWire's shim and deliberately not used: a concrete name can
be checked for existence *before* spawning, which is what makes the degradation
path possible at all.

**`-shortest` alone is enough to stop it.** §4.3 warned `-f pulse` never EOFs;
measured here, closing stdin ends the process promptly (3 s of piped source →
2.68 s wall). `FfmpegSink::finish`'s 30 s SIGINT escalation was never reached.

## 2. Device selection and the config schema (Stage 17 needs this)

```toml
audio = "none"                    # "none"|"mic"|"system"|"both", default "none"
audio-mic-source = "alsa_input.…" # unset = the server's default input
audio-system-source = "….monitor" # unset = the default output's .monitor
audio-offset = 0.13               # seconds of -itsoffset; default audio::DEFAULT_SYNC_OFFSET
```

Resolution rules (all in `audio::plan_audio`, all unit-tested with fake device
lists):

- **mic** — an `audio-mic-source` that exists → the default source *if it is
  not itself a monitor* → the first non-monitor source → none.
- **system** — an `audio-system-source` that exists → `<default sink>.monitor`
  *if that source exists* → the first monitor source → none.
- **both** — the two independently. Two hits mix; **one hit still records**
  with a warning naming the missing half; no hits degrade to video-only.
- An override that does **not** exist warns and falls back to discovery —
  identical posture to `vaapi-device`, and for the identical reason (`config.rs`
  refuses to answer hardware questions).

`--audio` beats the config; `--audio none` beats a config that turned it on
(there is no other way to say "not this time" from a keybind); an unrecognized
word is still a hard CLI error. `config::AudioSource` **moved from `cli.rs` to
`config.rs`** (next to `ImageFormat`/`VideoPreset`, same "one vocabulary"
reason) and is re-exported, so `cli::AudioSource` still names it.
`config::parse_audio` returns a double `Option` on purpose: "understood, and it
means silence" and "not understood" must not collapse.

## 3. A/V sync — what was measured, and the one thing still wrong

Two independent measurements, both on the real session, 2026-08-09
(full transcripts and method in CAPTURE-RESEARCH §4.5):

| quantity | result |
| --- | --- |
| §4.3 start-offset (`audio_start − video_start`, from ffmpeg's `-loglevel info` input dump) | **−37 ms, constant to ±1 ms** over four runs |
| end-to-end clap test (flash vs click in the output file) | **audio ahead by +141 ms**; per-run means 142, 139, 115, 139, 169 |
| **within one recording** | **drifts: +141 ms at t=2 s → +213 ms at t=14 s** (≈5 ms/s) |
| pulse latency reported for ffmpeg's capture stream | **0 µs** (audio is not back-dated) |
| with `-itsoffset 0.13` | residual **+25…+53 ms** |

**D7's escalation trigger is not met.** "Escalate to `pipe:3` only if the
measured offset is not constant across runs" — the start-offset is constant to
±1 ms, so `pipe:3` was not built and `EncoderSink` still has no `write_audio`.

**What the +141 ms actually is**: not start-offset but *path latency*. A video
frame is timestamped when its bytes reach ffmpeg's stdin — after the
compositor, PipeWire, the `sync_channel(4)`, a multi-MB pipe write and ffmpeg's
own read — while the audio is timestamped on read from the monitor tap with
nothing to compensate. `DEFAULT_SYNC_OFFSET = 0.13` corrects the mean; the
direction was chosen against ITU-R BT.1359's *asymmetric* thresholds (audio
ahead objectionable at 45 ms, behind only at 125 ms), so leaving +141 ms
uncorrected was the worse failure and over-correcting lands in the tolerant
direction.

**The open item, and it is the real one: the drift.** No constant corrects a
drift. The fix is to take the video PTS from the compositor's own capture
timestamp (the SPA meta header PipeWire already delivers) instead of from
arrival — which needs a *framed* transport to ffmpeg rather than the raw pipe,
i.e. a change to CAPTURE-RESEARCH D6's model. Not attempted here; the evidence
is recorded so whoever does it starts with numbers.

**Honest limits of the measurement** (do not quote 0.13 as exact):
- the clap test's reference is `ffplay`'s own A/V sync, which could not be
  removed without a human clapping into a microphone;
- the dominant term scales with frame size and encoder load, so it is a
  *this machine* number — hence the knob;
- **the human-verify item**: Jordan clapping in front of the mic once, with
  `--audio mic`, settles both the magnitude and the mic path's own offset (the
  mic path's offset is **unmeasured** — only the monitor path was).

**Re-measurement recipe** (also in CLAUDE.md's Testing section): generate one
clip whose white flash and 1 kHz click share a timestamp, play it with
`ffplay` into a scratch null sink, record *that window* with `--audio system`,
then locate the flash with `signalstats,metadata=print:key=lavfi.signalstats.YAVG`
and the click with `silencedetect=noise=-40dB:d=0.02`. Validate the harness
against the source file first (it measured 0.0 ms there). Cross-check
player-free with `SAOLA_CAPTURE_FFMPEG_LOGLEVEL=info` and the input dump's
absolute `start` values.

## 4. `--audio both` is one mixed track, not two

`[1:a][2:a]amix=inputs=2:duration=longest:normalize=0[aout]`, plus `-map 0:v:0
-map [aout]` (a complex graph turns automatic stream selection off).
`encode::mix_arguments` carries the reasoning: §4.4 said `amix`; MKV could hold
two tracks but the `h264` preset's MP4 is the *compatibility* preset and
track-switching is exactly what its audience won't do. **`normalize=0`** is the
argued parameter — the default `normalize=1` divides by the input count, so
adding a microphone would make system audio 6 dB quieter, which is a surprising
thing for adding a microphone to do; clipping is fixable by turning a source
down, a 6 dB loss is not. A future stage wanting separate tracks should add a
*third* `--audio` spelling rather than redefine `both`.

**`-vf` survives alongside `-filter_complex`** — verified live, because the
failure mode (a silently ignored `-vf`) would send un-cropped, un-uploaded
frames at the VAAPI encoder. `-itsoffset` is repeated per input, because it is
a per-input option.

## 5. Degradation (task 3) — what it does and what it cannot do

`audio::AudioPlan` has **no failure variant**, by construction: the worst it
can express is `spec: None` plus warnings, so a caller cannot turn a missing
microphone into a failed recording. `dbus::CaptureService::resolve_audio` runs
it *after* target resolution (an interactive region selection can sit on the
overlay for minutes and a snapshot taken before it would be stale) and *before*
the cast opens (so a degradation costs nothing to unwind), then raises
`DaemonEvent::Warning` — **the first event in that enum with no D-Bus signal
beside it**, deliberately: the `io.saola.Capture1` signal set is the frozen
saola-notifications contract and `Error` means the recording *failed*; this one
did not.

**The consequence, recorded rather than hidden**: a `record start --audio mic`
typed in a terminal prints nothing about a degradation — `StartRecording`
returns `()` and has nowhere to carry it. It reaches the user as the toast and
the daemon log. Closing that would mean changing the method's return type,
which is a wire-contract break for a v0.1 nicety.

**Mid-recording audio failure has no degradation path on this transport** and
that is a property of D7, not an oversight: an input cannot be removed from a
running ffmpeg. Driven live anyway (§7) — the pulse shim rerouted and the
recording survived intact. A genuine mid-recording drop is the *second* thing
`pipe:3` would buy.

## 6. Two pre-existing timing defects this stage found and fixed

Both were invisible in a file's duration and in every `cargo test`; both were
found only by measuring A/V sync, and both affect **video-only** recordings
too, so Stage 11's and 12's output was wrong in these ways.

1. **The rawvideo input had no `-framerate`, so its time base was 25 Hz.**
   `-use_wallclock_as_timestamps` rescaled every PTS onto a **40 ms grid** and
   `-fps_mode:v vfr` discarded whatever landed in a taken slot. Measured: a
   60 fps source came out **24.8 fps / 137 frames**; with
   `-framerate 1000` (`encode::RAWVIDEO_TIMEBASE_HZ`) the same source came out
   **66.2 fps / 360 frames**, 1 ms PTS grid, same duration, keyframe interval
   unchanged. **Every recording this project made before Stage 13 was
   effectively 25 fps.** Read that constant's doc comment before touching the
   pair — `-framerate` alone (without the wallclock flag beside it) is §4.2's
   3× fast-forward bug.
   One cosmetic cost: ffmpeg now logs `Stream #0: not enough frames to estimate
   rate` once per recording at `warning`.
2. **`-shortest` truncated the audio to the video.** A niri cast is
   damage-driven, so a still screen produces one frame and the video stream
   ends at 0.04 s — and `-shortest` (which an infinite `-f pulse` input
   *requires*) cut the audio there too. Measured: a 7.1 s `--audio system`
   recording of an idle screen was **0.048 s of video and 0.048 s of audio**.
   `modules::recorder::seal_last_frame` now re-writes the most recent frame
   once, at stop, on a *clean* end only; the same recording is now **8.28 s of
   video and 415 audio packets**. The frame is *moved*, never copied
   (`pump_frames` keeps it instead of dropping it), and `spin_up`'s own first
   frame is handed to the pump as a `seed` so a recording that never saw a
   second frame is still sealed.

## 7. Verified live (real session, 2026-08-09)

No daemon owned `io.saola.Capture1` (checked first). Test daemon on the real
session bus with `XDG_DATA_HOME` and `save-dir` in scratch dirs; a scratch
`module-null-sink` for the sync work, unloaded afterwards.

| check | result |
| --- | --- |
| `--audio system` | `alsa_output.….monitor` resolved and recorded; HEVC + Opus in one MKV |
| `--audio mic` | `alsa_input.….analog-stereo` resolved; valid 3.28 s stereo 48 kHz Opus track; **artifact deleted in the same command**, never analysed |
| `--audio both` | two `-itsoffset 0.13 -f pulse` inputs + `amix=inputs=2:duration=longest:normalize=0[aout]` + `-map 0:v:0 -map [aout]` → one mixed Opus track |
| `audio = "system"` in `capture.toml` | a bare `record start` recorded the monitor |
| `--audio none` | overrode that config → `no audio` |
| `--audio bluetooth` | still a clean CLI error naming the four accepted words |
| bogus `audio-mic-source` | warned by name, fell back to the default mic, recording unaffected |
| daemon with `PULSE_SERVER=/nonexistent`, `--audio both` | logged all three reasons, recorded **video only**, 3.28 s file, no failure |
| the warning toast | its layer surface mapped and unmapped: `niri msg layers` saola-capture count **1 → 2 → 1** |
| `pactl unload-module` on the sink being recorded, mid-recording | **recording survived** — pulse rerouted, 7.28 s of continuous audio, no error |
| idle-screen recording with audio | 8.283 s video / 8.264 s audio (was 0.048 s before the seal fix) |
| A/V sync | see §3 |
| teardown, every run | bus released, `niri msg casts` → "No screencasts", `pgrep -x ffmpeg` empty, no `ffplay`, null sink unloaded, no sink-inputs, default sink/source unchanged, only the two webcam `Video/Source` nodes, Jordan's real `history.jsonl` untouched |

**Not verified live** (and why): starting an audio recording *from the app
window* (needs a GUI click — same gap Stage 12 left); the warning toast's
*pixels* (the session locked mid-testing — verified by surface instead); the
mic path's own A/V offset; anything needing a human clap.

## 8. Gotchas for whoever is next

- **An option map carries decisions, and a zero can be one.** The one real bug
  this stage's live testing caught: `to_dbus_options` omitted `audio-offset`
  when it was zero, and the decode side defaults an absent key to
  `DEFAULT_SYNC_OFFSET` (0.13) — so `audio-offset = 0.0` in `capture.toml`
  silently kept the correction and the knob could not be turned off. Unlike
  `geometry`/`window-id`, absence is *not* the decision here. `cargo test`
  cannot see across the D-Bus boundary; only an A/V measurement found it.
- **`CaptureConfig` and `RecordOptions` lost their `Eq` derives** (`f64`
  fields). Nothing needed `Eq`; if something does, it needs a different
  representation for the offset, not a `#[allow]`.
- **`PULSE_SINK` does not route SDL/ffplay** (verified — a test player went to
  the real speakers). Use `pactl move-sink-input`. Never
  `pactl set-default-sink`: that is Jordan's session state.
- **`audio::query_devices` never fails.** An ffmpeg that cannot enumerate
  yields empty lists, which is exactly the input the degradation path takes —
  there is deliberately no second error type in that path.
- **`plan_audio` takes a snapshot, not an oracle** (unlike
  `encode::select_encoder`): both device lists come from two fixed process
  spawns, so there is nothing to short-circuit.
- **The `-framerate 1000` / `-use_wallclock_as_timestamps 1` pair is only safe
  together.** Separating them re-introduces §4.2's fast-forward.
- **Video-only recordings changed too** (both §6 fixes). A regression there
  would show as a 25 fps recording or a too-short file, not as an error.
