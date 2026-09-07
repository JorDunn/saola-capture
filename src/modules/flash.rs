//! The camera-flash overlay: a full-output, click-through ivory flash that
//! begins fading the instant a screenshot is taken (PLAN.md Stage 6, task 1
//! — the shutter feedback the Context section's "macOS-esque capture
//! experience" describes).
//!
//! # Why this is a fade-*out*, not a flash-then-fade (teaching note)
//!
//! A real camera flash fires *before* the shutter. This one can't: the
//! Stage 5 handoff is explicit that the flash must not be *visible* while a
//! capture is in flight, because screencopy composites layer-shell surfaces
//! (`docs/CAPTURE-RESEARCH.md` §1.5) — a flash showing when the capture runs
//! would show up *in* the screenshot. So the sequence `main.rs` drives is:
//! capture → save → [`Flash::trigger`] jumps to full ivory opacity → fade to
//! nothing. Perceptually this still reads as "the screen just flashed", even
//! though causally the capture already happened — same trick a physical
//! camera's LCD "flash" review does.
//!
//! # The surface is permanently mapped, not spawned per capture
//!
//! Unlike the toast, the flash's layer-shell surface is spawned exactly
//! once, at daemon boot (`main.rs::Daemon::boot`), and never torn down —
//! `main.rs::flash_surface_settings` explains the live nested-niri finding
//! that made a per-capture spawn/unmap the wrong shape (a brand-new
//! surface's Wayland configure round trip plus first `wgpu` frame can
//! easily eat the whole ~150 ms fade budget before anything is ever
//! composited). This module doesn't know or care either way — [`Flash`] is
//! just a fade-opacity state machine; `main.rs` decides what to do with a
//! surface.
//!
//! # Time is injected, never read (teaching note, following
//! `saola-lockscreen::modules::reveal`)
//!
//! [`Flash::is_active`] and [`Flash::opacity`] take `now: Instant` as a
//! parameter rather than calling `Instant::now()` themselves, so the fade
//! curve is testable in microseconds instead of by sleeping. `main.rs` is
//! the one place that reads the real clock — once when a `CaptureTaken`
//! event triggers the flash, and again on every [`Message::Tick`].
//!
//! # The saola-theme gap this hit, upstreamed at v0.15.0 (2026-09-06)
//!
//! `saola-theme` v0.5.0's `Motion` token group had no dedicated "flash"/
//! "shutter" duration (`hover`, `popover`, `wake`, `toast_*`, `breathe` —
//! no plain fade), so [`fade`] used to reuse `motion.hover` (140 ms) as the
//! closest existing family for a bare colour/opacity transition. v0.15.0
//! adds `motion.flash` (150 ms — the style guide's own "~150 ms", not
//! `hover`'s borrowed 140), and [`fade`] reads that directly now.

use std::time::{Duration, Instant};

use iced::widget::{container, Space};
use iced::{Element, Length, Subscription};
use saola_theme::{ColorExt, Theme};

/// How often the fade re-renders while active. 16 ms is close to one frame
/// at 60 Hz — smooth enough for a ~150 ms fade without a real
/// animation-frame API (iced 0.14 has none; `iced::time::every` is the
/// sanctioned polling exception the panel's `claude`/`window_title`
/// modules already use for exactly this kind of short, gated animation).
const TICK: Duration = Duration::from_millis(16);

/// The fade's duration, sourced from the theme — `motion.flash` as of
/// saola-theme v0.15.0 (was `motion.hover`; see this module's doc comment).
pub fn fade(theme: &Theme) -> Duration {
    Duration::from_millis(theme.motion.flash.into())
}

/// One flash's fade state. `None` (idle) means no surface should be
/// mapped; `Some(started)` means the surface is up and fading, timed from
/// `started`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Flash {
    started: Option<Instant>,
}

impl Flash {
    /// Start (or restart) the flash at full ivory opacity. Called once per
    /// successful screenshot — see `main.rs`'s `Message::CaptureTaken` arm.
    pub fn trigger(&mut self, now: Instant) {
        self.started = Some(now);
    }

    /// Whether a flash surface should be mapped right now. `main.rs` calls
    /// this both to decide whether to spawn the surface on trigger and to
    /// decide whether to tear it down on the next tick.
    pub fn is_active(&self, now: Instant, fade: Duration) -> bool {
        match self.started {
            Some(started) => now.saturating_duration_since(started) < fade,
            None => false,
        }
    }

    /// The ivory fill's opacity: `1.0` the instant the flash starts,
    /// linearly down to `0.0` at `fade`. Never negative, never above `1.0`
    /// — a `now` before `started` (unreachable in practice, since
    /// `main.rs` always triggers and ticks off the same clock, but the
    /// no-panic rule doesn't get to assume that) clamps rather than
    /// underflowing a `Duration` subtraction.
    pub fn opacity(&self, now: Instant, fade: Duration) -> f32 {
        let Some(started) = self.started else {
            return 0.0;
        };
        let elapsed = now.saturating_duration_since(started);
        if elapsed >= fade {
            return 0.0;
        }
        let fade_secs = fade.as_secs_f32();
        if fade_secs <= 0.0 {
            return 0.0;
        }
        (1.0 - elapsed.as_secs_f32() / fade_secs).clamp(0.0, 1.0)
    }

