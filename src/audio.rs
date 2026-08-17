//! Which PulseAudio sources a recording opens, and what happens when they
//! are not there — **Stage 13**.
//!
//! # The transport, in one paragraph
//!
//! CAPTURE-RESEARCH D7/§4.4 fixed the audio transport before any of this
//! existed: audio reaches the file as one or two `-f pulse` inputs that
//! **ffmpeg opens itself**, never as PCM crossing the [`EncoderSink`]
//! boundary (which is why that trait has no `write_audio` — see
//! `encode/mod.rs`'s head comment). So this module's whole job is to turn
//! `--audio mic|system|both` into *source name strings*, and to decide what
//! to do when it cannot.
//!
//! [`EncoderSink`]: crate::encode::EncoderSink
//!
//! # Pure above the line, the world below it
//!
//! Same split `encode::select_encoder`/`ffmpeg_cli::probe_encoder` already
//! uses, and for the same reason (PLAN.md Stage 13 task 3: "unit-test the
//! degradation path with fakes"):
//!
//! - [`plan_audio`] is a **pure function** over a [`PulseDevices`] snapshot.
//!   Every degradation rule lives there and is unit-tested against fake
//!   device lists — no audio server in the room.
//! - [`query_devices`] is the half that touches the machine: it asks
//!   **ffmpeg itself** what devices exist.
//!
//! # Why ffmpeg and not `pactl` (a deliberate deviation from D7's wording)
//!
//! D7 says "Device names come from `pactl list short sources` at record
//! time". The *decision* — resolve real names at record time, never hardcode
//! them — is unchanged here; only the tool that answers the question moved,
//! on evidence Stage 13 gathered (recorded in CAPTURE-RESEARCH §4.5):
//!
//! 1. **`ffmpeg -sources pulse` / `ffmpeg -sinks pulse` already answer it.**
//!    They print one line per device — `name [description] (media types)` —
//!    with the server's **default** device marked `*`, which is exactly the
//!    two facts this module needs. Measured at ~85 ms per call here.
//! 2. **It keeps ffmpeg the sole external CLI** (CLAUDE.md's standing rule,
//!    stated for `vainfo` and applying just as well here). `pactl` would be
//!    a second runtime binary, a second PKGBUILD `depends` entry, and a
//!    second thing whose absence has to degrade.
//! 3. **It asks the question through the library that will open the
//!    device.** `pactl` can be installed while *this* ffmpeg build lacks
//!    `--enable-libpulse`, in which case a `pactl`-resolved name would be
//!    resolved successfully and then fail at spawn — the worst ordering.
//!    ffmpeg's device list comes from the same `libavdevice` pulse backend
//!    `-f pulse` uses, so "enumeration worked" implies "capture can work".
//!
//! The naming conventions this relies on are Pulse's own and hold on
//! PipeWire's shim too (verified live, §4.5): a monitor source is named
//! `<sink name>.monitor`, and its description begins `Monitor of`.
//!
//! # What could not be measured here
//!
//! A *mid-recording* audio failure (the device disappears while ffmpeg is
//! reading it) cannot degrade to video-only on this transport: an input
//! cannot be removed from a running ffmpeg. It is handled by the recording's
//! existing failure path (`dbus`'s supervisor: the partial file is kept, an
//! `Error` signal and a toast are raised), and the honest fix is §4.4's
//! `pipe:3` escalation, which would own the audio clock and could simply
//! stop writing. Stage 13's live testing drove exactly this case — see the
//! handoff.

use std::process::{Command, Stdio};

use crate::config::AudioSource;
use crate::encode::AudioSpec;

