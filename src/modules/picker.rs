//! `PickColor` (PLAN.md Stage 16): the `pick-color` CLI verb and the app
//! window's "Pick Color" button both end here, through
//! `io.saola.Capture1`'s `PickColor` method (`dbus.rs::CaptureService::
//! pick_color`) — this module is that method's *implementation*, exactly as
//! `dbus.rs`'s own doc comment already promised ("Stage 16 wires this to
//! `modules/picker.rs`").
//!
//! # Not this crate's `AnnotationColor` (teaching note, and a scope note the
//! # Stage 15 handoff already flagged)
//!
//! "Color picker" is an overloaded phrase in this codebase. This module is
//! about *sampling a pixel from the screen* — niri's own
//! `org.gnome.Shell.Screenshot.PickColor`, a system eyedropper. It has
//! nothing to do with `modules::editor::AnnotationColor`, the three-color
//! terracotta/ink/ivory palette an annotation tool draws with. The two never
//! call into each other.
//!
//! # The `a{sv}` quirk (a real correction to `dbus.rs`'s own stub doc
//! # comment, found by reading the pre-plan probe evidence rather than
//! # assuming)
//!
//! `dbus.rs`'s `CaptureService::pick_color` stub was documented as
//! `PickColor() -> (ddd)` — a plain 3-tuple of doubles — matching this
//! crate's *own* `io.saola.Capture1` method signature. But **niri's**
//! `org.gnome.Shell.Screenshot.PickColor` does not return a bare `(ddd)`;
//! `docs/research/2026-08-08-probes/input-tests/NOTES.md` (Test 4) recorded
//! the real wire reply: `a{sv} 1 "color" (ddd) 0.215686 0.215686 0.215686` —
//! a one-entry **dictionary** whose `"color"` key holds the `(ddd)` variant,
//! not the tuple directly. Decoding this as `(f64, f64, f64)` would fail
//! every call (a signature mismatch at the D-Bus layer, not a Rust type
//! error) — [`ShellScreenshotProxy::pick_color`] below declares the real
//! `a{sv}` shape, and [`extract_rgb`] is the one place that unwraps it. This
//! crate's *own* `io.saola.Capture1::PickColor() -> (ddd)` is unaffected —
//! it is a different interface with a different (simpler, deliberately
//! chosen) contract; only the niri-facing proxy needed the correction.
//!
//! # Where the swatch toast and the clipboard copy actually happen
//!
//! Both live in `dbus.rs::CaptureService::pick_color`, not here — this
//! module's job stops at "ask niri, get back three doubles, or a hex
//! string". `pick_color` (the async orchestration below) is a thin,
//! testable-by-inspection wrapper around one D-Bus round trip;
//! [`rgb_to_hex`] and [`extract_rgb`] are the two pure functions worth
//! unit-testing directly.

use std::collections::HashMap;
use std::fmt;

use zbus::zvariant::OwnedValue;
use zbus::Connection;

// ---------------------------------------------------------------------
// niri's org.gnome.Shell.Screenshot — client proxy
// ---------------------------------------------------------------------

