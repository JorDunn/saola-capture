# Stage 4 handoff — config migration: KDL → TOML

Forward-facing context for Stage 5 (screencopy backend + WebP/PNG save +
clipboard, per this repo's current numbering). This stage touched only
`Cargo.toml`, `src/config.rs`, doc comments in `src/cli.rs`/`src/main.rs`,
and `CLAUDE.md` — nothing else in `src/` changed, and `Cargo.lock` was
regenerated via `cargo build`/`cargo remove kdl` (never hand-edited).

---

## `CaptureConfig`'s public API is unchanged

Every field, every default, every method signature on
[`CaptureConfig`](../../src/config.rs) is identical to what Stage 3's
handoff documented — `resolve_path`, `load`, `parse`, the eight fields
(`save_dir: Option<PathBuf>`, `image_format`, `png_also`, `video_preset`,
`cursor`, `delay: u32`, `toasts`, `copy`), `ImageFormat`/`VideoPreset` with
their `parse`/`as_str`/`Display` impls. **No caller in `src/cli.rs` or
`src/main.rs` changed behavior** — only their doc comments got a
`capture.kdl` → `capture.toml` text substitution (they're `clap` `///` doc
comments that become `--help` text, so leaving the old filename in them
would have been user-facing staleness, not just an internal comment). Stage
3's handoff stays accurate for everything except the file format itself and
its pre-renumber stage numbers (add 1 to any stage reference ≥ 4 in that
document, per CLAUDE.md's amendment note).

---

## The schema, verbatim

`~/.config/saola/capture.toml` (resolution: `--config-dir` >
`$SAOLA_CONFIG_DIR` > `$XDG_CONFIG_HOME/saola` > `~/.config/saola` —
unchanged from Stage 3). **No `[capture]` wrapper table** — every knob is a
bare top-level key (the file is already this app's own, unlike the panel's
shared-shape precedent):

```toml
save-dir = "~/Pictures/Screenshots"  # default: unset (storage.rs, Stage 5, falls back to ~/Pictures/Captures)
image-format = "webp"                # "webp" | "png", default "webp"
png-also = false                     # default false
video-preset = "hevc"                # "hevc" | "av1" | "h264", default "hevc"
cursor = true                        # default true
delay = 0                            # whole seconds, default 0
toasts = true                        # the saola-notifications kill-switch, default true
copy = true                          # default true
```

Resilience (unit-tested, `src/config.rs`, 19 `#[test]`s — up from Stage 3's
18): no file → defaults, silently. Malformed TOML → one `eprintln!` + full
defaults. A single bad knob value (wrong type, unrecognized string,
negative/fractional `delay`) → warn + that knob's default, rest of the
document still applies. **The KDL v2 `#true`/`#false` gotcha is gone** — TOML
uses bare `true`/`false`, so there's nothing to warn Stage 17's README
writer about there.

**New in this stage**: a `capture.kdl` found in the resolved config dir with
no `capture.toml` next to it logs a one-line migration hint (`eprintln!`,
not an error) naming both paths, then proceeds with defaults exactly as any
other missing-file case would. `capture.kdl` is **no longer read at all** —
the hint only fires off `Path::is_file()`, it never attempts to parse or
port the old file's contents.

---

## Crate survey outcome (also in `Cargo.toml`'s essay)

Picked **`toml = "0.9"`** (resolves **0.9.12**), default features. Walked by
hand via `toml::Table` (`Map<String, Value>`, `.get`/`.as_str`/`.as_bool`/
`.as_integer`) — **no `serde::Deserialize` derived on `CaptureConfig`**; the
crate's default `serde` feature only powers `Table`/`Value`'s own internal
`Deserialize` impl, which is what makes the generic-value-tree walk possible
without deriving on our struct.

**Why the `0.9` line specifically, not the newer `1.x` line** (`cargo add
toml` alone resolves 1.1.4): `saola-theme`'s own `saola-tokens` crate already
depends on `toml = "0.9"` (verified in its `Cargo.toml`, resolves 0.9.12) to
parse its own token files. Picking the same major line let Cargo unify to
that *already-resolved* crate instance instead of building a second major
version side by side — confirmed via `cargo tree -i toml` / `cargo tree -i
toml_edit` **before** the pick: `toml_datetime 0.7.5`, `toml_parser`,
`toml_writer`, `winnow 0.7.15`, `serde_core`, `serde_spanned` were all
already in `Cargo.lock` at the exact versions this pick resolves to, so
adding `toml = "0.9"` added **zero net new crates** to the dependency tree
(confirmed post-add: `cargo build` recompiled nothing beyond `toml` itself
and the two `saola-theme`/`saola-tokens` crates that already depended on it).

Rejected: `toml_edit` (format-preserving; no `serde` dep at all, but this
module never writes the file back out, so preservation buys nothing, and it
was already in the tree only as a **build-time proc-macro** dependency via
`zbus_macros` → `proc-macro-crate`, not a runtime one — reusing `saola-theme`'s
real runtime `toml` dependency was the more justified reuse). `basic-toml`
(inspected its source directly: only exposes `from_str<T: Deserialize>` /
`to_string<T: Serialize>`, no generic `Value`/`Table` type at all — would
have forced deriving `Deserialize` on `CaptureConfig`, exactly the
one-shot-error failure mode the hand-walked posture exists to avoid;
rejected outright).

`kdl = "6.7.1"` was removed from `Cargo.toml` entirely and confirmed gone
from `Cargo.lock` (`cargo remove kdl` — no hand-editing) — `grep -c '^name =
"kdl"' Cargo.lock` returns `0`.

---

## Migration debt note (for whoever writes Stage 17's README, and for now)

- `docs/CAPTURE-RESEARCH.md:978` and `.claude/handoffs/handoff_stage_2.md:185`
  still say "…override knob to `capture.kdl`" — both are dated historical
  documents (research transcript, a prior stage's own handoff) and, per this
  repo's own established convention (CLAUDE.md's amendment note explicitly
  leaves handoffs 1–3's stage numbering un-rewritten rather than editing
  history), were **not** rewritten. Anyone acting on either document should
  mentally substitute `capture.toml`.
- `Cargo.toml:194` (inside Stage 1's original CLI-parser survey essay, now
  superseded by this stage's edits elsewhere in the same file) also still
  says `capture.kdl` in one clause — same "historical essay, not rewritten"
  reasoning; the essay's substantive content (why `clap` over `lexopt`) is
  still correct, only the example filename inside it is dated.
- `PLAN.md` itself (lines ~420, ~480, ~485) still describes the Stage-3-era
  KDL schema and says "the Commands example mentioning `capture.kdl`" —
  this is PLAN.md's own **task description text** for Stage 3 and this
  stage, not live documentation; it wasn't in this stage's edit scope (only
  `CLAUDE.md` was) and PLAN.md generally isn't rewritten after a stage
  lands, per the plan-orchestrator convention.
- **Nothing user-facing still points at `capture.kdl`** as the live config
  path: `CLAUDE.md`, `src/config.rs`'s own doc comments, and every `clap`
  `--help` string in `src/cli.rs`/`src/main.rs` now say `capture.toml`.

---

## Gotchas Stage 5 needs

1. **`toml::Table::get` returns `Option<&Value>`, and `Value::as_str`/
   `as_bool`/`as_integer` each return `Option<...>`** — same two-layer
   `Option` shape the KDL version had (`get_arg` then `as_string`/`as_bool`),
   just a different crate. If Stage 5 or later ever needs to read a new knob
   out of `capture.toml`, `config.rs`'s existing `read_str`/`read_bool`/
   `read_delay` helpers are the pattern to extend, not reinvent.
2. **`toml::de::Error` (the type `ConfigError` wraps) implements `Display`
   and `std::error::Error` directly** — no `map_err`-to-a-different-error-type
   dance was needed, unlike some of the `zbus`/`zvariant` friction Stage 3's
   handoff flagged.
3. **The `toml` crate's default features include `serde`** — this is
   already explained above and in both `Cargo.toml`'s essay and
   `config.rs`'s module doc comment, but it's worth restating here since
   it's an easy thing to misread as "this module secretly derives
   Deserialize now": it doesn't. `serde` powers `toml::Table`/`Value`'s own
   internal parsing; `CaptureConfig` itself has no `#[derive(Deserialize)]`
   anywhere.
4. **Bare TOML keys allow dashes** (`image-format`, not `image_format`) —
   confirmed live via the parse round-trip in this stage's tests; no
   quoting (`"image-format" = ...`) is needed or used anywhere in the
   schema above.