/// `-itsoffset` applied to every pulse input by default, in seconds.
///
/// # Where this number comes from (PLAN.md Stage 13 task 2)
///
/// CAPTURE-RESEARCH §4.3 measured that ffmpeg normalises each input to its
/// own first packet, so whatever real gap exists between "ffmpeg opened the
/// audio device" and "the first video frame reached its stdin" becomes A/V
/// desync of exactly that size, silently, and `-copyts` does not rescue it.
/// §4.4's mitigation 3 is therefore: measure the residual and bake it in
/// here.
///
/// Stage 13 measured it two independent ways (transcripts in
/// CAPTURE-RESEARCH §4.5 and the Stage 13 handoff), and they answer *different
/// questions* — which is the whole reason this number is what it is:
///
/// 1. **The §4.3 start-offset**, read straight out of ffmpeg's own input dump
///    (`-loglevel info` prints each input's absolute `start` wallclock):
///    **−37 ms, and constant to ±1 ms across four real recordings**. So the
///    hazard §4.3 warns about is genuinely collapsed by mitigation 1 — and
///    D7's escalation trigger ("escalate to `pipe:3` if the residual is *not
///    constant*") is **not met**, which is why this crate still has no
///    `write_audio`.
/// 2. **The end-to-end clap test** — a player flashing white and clicking at
///    the same source timestamp, recorded through the real pipeline, both
///    events then located in the output file: **audio ahead of video by
///    +141 ms** (per-run means over five uncorrected 15 s recordings: 142,
///    139, 115, 139, 169).
///
/// The difference between the two is *path latency*, not start-offset: the
/// audio input is timestamped when ffmpeg reads the monitor tap (measured:
/// pulse reports **0 µs** of latency for that stream, so nothing is
/// back-dated), while a video frame is timestamped when its bytes reach
/// ffmpeg's stdin — after the compositor, the PipeWire delivery, the
/// `sync_channel(4)`, a 7–16 MB pipe write and ffmpeg's own read. Nothing in
/// `-use_wallclock_as_timestamps` can see that, so the file is genuinely
/// video-late by that much.
///
/// **So the shipped default corrects it**, at the measured mean rounded to
/// 10 ms; a corrected run measures a residual of +25 to +53 ms. The direction
/// matters: ITU-R BT.1359's detectability thresholds are asymmetric — audio
/// **ahead** of video is objectionable at 45 ms, audio **behind** only at
/// 125 ms — so an uncorrected +141 ms (audio ahead, 3× the threshold) is the
/// worse failure, and over-correcting lands in the tolerant direction.
///
/// # What this number is *not*
///
/// It is not exact, and the error budget is written down rather than hidden:
///
/// - **It is a mean over something that drifts.** Within a single recording
///   the offset grows — measured at +141 ms at t=2 s and +213 ms at t=14 s of
///   one 15 s capture, ≈5 ms/s — because a video frame's timestamp is
///   *arrival at ffmpeg*, so any encoder backlog pushes the whole video
///   timeline later as it accumulates. No constant can correct a drift. The
///   real fix is to take the video PTS from the compositor's own capture
///   timestamp (the SPA meta header PipeWire already delivers) instead of
///   from arrival — which needs a framed transport to ffmpeg rather than the
///   raw pipe, and is therefore a change to D6's model, not to this constant.
/// - **Its reference is a media player's own A/V sync**, which could not be
///   eliminated without a human clapping in front of a microphone.
/// - **It is not universal**: the dominant term is *this* machine's video
///   path latency, which scales with frame size and encoder load.
///
/// That is what `audio-offset` in `capture.toml` is for, and the Stage 13
/// handoff carries the exact re-measurement recipe.
pub const DEFAULT_SYNC_OFFSET: f64 = 0.13;

// ---------------------------------------------------------------------
// The device snapshot (data)
// ---------------------------------------------------------------------

/// One PulseAudio device, as ffmpeg's own device list reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulseDevice {
    /// The name `-f pulse -i <name>` takes.
    pub name: String,
    /// The human description in brackets. Kept because it is the secondary
    /// signal for [`Self::is_monitor`] and it is what a future picker UI
    /// would show; nothing in the resolution rules depends on it alone.
    pub description: String,
    /// The server's default device of this kind — ffmpeg marks it `*`.
    pub is_default: bool,
}