    /// Ticks only while the fade is running — the same gated-subscription
    /// shape `saola-lockscreen::modules::reveal` and the panel's
    /// `window_title`/`claude` modules use, so a daemon with no recent
    /// screenshot burns zero timer wakeups.
    pub fn subscription(&self, now: Instant, fade: Duration) -> Subscription<Message> {
        if self.is_active(now, fade) {
            // The tick's own `Instant` is discarded (`main.rs` re-reads
            // `Instant::now()` fresh at dispatch time instead — see
            // `Message`'s doc comment) — the same "Tick carries nothing,
            // its only job is to wake the runtime into rendering again"
            // shape `saola-panel::modules::clock::Message::Tick` uses,
            // rather than `window_title`/`claude`'s `Tick(Instant)` (which
            // *do* read their payload, for marquee/breath progress with no
            // externally-injected `now`).
            iced::time::every(TICK).map(|_instant| Message::Tick)
        } else {
            Subscription::none()
        }
    }

    /// The whole surface: an ivory fill at [`Self::opacity`]. No
    /// interactive widgets — click-through is enforced at the layer-shell
    /// level (`main.rs`'s `flash_surface_settings`, `events_transparent:
    /// true`); this `view` having nothing to click is the belt to that
    /// braces.
    pub fn view(&self, theme: &Theme, now: Instant, fade: Duration) -> Element<'static, Message> {
        let opacity = self.opacity(now, fade);
        let mut ivory = theme.palette.paper.into_iced();
        ivory.a = opacity;
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(ivory)),
                ..container::Style::default()
            })
            .into()
    }
}

/// One frame of the fade. Carries no data — `main.rs` re-reads
/// `Instant::now()` on receipt rather than trusting the tick's own
/// timestamp, so the tick's only job is to wake `Daemon::update` into
/// re-evaluating [`Flash::is_active`] (the same shape
/// `saola-panel::modules::clock::Message::Tick` uses).
#[derive(Debug, Clone, Copy)]
pub enum Message {
    Tick,
}

#[cfg(test)]
mod tests {
    use super::*;

    const FADE: Duration = Duration::from_millis(140);

    #[test]
    fn idle_flash_is_never_active_and_fully_transparent() {
        let flash = Flash::default();
        let now = Instant::now();
        assert!(!flash.is_active(now, FADE));
        assert_eq!(flash.opacity(now, FADE), 0.0);
    }

    #[test]
    fn triggering_starts_at_full_opacity() {
        let mut flash = Flash::default();
        let now = Instant::now();
        flash.trigger(now);
        assert!(flash.is_active(now, FADE));
        assert_eq!(flash.opacity(now, FADE), 1.0);
    }

    #[test]
    fn opacity_fades_linearly_to_zero() {
        let mut flash = Flash::default();
        let now = Instant::now();
        flash.trigger(now);

        let half = flash.opacity(now + FADE / 2, FADE);
        assert!((half - 0.5).abs() < 0.01, "got {half}");

        let three_quarters = flash.opacity(now + FADE * 3 / 4, FADE);
        assert!((three_quarters - 0.25).abs() < 0.01, "got {three_quarters}");
    }

    #[test]
    fn the_flash_becomes_inactive_exactly_at_fade_and_opacity_hits_zero() {
        let mut flash = Flash::default();
        let now = Instant::now();
        flash.trigger(now);

        assert!(flash.is_active(now + FADE - Duration::from_millis(1), FADE));
        assert!(!flash.is_active(now + FADE, FADE));
        assert_eq!(flash.opacity(now + FADE, FADE), 0.0);
        // Well past the fade, still zero — never negative, never resurrects.
        assert_eq!(flash.opacity(now + FADE * 10, FADE), 0.0);
    }

    #[test]
    fn retriggering_restarts_the_fade_from_full_opacity() {
        let mut flash = Flash::default();
        let now = Instant::now();
        flash.trigger(now);
        let later = now + FADE / 2;
        flash.trigger(later);

        assert_eq!(flash.opacity(later, FADE), 1.0, "restarted, not continued");
        assert!(flash.is_active(later + FADE - Duration::from_millis(1), FADE));
    }

    /// A flash that has fully faded stays inert indefinitely — no
    /// `dismiss()`/reset call is needed (there isn't one — see this
    /// module's doc comment on why the surface is now permanently mapped):
    /// `is_active`/`opacity` must keep answering "idle" no matter how much
    /// wall-clock time passes after `started`, since `main.rs` never clears
    /// `started` back to `None` between captures.
    #[test]
    fn a_long_faded_flash_stays_inert_without_ever_being_reset() {
        let mut flash = Flash::default();
        let now = Instant::now();
        flash.trigger(now);

        let far_future = now + FADE * 1000;
        assert!(!flash.is_active(far_future, FADE));
        assert_eq!(flash.opacity(far_future, FADE), 0.0);
    }

    #[test]
    fn a_now_before_started_clamps_to_full_opacity_instead_of_underflowing() {
        let mut flash = Flash::default();
        let now = Instant::now();
        flash.trigger(now);
        // `saturating_duration_since` makes this `Duration::ZERO`, not a
        // panic — belt-and-braces against a clock that could somehow move
        // backward between trigger and tick.
        assert_eq!(flash.opacity(now, FADE), 1.0);
    }

    #[test]
    fn fade_reads_the_flash_token() {
        let theme = Theme::saola();
        assert_eq!(
            fade(&theme),
            Duration::from_millis(theme.motion.flash.into())
        );
    }
}
