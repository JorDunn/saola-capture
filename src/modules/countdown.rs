//! The delayed-capture countdown pill (PLAN.md Stage 8, task 2): a small
//! floating ink card, centred on the output, counting whole seconds down to
//! zero before a delayed `shot` (any of the three kinds — fullscreen,
//! region, window) actually captures.
//!
//! # Why this exists at all
//!
//! Before Stage 8, `--delay N` was honoured as a plain
//! `std::thread::sleep` deep inside `capture::freeze_focused_output` /
//! `capture::take_screenshot`, invisible to the user — the screen just sits
//! there for N seconds with no feedback that anything is happening at all,
//! which is exactly the "looks like nothing, isn't" shape CLAUDE.md's
//! no-panic rule warns about (there the failure mode is a crash; here it's
//! a user who assumes the keybind didn't register and presses it again).
//! This module is the visible half of that wait. The *sleep itself* is
//! unchanged and still lives in `capture/mod.rs` — this module never
//! delays anything on its own, it only shows that a delay already in
//! progress elsewhere is counting down.
//!
//! # Why a fourth surface lifecycle, and which one it is
//!
//! `main.rs`'s `SurfaceRole` doc comment names three lifecycles the flash,
//! toast and overlay established (permanent, respawn-to-resize, reactive).
//! The countdown is the **reactive** shape (spawned on demand, torn down
//! the moment it's done) — same category as the overlay, not the flash.
//! It could *not* use the flash's boot-time pre-warm trick even if it
//! wanted to: unlike the flash (opacity-only, always harmless to leave
//! mapped) the countdown's content changes over its own lifetime and it has
//! nothing to be idle-and-invisible *as* between captures, so pre-warming
//! it would just be an always-visible pill nobody asked for.
//!
//! Unlike the overlay, though, the countdown's reactive-spawn latency risk
//! (Stage 6's bug #1 — "an event arrives" to "a pixel is composited" crosses
//! several scheduler hops) is **not** a serious concern here: `--delay` is
//! specified in whole seconds and the shortest meaningful one is 1s, an
//! order of magnitude longer than the ~450-560 ms the overlay measured for
//! its own first-frame latency (CAPTURE-RESEARCH-adjacent, Stage 7's
//! handoff). A `--delay 1` might show its "1" a little late; it will not
//! fail to show up at all the way the flash's ~140 ms fade could.
//!
//! # Time is injected, never read (teaching note, following
//! `modules::flash`)
//!
//! [`Countdown::is_active`]/[`Countdown::remaining_secs`] take `now:
//! Instant` rather than reading the clock themselves, so the countdown math
//! is testable in microseconds instead of by sleeping — `main.rs` is the
//! one place that reads `Instant::now()`, exactly like `modules::flash`.

use std::time::{Duration, Instant};

use iced::widget::{container, text};
use iced::{Element, Length, Subscription};
use saola_theme::{ColorExt, Theme};

/// How often the countdown redraws while active. The numeral itself only
/// needs to change once a second, but ticking a little faster keeps the
/// surface's very first frame from waiting up to a full second to appear —
/// otherwise a `--delay 1` could plausibly render its "1" for a few hundred
/// milliseconds and then vanish having shown nothing at all.
const TICK: Duration = Duration::from_millis(100);

/// The delayed-capture countdown's whole state: when it started and for how
/// long. `None` (idle) means no surface should be mapped — the same
/// `Option`-as-idle-flag shape `modules::flash::Flash` uses.
#[derive(Debug, Default, Clone, Copy)]
pub struct Countdown {
    /// `(started, total)`.
    running: Option<(Instant, Duration)>,
}

impl Countdown {
    /// Start (or restart) a `total`-second countdown, timed from `now`.
    /// Called once per delayed shot — see `dbus.rs`'s
    /// `DaemonEvent::CountdownStarted` and `main.rs`'s
    /// `Message::CountdownStarted` arm. Only ever called with `total >
    /// Duration::ZERO` in practice (`dbus.rs` only sends the event when
    /// `options.delay > 0`), but a zero-length countdown is handled the
    /// same as any other rather than special-cased — [`Self::is_active`]
    /// is simply `false` for it on the very next check, which is the
    /// correct answer.
    pub fn trigger(&mut self, total: Duration, now: Instant) {
        self.running = Some((now, total));
    }

    /// Whether a countdown surface should be mapped right now — `main.rs`
    /// calls this both to decide whether to spawn the surface and, on every
    /// later tick, whether to tear it down.
    pub fn is_active(&self, now: Instant) -> bool {
        match self.running {
            Some((started, total)) => now.saturating_duration_since(started) < total,
            None => false,
        }
    }

    /// Whole seconds remaining, rounded **up**. The number a user watching
    /// a countdown wants is "how many more whole seconds", so 2.3 s
    /// remaining reads "3", not "2" — a countdown that shows "0" while the
    /// shutter still hasn't fired reads as broken, and one that shows "1"
    /// a beat too early is unremarkable.
    pub fn remaining_secs(&self, now: Instant) -> u32 {
        let Some((started, total)) = self.running else {
            return 0;
        };
        let elapsed = now.saturating_duration_since(started);
        if elapsed >= total {
            return 0;
        }
        let remaining = total - elapsed;
        let remaining_nanos =
            u128::from(remaining.as_secs()) * 1_000_000_000 + u128::from(remaining.subsec_nanos());
        // Ceil-divide whole seconds without a floating-point round.
        let whole_seconds = remaining_nanos.div_ceil(1_000_000_000);
        u32::try_from(whole_seconds).unwrap_or(u32::MAX)
    }