impl PulseDevice {
    /// Is this source a *monitor* (i.e. system audio) rather than a real
    /// input (a microphone)?
    ///
    /// The name suffix is the primary test — Pulse constructs a monitor
    /// source's name as `<sink name>.monitor` and PipeWire's shim does the
    /// same (verified live, §4.5). The description prefix is a secondary
    /// signal so a server that ever breaks the naming convention still
    /// classifies correctly rather than offering a monitor as a microphone.
    pub fn is_monitor(&self) -> bool {
        self.name.ends_with(MONITOR_SUFFIX) || self.description.starts_with("Monitor of")
    }
}

/// The suffix Pulse gives a sink's monitor source.
const MONITOR_SUFFIX: &str = ".monitor";

/// Everything [`plan_audio`] is allowed to know about the machine.
///
/// Deliberately a plain snapshot rather than a trait: unlike
/// `encode::select_encoder`'s oracle (where each answer costs a ~250 ms
/// trial encode and short-circuiting matters), both lists here come from two
/// fixed process spawns, so there is nothing to short-circuit and a struct is
/// the simpler injection point.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PulseDevices {
    /// Capture devices — microphones *and* monitors, undifferentiated, which
    /// is why [`PulseDevice::is_monitor`] exists.
    pub sources: Vec<PulseDevice>,
    /// Playback devices. Needed only to learn which sink is the **default**,
    /// whose `.monitor` is what "system audio" means.
    pub sinks: Vec<PulseDevice>,
}

/// `capture.toml`'s two device-override knobs, resolved.
///
/// Same posture as `vaapi-device` (CLAUDE.md's Config bullet): a name that
/// **exists** is used on its own, so the override genuinely overrides rather
/// than merely reordering; a name that does not exist warns and falls back to
/// discovery, because "is this a real device?" is a runtime question about
/// hardware that `config.rs` has a firm rule against trying to answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioOverrides {
    pub mic_source: Option<String>,
    pub system_source: Option<String>,
}

// ---------------------------------------------------------------------
// The plan (pure)
// ---------------------------------------------------------------------

/// What a recording should do about audio.
///
/// There is deliberately **no failure variant**. PLAN.md Stage 13 task 3:
/// "Audio failure … degrades to video-only with a warning toast — never a
/// dead pipeline." So the worst outcome expressible here is
/// `spec: None` plus a warning that says why, and the caller's only branch is
/// "is there a spec" — it cannot accidentally turn a missing microphone into
/// a failed recording.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioPlan {
    /// `None` means "record video only".
    pub spec: Option<AudioSpec>,
    /// Things the user should be told: a device that was asked for and not
    /// found, an override that was ignored. Raised as one warning toast and
    /// logged; never fatal.
    pub warnings: Vec<String>,
}

impl AudioPlan {
    /// The video-only plan, with nothing to say — what a recording started
    /// without `--audio` gets.
    pub fn silent() -> Self {
        Self::default()
    }

    /// Every warning as one sentence for the toast body, or `None` if there
    /// is nothing to say.
    pub fn warning(&self) -> Option<String> {
        if self.warnings.is_empty() {
            None
        } else {
            Some(self.warnings.join("; "))
        }
    }

    /// A one-line summary for the daemon log.
    pub fn summary(&self) -> String {
        match &self.spec {
            Some(spec) => format!("audio from {}", spec.sources.join(" + ")),
            None => "no audio".to_string(),
        }
    }
}

