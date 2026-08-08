# Stage 1 handoff — repo skeleton + dependency survey

## Toolchain / workspace

- `cargo 1.97.1`, `rustc 1.97.1`, both stable channel; `rust-toolchain.toml` = `channel = "stable"` (mirrored verbatim from saola-panel).
- `rustfmt.toml` = default (single comment line), mirrored verbatim from saola-panel.
- Single binary crate, not a workspace. `[package] edition = "2021"`, `version = "0.1.0-dev"` (matches sibling convention — confirmed by checking saola-lockscreen's first commit, which also started at `0.1.0-dev`), `license = "MIT OR Apache-2.0"`.
- `cargo build && cargo clippy --all-targets -- -D warnings && cargo test` all pass clean (0 warnings, 0 tests present yet). `cargo fmt --check` passes.
- Network access to crates.io and github.com confirmed available in this environment (used `cargo info`, `cargo add --dry-run`, `git ls-remote`, `git clone` for the survey).

## Exact dependency versions resolved (from Cargo.lock)

| crate | resolved version |
|---|---|
| iced | 0.14.0 |
| iced_layershell | 0.19.1 |
| saola-theme | 0.5.0 (git tag `saola-theme-v0.5.0`, verified live against the remote tag) |
| zbus | 5.18.0 |
| niri-ipc | 26.4.0 (exact-pinned `=26.4.0`) |
| wayland-client | 0.31.15 |
| wayland-protocols | 0.32.13 |
| wayland-protocols-wlr | 0.3.12 |
| kdl | 6.7.1 |
| image | 0.25.10 |
| webp | 0.3.1 |
| libwebp-sys | **0.9.6** (transitive via `webp`'s `^0.9.3` requirement — NOT the 0.14.x line; see WebP section below, this matters) |
| wl-clipboard-rs | 0.9.3 |
| clap | 4.6.6 (feature `derive`) |
| pipewire | **not added** — Stage 9's job per plan. Latest is 0.10.0, rust-version 1.80; cumulative version-gate features up to `v1_0_0` (Jordan's PipeWire is 1.6.8, past that gate). `pipewire-sys` links system `libpipewire` via `system_deps`/pkg-config + `bindgen` — no vendored C compile, needs build-time `libclang` + pipewire dev headers.

`iced` features enabled: `tokio, svg, image, advanced, canvas` — same four as both siblings plus `canvas` (new; needed for Stage 13's annotation editor, which neither sibling has). `wayland` is not listed explicitly — it's already in iced 0.14's *default* feature set, confirmed via `cargo info iced`.

## Screencopy feature flag (verified, not inferred)

`wayland-protocols-wlr` has **no per-protocol cargo feature**. Read directly from the downloaded crate source (`~/.cargo/registry/src/.../wayland-protocols-wlr-0.3.12/{Cargo.toml,src/lib.rs}`): every protocol module (`screencopy::v1`, `layer_shell::v1`, `foreign_toplevel::v1`, `data_control::v1`, etc.) is compiled unconditionally. The crate's only features are `client` and `server`, which gate whether the `client`/`server` submodule (and its `wayland-client`/`wayland-server` dependency) is generated at all. This repo depends on it with `features = ["client"]` — that's the flag `screencopy.rs` needs for `wayland_protocols_wlr::screencopy::v1::client::...`. Same posture applied to `wayland-protocols` (`features = ["client"]`).

## WebP encoder pick

`image` 0.25's `WebPEncoder` is lossless-only — confirmed by reading `image-0.25.10/src/codecs/webp/encoder.rs` directly (doc comment says so explicitly). Picked `webp = "0.3"` (wraps `libwebp-sys`).

**Important resolution detail**: `libwebp-sys` resolves to **0.9.6** here (via `webp`'s `^0.9.3` requirement), not the 0.14.x line that shows up if you `cargo info libwebp-sys` cold. I inspected 0.14.x first (which has a `system-dylib` feature to link system libwebp via pkg-config instead of compiling), then caught the mismatch by checking `Cargo.lock` and reading 0.9.6's actual `build.rs`: **0.9.6 has no `system-dylib` feature at all** — it unconditionally vendors and compiles libwebp's C sources via the `cc` crate. So a build-time C compiler is a hard requirement, not a chosen tradeoff. Confirmed working (`cargo build` compiled it without issue on this machine). Fully static — no runtime `libwebp.so`, so the future PKGBUILD needs a `makedepends` C-toolchain entry, not a `depends` entry. Full essay is in `Cargo.toml`.

Rejected: spawning `cwebp` (would be a second external-CLI runtime dependency alongside ffmpeg, on the hot per-screenshot path — no precedent boundary for it like ffmpeg has).

## Clipboard pick

`wl-clipboard-rs = "0.9"` (resolves 0.9.3). Pure Rust — reuses the wayland-client stack already needed for screencopy (`libc`/`log`/`os_pipe`/`rustix` + wayland-client/wayland-backend, verified in its Cargo.toml — no new protocol dependency, no C toolchain, no external binary). Rejected spawning `wl-copy`/`wl-paste`: the `wl-clipboard` package is **not installed** on Jordan's machine (per PLAN.md Context, live-verified fact), so it'd add a third `sudo pacman -S` line and a silent-breakage risk on the always-on `--copy` default.

## CLI parser pick

`clap = { version = "4", features = ["derive"] }`. Four subcommands (`daemon`, `window`, `shot`, `record`, `pick-color`, `open`) with a real flag surface coming in Stage 3 (`--fullscreen`/`--region`/`--geometry`/`--format`/`--output`/`--delay`/`--cursor`/`--copy`/`--no-toast`/`--preset`/`--audio`, nested `record start|stop|toggle`) justified derive's generated `--help`/validation over hand-rolling. `cargo tree --features derive` in a scratch crate: 14 real crates; the syn/quote/proc-macro2 chain overlaps with what zbus's own proc macros and wayland-scanner already pull in, so marginal cost is small. `lexopt`/`pico-args` are zero-dependency and were seriously considered but rejected specifically for the subcommand + config-flag-precedence shape (Stage 3: CLI flags need "was this flag given" `Option` semantics over `capture.kdl` defaults).

## src/main.rs dispatch shape (Stage 1 stub — exact)

`clap::Parser` derive on `struct Cli { #[command(subcommand)] command: Command }`, with `#[command(name = "saola-capture", version, about = "...")]` (the `version` attr pulls `CARGO_PKG_VERSION`, i.e. `0.1.0-dev`, satisfying `--version`). `enum Command` has six variants with doc comments (become `--help` text): `Daemon`, `Window`, `Shot`, `Record`, `PickColor`, `Open` — **no flags on any variant yet**, that's Stage 3. `main()` matches and calls a shared `stub(verb: &str, note: &str)` helper that `println!`s and returns (exit 0) — no `todo!()`, no panics, satisfies the no-panic rule. Verified live: `cargo run -- daemon` / `shot` / `--version` / `--help` all behave as expected.

## Surprise: niri-ipc license

`niri-ipc` is `GPL-3.0-or-later` — verified by reading its own downloaded `Cargo.toml`, not just crates.io metadata (crates.io metadata agreed too). Rust static-links, so the distributed `saola-capture` binary is technically a combined work under GPL-3.0-or-later's terms even though the repo's own source stays `MIT OR Apache-2.0`. This is the exact same posture `saola-panel` already accepts with the same dependency — not a new decision made in this stage, just flagged since it's non-obvious and Stage 1's task explicitly asked for surprises. Not treated as a blocker.

## Other notes for Stage 2

- Pre-plan probes already answered several Stage 2 items (see `docs/research/2026-08-08-probes/README.md`) — screencopy handshake basics, Mutter.ScreenCast v4 handshake, cast-node dmabuf-only advertisement, enumeration via `niri msg casts`. Stage 2's remaining gaps are listed at the bottom of that README (shm negotiability, ffmpeg install + smoke test, RecordWindow id-space match, iced_layershell overlay viability in nested niri).
- `wayland-protocols-wlr`'s `foreign_toplevel::v1` module is available for free once Stage 2/7 needs window geometry — no extra crate, just use the module (already have `features = ["client"]` on).
- No deviations from the Stage 1 task as written. `cargo init` ran cleanly (kept `.gitignore`/`README.md`/licenses/docs/`CLAUDE.md`/`AGENTS.md` as instructed — only appended cargo's standard `/target` ignore lines to `.gitignore`, didn't touch anything else). Nothing committed; working tree left for review.
