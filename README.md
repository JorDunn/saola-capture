# saola-capture

Screenshot and video recording application for [Saola](https://github.com/JorDunn/saola-panel),
a Linux desktop environment built in Rust targeting the [niri](https://github.com/YaLTeR/niri)
compositor.

`Print` flashes the screen, saves a WebP, and pops a toast — click it to
annotate. Open the app to grab a region or window, or to record your screen
(HEVC/MKV via VA-API, AV1 and H.264 presets, with mic/system audio). A
scriptable CLI (`saola-capture shot --fullscreen --format=webp
--output=$HOME/Pictures`) covers automation.

**Status: planning.** The staged build plan is [PLAN.md](PLAN.md); agent
conventions are in [CLAUDE.md](CLAUDE.md). The UI follows the
[Saola style guide](docs/SAOLA-STYLE-GUIDE.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.