/// Resolve `request` against `devices` — the whole of Stage 13's device
/// selection and its degradation rules, as one pure function.
///
/// `offset` is `capture.toml`'s `audio-offset` in seconds; a zero offset
/// emits **no** `-itsoffset` argument at all rather than `-itsoffset 0`, so
/// the default command line is exactly the one Stage 11's tests already
/// describe plus the pulse input.
///
/// # The rules
///
/// **mic** — the first of: an `audio-mic-source` override that exists; the
/// server's default source, if it is not itself a monitor (a user who has
/// made a loopback their default source has not thereby made it a
/// microphone); the first non-monitor source. Otherwise there is no
/// microphone.
///
/// **system** — the first of: an `audio-system-source` override that exists;
/// `<default sink>.monitor`, if that source exists; the first monitor
/// source. Otherwise there is no system audio.
///
/// **both** — the two above, independently. Two hits are **mixed into one
/// track** (`encode::mix_arguments`, which carries the reasoning and the
/// `amix` parameters); **one hit still records**, with a warning naming the
/// half that is missing, because a recording with half the audio the user
/// asked for is strictly better than one with none; no hits degrade to
/// video-only.
pub fn plan_audio(
    request: AudioSource,
    devices: &PulseDevices,
    overrides: &AudioOverrides,
    offset: f64,
) -> AudioPlan {
    let mut warnings = Vec::new();

    let mic = match request {
        AudioSource::Mic | AudioSource::Both => {
            resolve_mic(devices, overrides.mic_source.as_deref(), &mut warnings)
        }
        AudioSource::System => None,
    };
    let system = match request {
        AudioSource::System | AudioSource::Both => {
            resolve_system(devices, overrides.system_source.as_deref(), &mut warnings)
        }
        AudioSource::Mic => None,
    };

    let sources: Vec<String> = match request {
        AudioSource::Mic => mic.into_iter().collect(),
        AudioSource::System => system.into_iter().collect(),
        // Mic first, system second — an arbitrary but fixed order, so the
        // `-filter_complex` labels below never depend on which one resolved.
        AudioSource::Both => mic.into_iter().chain(system).collect(),
    };

    if sources.is_empty() {
        warnings.push(format!(
            "recording video only — nothing on this machine can supply `--audio {}`",
            request.as_str()
        ));
        return AudioPlan {
            spec: None,
            warnings,
        };
    }

    AudioPlan {
        spec: Some(AudioSpec {
            sources,
            // A zero offset is "no correction", not "correct by zero".
            itsoffset: (offset != 0.0).then_some(offset),
        }),
        warnings,
    }
}

/// The `mic` half of [`plan_audio`]'s rules.
fn resolve_mic(
    devices: &PulseDevices,
    override_name: Option<&str>,
    warnings: &mut Vec<String>,
) -> Option<String> {
    if let Some(name) = override_name {
        match devices.sources.iter().find(|source| source.name == name) {
            Some(source) => return Some(source.name.clone()),
            None => warnings.push(format!(
                "capture.toml: audio-mic-source \"{name}\" is not a capture device this machine \
                 has — using the default microphone instead"
            )),
        }
    }

    let default = devices
        .sources
        .iter()
        .find(|source| source.is_default && !source.is_monitor());
    let first = devices.sources.iter().find(|source| !source.is_monitor());

    match default.or(first) {
        Some(source) => Some(source.name.clone()),
        None => {
            warnings.push(
                "no microphone is available (this machine reports no non-monitor capture device)"
                    .to_string(),
            );
            None
        }
    }
}

/// The `system` half of [`plan_audio`]'s rules.
fn resolve_system(
    devices: &PulseDevices,
    override_name: Option<&str>,
    warnings: &mut Vec<String>,
) -> Option<String> {
    if let Some(name) = override_name {
        match devices.sources.iter().find(|source| source.name == name) {
            Some(source) => return Some(source.name.clone()),
            None => warnings.push(format!(
                "capture.toml: audio-system-source \"{name}\" is not a capture device this \
                 machine has — using the default output's monitor instead"
            )),
        }
    }

    // The default sink's own monitor, by Pulse's `<sink>.monitor` naming
    // convention — checked against the real source list rather than assumed,
    // so a server that ever breaks the convention falls through to the scan
    // below instead of naming a device that cannot be opened.
    let default_monitor = devices
        .sinks
        .iter()
        .find(|sink| sink.is_default)
        .map(|sink| format!("{}{MONITOR_SUFFIX}", sink.name))
        .filter(|name| devices.sources.iter().any(|source| &source.name == name));

    let first_monitor = devices
        .sources
        .iter()
        .find(|source| source.is_monitor())
        .map(|source| source.name.clone());

    match default_monitor.or(first_monitor) {
        Some(name) => Some(name),
        None => {
            warnings.push(
                "no system-audio device is available (this machine reports no monitor source)"
                    .to_string(),
            );
            None
        }
    }
}

// ---------------------------------------------------------------------
// The world half
// ---------------------------------------------------------------------