/// Mirrors `capture::screencast`'s own teaching note on `#[zbus::proxy]`:
/// this generates a client (`ShellScreenshotProxy`), the trait itself is
/// never constructed directly. Kept private — nothing outside this module
/// should reach past [`pick_color`] to drive the Shell proxy by hand.
#[zbus::proxy(
    interface = "org.gnome.Shell.Screenshot",
    default_service = "org.gnome.Shell.Screenshot",
    default_path = "/org/gnome/Shell/Screenshot"
)]
trait ShellScreenshot {
    /// `PickColor() -> a{sv}` — **not** `(ddd)`; see this module's doc
    /// comment on the quirk. Blocks in the compositor until the user clicks
    /// (or, per CAPTURE-RESEARCH-style live evidence, presumably Escape —
    /// untested here; see the handoff for what a human should verify) —
    /// `zbus`'s default `method_timeout` is `None`, matching every other
    /// "this may legitimately take a while" call in this crate
    /// (`Screenshot`'s interactive region, `StopRecording`).
    fn pick_color(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
}

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

/// Everything that can go wrong between "PickColor was called" and "we have
/// three doubles". Every variant's `Display` names something actionable —
/// this ends up in a D-Bus `Error` reply and (via `dbus.rs`) potentially a
/// toast, so CLAUDE.md's "absent services produce actionable errors" rule
/// applies here exactly as it does to a missing ffmpeg.
#[derive(Debug)]
pub enum PickColorError {
    /// The D-Bus call itself failed — most likely because
    /// `org.gnome.Shell.Screenshot` isn't being served (a non-niri
    /// compositor, or a niri build without it). CAPTURE-RESEARCH's probes
    /// confirm niri 26.04 serves this itself; a future compositor swap is
    /// exactly the kind of portability CLAUDE.md's Boundaries assign to the
    /// `CaptureBackend` trait, which this D-Bus call deliberately sits
    /// outside of (there is no screen-color-sampling method on that trait
    /// today — recorded as a gap for whoever needs it next, not silently
    /// worked around here).
    Call(zbus::Error),
    /// The reply had no `"color"` key at all — a compositor claiming this
    /// interface but not answering it the documented way.
    MissingColor,
    /// The `"color"` value was present but not the `(ddd)` shape.
    Decode(zbus::zvariant::Error),
}

impl fmt::Display for PickColorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PickColorError::Call(err) => write!(
                f,
                "could not reach org.gnome.Shell.Screenshot's PickColor — is niri (or another \
                 compositor serving that interface) running? ({err})"
            ),
            PickColorError::MissingColor => write!(
                f,
                "org.gnome.Shell.Screenshot.PickColor replied with no \"color\" entry"
            ),
            PickColorError::Decode(err) => write!(
                f,
                "org.gnome.Shell.Screenshot.PickColor's \"color\" entry was not the expected \
                 (double, double, double) — {err}"
            ),
        }
    }
}

impl std::error::Error for PickColorError {}

// ---------------------------------------------------------------------
// The pick itself
// ---------------------------------------------------------------------

/// Calls niri's `PickColor` over `connection` and returns `(r, g, b)`, each
/// `0.0..=1.0` — CAPTURE-RESEARCH's own probe evidence for the shape.
///
/// Blocking, in the D-Bus sense, not the OS-thread sense: this `.await`s a
/// method call that does not return until the user clicks (or cancels) —
/// `dbus.rs::CaptureService::pick_color` calls this directly from its own
/// `async fn`, the same "a method may legitimately take a while" posture
/// `Screenshot`'s interactive region and `StopRecording` already established,
/// and for the same reason it is safe: zbus dispatches each served method on
/// its own task, so nothing else on `io.saola.Capture1` blocks meanwhile.
pub async fn pick_color(connection: &Connection) -> Result<(f64, f64, f64), PickColorError> {
    let proxy = ShellScreenshotProxy::new(connection)
        .await
        .map_err(PickColorError::Call)?;
    let reply = proxy.pick_color().await.map_err(PickColorError::Call)?;
    extract_rgb(&reply)
}

/// The pure half of [`pick_color`]: given the already-received `a{sv}` reply,
/// pull out `"color"` and decode it as `(f64, f64, f64)`. Split out
/// specifically so it can be unit-tested without a real bus — building the
/// `HashMap<String, OwnedValue>` a real `PickColor` reply would contain is
/// cheap and needs no compositor.
fn extract_rgb(reply: &HashMap<String, OwnedValue>) -> Result<(f64, f64, f64), PickColorError> {
    let color = reply
        .get("color")
        .cloned()
        .ok_or(PickColorError::MissingColor)?;
    <(f64, f64, f64)>::try_from(color).map_err(PickColorError::Decode)
}

// ---------------------------------------------------------------------
// Hex formatting
// ---------------------------------------------------------------------