    /// Ticks only while counting down — the same gated-subscription shape
    /// `modules::flash::Flash::subscription` uses, so a daemon with no
    /// delayed shot in flight burns zero timer wakeups.
    pub fn subscription(&self, now: Instant) -> Subscription<Message> {
        if self.is_active(now) {
            iced::time::every(TICK).map(|_instant| Message::Tick)
        } else {
            Subscription::none()
        }
    }

    /// The whole surface: one ink pill, centred on the output, holding the
    /// remaining whole seconds in tabular numerals (CLAUDE.md's design
    /// language: "Size/duration readouts use tabular numerals"). No
    /// interactive widgets — click-through is enforced at the layer-shell
    /// level (`main.rs`'s `countdown_surface_settings`,
    /// `events_transparent: true`), matching the flash's belt-and-braces
    /// posture rather than trusting one layer alone.
    pub fn view(&self, theme: &Theme, now: Instant) -> Element<'static, Message> {
        let seconds = self.remaining_secs(now);

        let pill = container(
            text(seconds.to_string())
                .font(saola_theme::convert::ui_font(theme))
                .size(theme.typography.size.dialog_title)
                .color(theme.on_ink.primary.into_iced()),
        )
        .width(Length::Fixed(theme.sizes.hit_target_touch * 1.5))
        .height(Length::Fixed(theme.sizes.hit_target_touch))
        .align_x(iced::Center)
        .align_y(iced::Center)
        .style(saola_theme::style::container::popover(theme));

        container(pill)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::Center)
            .align_y(iced::Center)
            .into()
    }
}

/// One frame of the countdown. Carries no data — `main.rs` re-reads
/// `Instant::now()` on receipt rather than trusting the tick's own
/// timestamp, exactly like `modules::flash::Message::Tick`.
#[derive(Debug, Clone, Copy)]
pub enum Message {
    Tick,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_countdown_is_never_active_and_has_nothing_remaining() {
        let countdown = Countdown::default();
        let now = Instant::now();
        assert!(!countdown.is_active(now));
        assert_eq!(countdown.remaining_secs(now), 0);
    }

    #[test]
    fn triggering_starts_the_countdown_at_the_full_duration() {
        let mut countdown = Countdown::default();
        let now = Instant::now();
        countdown.trigger(Duration::from_secs(3), now);
        assert!(countdown.is_active(now));
        assert_eq!(countdown.remaining_secs(now), 3);
    }

    #[test]
    fn remaining_seconds_rounds_up_to_the_next_whole_second() {
        let mut countdown = Countdown::default();
        let now = Instant::now();
        countdown.trigger(Duration::from_secs(3), now);

        // 2.4s left of a 3s countdown still reads "3" until the clock
        // actually crosses the 1s mark.
        assert_eq!(
            countdown.remaining_secs(now + Duration::from_millis(600)),
            3
        );
        assert_eq!(
            countdown.remaining_secs(now + Duration::from_millis(1000)),
            2
        );
        assert_eq!(
            countdown.remaining_secs(now + Duration::from_millis(2999)),
            1
        );
    }

    #[test]
    fn the_countdown_becomes_inactive_exactly_at_its_total_duration() {
        let mut countdown = Countdown::default();
        let now = Instant::now();
        let total = Duration::from_secs(2);
        countdown.trigger(total, now);

        assert!(countdown.is_active(now + total - Duration::from_millis(1)));
        assert!(!countdown.is_active(now + total));
        assert_eq!(countdown.remaining_secs(now + total), 0);
        // Well past the total, still zero and still inactive — never
        // resurrects on its own.
        assert!(!countdown.is_active(now + total * 10));
        assert_eq!(countdown.remaining_secs(now + total * 10), 0);
    }

    #[test]
    fn retriggering_restarts_the_countdown_from_the_new_total() {
        let mut countdown = Countdown::default();
        let now = Instant::now();
        countdown.trigger(Duration::from_secs(5), now);

        let later = now + Duration::from_secs(2);
        countdown.trigger(Duration::from_secs(3), later);

        assert_eq!(
            countdown.remaining_secs(later),
            3,
            "restarted from the new total, not continuing the old one"
        );
        assert!(countdown.is_active(later + Duration::from_secs(2)));
        assert!(!countdown.is_active(later + Duration::from_secs(3)));
    }

    #[test]
    fn a_now_before_started_clamps_instead_of_underflowing() {
        let mut countdown = Countdown::default();
        let now = Instant::now();
        countdown.trigger(Duration::from_secs(4), now);
        // `saturating_duration_since` makes this `Duration::ZERO`, not a
        // panic — belt-and-braces against a clock that could somehow move
        // backward between trigger and tick, the same guard
        // `modules::flash::Flash::opacity` carries for the same reason.
        assert_eq!(countdown.remaining_secs(now), 4);
    }
}