/// Ask ffmpeg what audio devices exist, right now.
///
/// **Blocking** (two process spawns, ~85 ms each as measured) — callers run
/// it through `dbus::run_blocking`, never on the executor.
///
/// Never fails: an ffmpeg that cannot enumerate (not installed, built without
/// `--enable-libpulse`, no audio server running) simply yields empty lists,
/// which [`plan_audio`] already turns into the video-only degradation with a
/// warning. That is the whole no-panic story for this path — there is no
/// second error type to handle.
///
/// `want_sinks` skips the second spawn for `--audio mic`, which has no use
/// for the sink list.
pub fn query_devices(want_sources: bool, want_sinks: bool) -> PulseDevices {
    PulseDevices {
        sources: if want_sources {
            list_devices("-sources")
        } else {
            Vec::new()
        },
        sinks: if want_sinks {
            list_devices("-sinks")
        } else {
            Vec::new()
        },
    }
}

/// One `ffmpeg -sources pulse` / `ffmpeg -sinks pulse` run, parsed.
fn list_devices(flag: &str) -> Vec<PulseDevice> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", flag, "pulse"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();

    let Ok(output) = output else {
        eprintln!("saola-capture: audio: could not run `ffmpeg {flag} pulse` to list devices");
        return Vec::new();
    };

    // Not checked for success: ffmpeg exits 0 even for a device type it does
    // not have, printing nothing — so "no parsed lines" is the real signal
    // and an exit status adds nothing. Lossy UTF-8 because a device
    // description is server-supplied text this process should never reject.
    parse_device_list(&String::from_utf8_lossy(&output.stdout))
}

/// Parse ffmpeg's device-list output.
///
/// The shape, verbatim from a live run (§4.5):
///
/// ```text
/// Auto-detected sources for pulse:
///   alsa_output.pci-0000_07_00.6.analog-stereo.monitor [Monitor of Ryzen HD Audio Controller Analog Stereo] (none)
/// * alsa_input.pci-0000_07_00.6.analog-stereo [Ryzen HD Audio Controller Analog Stereo] (none)
/// ```
///
/// A leading `*` marks the server default; the name runs to the first ` [`;
/// the bracketed description is next; the trailing `(media types)` is
/// ignored. Any line that does not fit is skipped rather than guessed at —
/// this is a human-facing format being read by a machine, so the parser's
/// contract is "recognise what it recognises, ignore the rest", which
/// degrades a future format change into "no devices found" (a warning and a
/// video-only recording) rather than into a wrong device name.
fn parse_device_list(text: &str) -> Vec<PulseDevice> {
    let mut devices = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_end();
        // The header line, and anything else with no bracketed description.
        let Some(open) = trimmed.find(" [") else {
            continue;
        };
        let Some(close) = trimmed.rfind(']') else {
            continue;
        };
        if close < open {
            continue;
        }

        let (head, _) = trimmed.split_at(open);
        let is_default = head.trim_start().starts_with('*');
        let name = head.trim_start().trim_start_matches('*').trim();
        if name.is_empty() {
            continue;
        }
        let description = trimmed.get(open + 2..close).unwrap_or_default().to_string();

        devices.push(PulseDevice {
            name: name.to_string(),
            description,
            is_default,
        });
    }
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parsing -------------------------------------------------------

    /// The exact bytes a live `ffmpeg -sources pulse` printed on this machine
    /// (2026-08-09), including the `saola_test` null sink Stage 13's own
    /// measurement created.
    const LIVE_SOURCES: &str = "\
Auto-detected sources for pulse:
  alsa_output.pci-0000_07_00.6.analog-stereo.monitor [Monitor of Ryzen HD Audio Controller Analog Stereo] (none)
* alsa_input.pci-0000_07_00.6.analog-stereo [Ryzen HD Audio Controller Analog Stereo] (none)
  saola_test.monitor [Monitor of SaolaSyncTest] (none)
";

    const LIVE_SINKS: &str = "\
Auto-detected sinks for pulse:
* alsa_output.pci-0000_07_00.6.analog-stereo [Ryzen HD Audio Controller Analog Stereo] (none)
  saola_test [SaolaSyncTest] (none)