/// `(r, g, b)` in `0.0..=1.0` to `#RRGGBB`, uppercase — the swatch toast's
/// body text and what actually lands on the clipboard.
///
/// Clamped before the cast to `u8`: floating-point noise at the 0.0/1.0
/// boundary (or, in principle, a misbehaving compositor) rounds to a valid
/// byte instead of wrapping. Moved here from `main.rs` in Stage 16 — that
/// module's own doc comment on `run_pick_color` said as much ahead of time
/// ("kept real ... so Stage 16 only has to delete the `Err` short-circuit in
/// `dbus.rs`, not write this conversion"); it is now the one definition both
/// the CLI's stdout formatting and the daemon's swatch toast/clipboard copy
/// share, rather than two copies that could drift.
pub fn rgb_to_hex(r: f64, g: f64, b: f64) -> String {
    let byte = |c: f64| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", byte(r), byte(g), byte(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- rgb_to_hex ----------------------------------------------------

    #[test]
    fn rgb_to_hex_formats_black_white_and_a_measured_swatch() {
        assert_eq!(rgb_to_hex(0.0, 0.0, 0.0), "#000000");
        assert_eq!(rgb_to_hex(1.0, 1.0, 1.0), "#FFFFFF");
        // The exact value CAPTURE-RESEARCH's probe measured and verified
        // against a live `grim` sample: 0.215686 * 255 rounds to 55 = 0x37.
        assert_eq!(rgb_to_hex(0.215_686, 0.215_686, 0.215_686), "#373737");
    }

    #[test]
    fn rgb_to_hex_clamps_out_of_range_noise() {
        assert_eq!(rgb_to_hex(-0.01, 1.5, 0.5), "#00FF80");
    }

    #[test]
    fn rgb_to_hex_rounds_rather_than_truncates() {
        // 0.5 * 255 = 127.5, which rounds to 128 (0x80), not 127 (0x7F).
        assert_eq!(rgb_to_hex(0.5, 0.0, 0.0), "#800000");
    }

    // -- extract_rgb -----------------------------------------------------

    fn color_reply(r: f64, g: f64, b: f64) -> HashMap<String, OwnedValue> {
        let mut map = HashMap::new();
        let value = zbus::zvariant::Value::from(zbus::zvariant::Structure::from((r, g, b)));
        let owned = OwnedValue::try_from(value).expect("a plain (ddd) structure always converts");
        map.insert("color".to_string(), owned);
        map
    }

    #[test]
    fn extract_rgb_reads_the_color_key_out_of_the_a_sv_reply() {
        let reply = color_reply(0.215_686, 0.4, 0.9);
        let (r, g, b) = extract_rgb(&reply).expect("a well-formed reply decodes");
        assert!((r - 0.215_686).abs() < 1e-9);
        assert!((g - 0.4).abs() < 1e-9);
        assert!((b - 0.9).abs() < 1e-9);
    }

    #[test]
    fn extract_rgb_reports_a_missing_color_key() {
        let reply: HashMap<String, OwnedValue> = HashMap::new();
        let err = extract_rgb(&reply).expect_err("no \"color\" key at all");
        assert!(matches!(err, PickColorError::MissingColor));
    }

    #[test]
    fn extract_rgb_reports_a_wrong_shaped_color_value() {
        let mut reply = HashMap::new();
        reply.insert("color".to_string(), OwnedValue::from(42u32));
        let err = extract_rgb(&reply).expect_err("a u32 is not a (ddd) structure");
        assert!(matches!(err, PickColorError::Decode(_)));
    }

    #[test]
    fn pick_color_error_messages_are_actionable() {
        // No live bus needed — this only exercises `Display`, matching this
        // crate's "every error string names something the human can do"
        // testing posture elsewhere (`encode::EncodeError`'s own doc comment).
        let missing = PickColorError::MissingColor.to_string();
        assert!(missing.contains("PickColor"));
    }
}