";

    fn live_devices() -> PulseDevices {
        PulseDevices {
            sources: parse_device_list(LIVE_SOURCES),
            sinks: parse_device_list(LIVE_SINKS),
        }
    }

    #[test]
    fn the_live_source_list_parses_into_three_devices() {
        let sources = parse_device_list(LIVE_SOURCES);
        assert_eq!(sources.len(), 3);
        assert_eq!(
            sources[0].name,
            "alsa_output.pci-0000_07_00.6.analog-stereo.monitor"
        );
        assert!(sources[0].is_monitor());
        assert!(!sources[0].is_default);
        assert_eq!(sources[1].name, "alsa_input.pci-0000_07_00.6.analog-stereo");
        assert!(!sources[1].is_monitor());
        assert!(sources[1].is_default, "ffmpeg marks the default with `*`");
        assert_eq!(sources[2].description, "Monitor of SaolaSyncTest");
    }

    #[test]
    fn the_header_line_is_not_a_device() {
        for device in parse_device_list(LIVE_SINKS) {
            assert!(!device.name.contains("Auto-detected"), "{device:?}");
        }
        assert_eq!(parse_device_list(LIVE_SINKS).len(), 2);
    }

    /// A format this parser does not recognise yields no devices, which the
    /// planner turns into a warning and a video-only recording — never a
    /// wrong device name.
    #[test]
    fn an_unrecognised_format_parses_to_nothing() {
        assert!(parse_device_list("").is_empty());
        assert!(parse_device_list("some future format\nwith no brackets").is_empty());
        // A bracketed description with no name is not a device either.
        assert!(parse_device_list("* [only a description]").is_empty());
    }

    #[test]
    fn a_description_containing_brackets_still_parses() {
        let devices = parse_device_list("  weird.name [Built-in Audio [analog]] (none)\n");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "weird.name");
        assert_eq!(devices[0].description, "Built-in Audio [analog]");
    }

    // -- resolution, on the real machine's device list -------------------

    #[test]
    fn mic_resolves_to_the_default_input_on_this_machine() {
        let plan = plan_audio(
            AudioSource::Mic,
            &live_devices(),
            &AudioOverrides::default(),
            0.0,
        );
        let spec = plan.spec.expect("a mic exists here");
        assert_eq!(spec.sources, ["alsa_input.pci-0000_07_00.6.analog-stereo"]);
        assert_eq!(spec.itsoffset, None, "a zero offset emits no argument");
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }

    #[test]
    fn system_resolves_to_the_default_sinks_monitor_not_just_any_monitor() {
        // Two monitors exist here and the *first* one listed happens to be
        // the right answer only because the default sink is the first sink;
        // this asserts the rule, not the coincidence — see the next test.
        let plan = plan_audio(
            AudioSource::System,
            &live_devices(),
            &AudioOverrides::default(),
            0.0,
        );
        assert_eq!(
            plan.spec.expect("a monitor exists here").sources,
            ["alsa_output.pci-0000_07_00.6.analog-stereo.monitor"]
        );
    }

    #[test]
    fn system_follows_the_default_sink_even_when_it_is_listed_last() {
        let mut devices = live_devices();
        for sink in &mut devices.sinks {
            sink.is_default = sink.name == "saola_test";
        }
        let plan = plan_audio(
            AudioSource::System,
            &devices,
            &AudioOverrides::default(),
            0.0,
        );
        assert_eq!(
            plan.spec.expect("a monitor exists").sources,
            ["saola_test.monitor"],
            "the default sink's monitor wins over the first-listed monitor"
        );
    }

    #[test]
    fn both_mixes_the_mic_and_the_system_monitor() {
        let plan = plan_audio(
            AudioSource::Both,
            &live_devices(),
            &AudioOverrides::default(),
            0.0,
        );
        let spec = plan.spec.expect("both exist here");
        assert_eq!(
            spec.sources,
            [
                "alsa_input.pci-0000_07_00.6.analog-stereo",
                "alsa_output.pci-0000_07_00.6.analog-stereo.monitor",
            ]
        );
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
        // What that pair becomes on the command line is `encode`'s half —
        // see `encode::mix_arguments` and its own tests.
    }

    // -- the degradation paths (PLAN.md Stage 13 task 3) -----------------

    /// A machine with no audio server at all — which is exactly what an
    /// ffmpeg that cannot enumerate produces (`query_devices` returns empty
    /// lists rather than an error).
    #[test]
    fn no_devices_at_all_degrades_to_video_only_for_every_request() {
        for request in [AudioSource::Mic, AudioSource::System, AudioSource::Both] {
            let plan = plan_audio(
                request,
                &PulseDevices::default(),
                &AudioOverrides::default(),
                0.0,
            );
            assert!(plan.spec.is_none(), "{request:?} must not invent a device");
            let warning = plan.warning().unwrap_or_default();
            assert!(warning.contains("video only"), "{warning}");
            assert!(warning.contains(request.as_str()), "{warning}");
        }
    }

    #[test]
    fn a_machine_with_only_a_monitor_still_records_system_audio() {
        let devices = PulseDevices {
            sources: parse_device_list("  hdmi.monitor [Monitor of HDMI] (none)\n"),
            sinks: parse_device_list("* hdmi [HDMI] (none)\n"),
        };
        let plan = plan_audio(
            AudioSource::System,
            &devices,
            &AudioOverrides::default(),
            0.0,
        );
        assert_eq!(
            plan.spec.as_ref().expect("system audio exists").sources,
            ["hdmi.monitor"]
        );
        assert!(plan.warnings.is_empty());

        // …but has no microphone, and says so rather than offering the
        // monitor as one.
        let plan = plan_audio(AudioSource::Mic, &devices, &AudioOverrides::default(), 0.0);
        assert!(plan.spec.is_none());
        assert!(
            plan.warning().unwrap_or_default().contains("no microphone"),
            "{:?}",
            plan.warnings
        );
    }

    /// `--audio both` with only half the hardware records the half that
    /// exists — a warning, not a failure, and not a silent full degradation.
    #[test]
    fn both_with_no_microphone_records_system_audio_and_warns() {
        let devices = PulseDevices {
            sources: parse_device_list("  hdmi.monitor [Monitor of HDMI] (none)\n"),
            sinks: parse_device_list("* hdmi [HDMI] (none)\n"),
        };
        let plan = plan_audio(AudioSource::Both, &devices, &AudioOverrides::default(), 0.0);
        assert_eq!(
            plan.spec.as_ref().expect("half is still audio").sources,
            ["hdmi.monitor"]
        );
        let warning = plan.warning().expect("the missing half is reported");
        assert!(warning.contains("no microphone"), "{warning}");
        assert!(!warning.contains("video only"), "{warning}");
    }

    #[test]
    fn both_with_no_monitor_records_the_microphone_and_warns() {
        let devices = PulseDevices {
            sources: parse_device_list("* usb.mic [USB Microphone] (none)\n"),
            sinks: Vec::new(),
        };
        let plan = plan_audio(AudioSource::Both, &devices, &AudioOverrides::default(), 0.0);
        assert_eq!(
            plan.spec.as_ref().expect("half is still audio").sources,
            ["usb.mic"]
        );
        assert!(
            plan.warning()
                .unwrap_or_default()
                .contains("no system-audio"),
            "{:?}",
            plan.warnings
        );
    }

    /// A default source that is itself a monitor (a user who routed a
    /// loopback as their default input) is not a microphone.
    #[test]
    fn a_monitor_default_source_is_not_offered_as_the_microphone() {
        let devices = PulseDevices {
            sources: parse_device_list(
                "* loop.monitor [Monitor of Loopback] (none)\n  usb.mic [USB Microphone] (none)\n",
            ),
            sinks: Vec::new(),
        };
        let plan = plan_audio(AudioSource::Mic, &devices, &AudioOverrides::default(), 0.0);
        assert_eq!(plan.spec.expect("a real mic exists").sources, ["usb.mic"]);
    }

    // -- the overrides ---------------------------------------------------

    #[test]
    fn an_override_that_exists_is_used_on_its_own() {
        let overrides = AudioOverrides {
            system_source: Some("saola_test.monitor".to_string()),
            ..AudioOverrides::default()
        };
        let plan = plan_audio(AudioSource::System, &live_devices(), &overrides, 0.0);
        assert_eq!(
            plan.spec.as_ref().expect("the override exists").sources,
            ["saola_test.monitor"],
            "the override wins over the default sink's own monitor"
        );
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }

    #[test]
    fn an_override_that_does_not_exist_warns_and_falls_back_to_discovery() {
        let overrides = AudioOverrides {
            mic_source: Some("alsa_input.usb-gone".to_string()),
            ..AudioOverrides::default()
        };
        let plan = plan_audio(AudioSource::Mic, &live_devices(), &overrides, 0.0);
        assert_eq!(
            plan.spec
                .as_ref()
                .expect("discovery still finds a mic")
                .sources,
            ["alsa_input.pci-0000_07_00.6.analog-stereo"]
        );
        let warning = plan.warning().expect("an ignored override is worth saying");
        assert!(warning.contains("audio-mic-source"), "{warning}");
        assert!(warning.contains("alsa_input.usb-gone"), "{warning}");
    }

    /// The failure mode this guards: an override that names something real
    /// on a machine with **no** fallback must still degrade, not panic or
    /// pass the bad name to ffmpeg.
    #[test]
    fn a_bad_override_with_nothing_to_fall_back_to_still_degrades() {
        let overrides = AudioOverrides {
            system_source: Some("nope.monitor".to_string()),
            ..AudioOverrides::default()
        };
        let plan = plan_audio(
            AudioSource::System,
            &PulseDevices::default(),
            &overrides,
            0.0,
        );
        assert!(plan.spec.is_none());
        let warning = plan.warning().unwrap_or_default();
        assert!(warning.contains("audio-system-source"), "{warning}");
        assert!(warning.contains("video only"), "{warning}");
    }

    // -- the offset ------------------------------------------------------

    #[test]
    fn a_non_zero_offset_reaches_the_spec() {
        let plan = plan_audio(
            AudioSource::Mic,
            &live_devices(),
            &AudioOverrides::default(),
            0.075,
        );
        assert_eq!(plan.spec.expect("a mic exists").itsoffset, Some(0.075));
    }

    /// The shipped correction, and the direction it must point.
    ///
    /// A **positive** `-itsoffset` delays the audio input, which is what
    /// corrects the measured "audio ahead of video". A sign flip here would
    /// double the desync instead of removing it, silently, in the direction
    /// ITU-R BT.1359 says is the objectionable one — so the sign is asserted,
    /// not just the value.
    #[test]
    fn the_shipped_default_offset_delays_the_audio() {
        // A `const` block, so "the sign is right" is a compile-time fact
        // rather than a runtime assertion clippy calls pointless.
        const _: () = assert!(DEFAULT_SYNC_OFFSET > 0.0);
        assert_eq!(DEFAULT_SYNC_OFFSET, 0.13);
        let plan = plan_audio(
            AudioSource::Mic,
            &live_devices(),
            &AudioOverrides::default(),
            DEFAULT_SYNC_OFFSET,
        );
        assert_eq!(
            plan.spec.expect("a mic exists").itsoffset,
            Some(0.13),
            "the default reaches the command line rather than being dropped as \"no correction\""
        );
    }

    // -- the plan's own reporting ---------------------------------------

    #[test]
    fn a_silent_plan_says_nothing_and_records_nothing() {
        let plan = AudioPlan::silent();
        assert!(plan.spec.is_none());
        assert_eq!(plan.warning(), None);
        assert_eq!(plan.summary(), "no audio");
    }

    #[test]
    fn the_summary_names_every_source() {
        let plan = plan_audio(
            AudioSource::Both,
            &live_devices(),
            &AudioOverrides::default(),
            0.0,
        );
        assert_eq!(
            plan.summary(),
            "audio from alsa_input.pci-0000_07_00.6.analog-stereo + \
             alsa_output.pci-0000_07_00.6.analog-stereo.monitor"
        );
    }
}
