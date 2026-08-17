//! The toast stack: the §6 notification card, verbatim, animating §5's
//! exact timing (PLAN.md Stage 6, task 2) — 440 px ink card, 26 px radius,
//! a thumbnail in the 36 px icon tile, a 3 px terracotta life rule counting
//! the card down, stack of at most 3 (the fourth replaces the oldest),
//! hover pauses. Click spawns detached `saola-capture window edit <path>`
//! (Stage 9's window process is still a stub — it prints and exits 0 — so
//! this call site only has to *ask*, not depend on what answers).
//!
//! # Time is injected, never read (teaching note, following
//! `saola-lockscreen::modules::reveal` and `modules::flash`)
//!
//! Every duration in this module comes from `theme.motion.toast_*`, and
//! every `now` is a parameter, never `Instant::now()` called internally —
//! see [`Toast::elapsed`]/[`ToastStack::update`]. `main.rs` reads the real
//! clock exactly twice: once when a `CaptureTaken` event pushes a new
//! toast, and again on every [`Message::Tick`].
//!
//! # The pausable stopwatch (teaching note)
//!
//! "Hover pauses" (§5) is implemented as a stopwatch that can be paused and
//! resumed rather than as a special case in the expiry check: each
//! [`Toast`] holds `elapsed_at_last_change` (the frozen total from every
//! *previous* running interval) plus `resumed_at` (`Some(when the current
//! interval started)`, or `None` while paused). [`Toast::elapsed`] is then
//! just "the frozen total, plus however long the current interval has run"
//! — and because a paused toast's `elapsed` stops advancing on its own,
//! [`ToastStack::update`]'s `Tick` arm (which drops any toast whose
//! `elapsed` has passed `motion.toast_total`) *automatically* never expires
//! a hovered card, with no extra branch to keep in sync.
//!
//! Hover pauses **per card**, not the whole stack: the style guide's "hover
//! pauses both [the auto-dismiss and the life rule]" reads naturally as "the
//! one you're pointing at", and per-card pause is what a `MouseArea` wrapped
//! around one card can express without the stack knowing which card is
//! "the" one — see [`Message::Hovered`]/[`Message::Unhovered`].
//!
//! # The slide-in, without a transform (teaching note)
//!
//! §5's "slide in from the right edge (`translateX(120% → 0)`)" has no
//! direct iced 0.14 equivalent — there is no subtree transform. [`phase`]
//! instead returns a **leading spacer width** that shrinks from a full card
//! width down to `0` over `motion.toast_in`: the toast surface itself is
//! declared exactly [`Theme::sizes::notification_card_width`] wide (see
//! `main.rs`'s `toast_surface_settings`), so a spacer that wide pushes the
//! card entirely past the surface's own right edge — which, because a
//! layer-shell surface has no canvas beyond its own negotiated pixel size,
//! is indistinguishable from "off-screen". As the spacer shrinks to `0` the
//! card slides into (and stays in) view. Same story for the fade: iced 0.14
//! has no subtree opacity, so [`card_view`] scales the *alpha channel* of
//! every color it draws (background, text, life rule, shadow) by the
//! phase's `alpha` instead of wrapping the card in an opacity widget.
//!
//! # The saola-theme gaps this hit
//!
//! Two, both flagged for a future tag bump rather than restyled locally
//! (CLAUDE.md Design language):
//!
//! - **No opaque-ink "card" style helper.** `saola_theme::style::container::card`
//!   exists but does the *opposite* of what §6 wants here: its `Surface::Ink`
//!   arm paints an **ivory** card (for ivory content floating on an ink
//!   shell — its own doc comment says as much), while §6's notification card
//!   is itself solid ink with ivory text. [`ink_card_style`] below composes
//!   the right thing from tokens that do exist (`palette.ink`,
//!   `on_ink.primary`, `radii.card`, `shadows.popover`) — the same
//!   "derive locally, don't restyle" posture `saola-lockscreen::modules::reveal`
//!   used for its own three missing style helpers. (`shadows.popover`'s
//!   `0 18px 48px rgba(12,10,0,.5)` is, gratifyingly, *exactly* §6's spec
//!   value — no gap there.)
//! - **No `icon_tile` size token.** §6 wants a "36px icon tile"; `Sizes` has
//!   no field for it (`icon_bare` is 32–34 for the power menu, `list_row` is
//!   38 — neither is it). [`ICON_TILE_SIZE`] is the spec's literal value,
//!   named and documented rather than inlined as a bare number at each use
//!   site.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::widget::{column, container, image, mouse_area, row, text, Space};
use iced::{Center, Element, Length, Subscription};
use saola_theme::{ColorExt, ShadowExt, Theme};

use crate::capture::Frame;

/// How often the stack re-renders while at least one toast is up. Coarser
/// than [`crate::modules::flash::Flash`]'s 16 ms — the life rule drains
/// over up to 6.35 s, so ~30 fps reads just as smooth and costs less than
/// half the wakeups.
const TICK: Duration = Duration::from_millis(32);

/// §6's "36px icon tile" — no `Sizes` field for it exists in saola-theme
/// v0.5.0; see this module's doc comment for the gap note.
const ICON_TILE_SIZE: f32 = 36.0;

/// §6's "3px life rule" — same gap-note posture as [`ICON_TILE_SIZE`]; no
/// dedicated rule-thickness token exists (`sizes.window_border`, at 2px, is
/// the closest and is for a different purpose).
const LIFE_RULE_HEIGHT: f32 = 3.0;

// ---------------------------------------------------------------------
// Thumbnails
// ---------------------------------------------------------------------

/// Builds a small RGBA thumbnail [`iced::widget::image::Handle`] from a
/// captured [`Frame`], for the toast's icon tile.
///
/// **Deliberately `Handle::from_rgba`, never `from_path`/`from_bytes`.**
/// `saola-lockscreen::wallpaper`'s module doc comment has the full,
/// live-verified account: `iced_wgpu`'s image cache decodes `Path`/`Bytes`
/// handles on a background worker and draws nothing the first frame (and,
/// for large buffers, uploads to the GPU asynchronously too), while
/// `Handle::Rgba` is the one variant it resolves synchronously. This
/// daemon's toast surface redraws on every life-rule tick (unlike the
/// lockscreen's near-static one), so the risk of a blank first frame is
/// much smaller here — but there's no reason to reintroduce it when the
/// pixels are already in hand as a plain `Frame` (see `dbus.rs`'s
/// `CaptureService::screenshot`, which calls this before the `Frame` is
/// dropped).
///
/// Downsampled with simple nearest-neighbor sampling first: a full
/// screenshot frame can be tens of megabytes of RGBA, and every byte of it
/// would otherwise ride into the GPU's image cache to be displayed at
/// `ICON_TILE_SIZE`. `max_dim` bounds the *longer* side; the shorter side
/// scales to match, so the thumbnail keeps the screenshot's aspect ratio
/// (a centre-crop-to-square would distort or lose content instead).
pub fn thumbnail_handle(frame: &Frame, max_dim: u32) -> image::Handle {
    let (src_w, src_h) = (frame.width(), frame.height());
    let longest = src_w.max(src_h).max(1);
    let scale = (max_dim as f32 / longest as f32).min(1.0);
    let dst_w = ((src_w as f32 * scale).round() as u32).max(1);
    let dst_h = ((src_h as f32 * scale).round() as u32).max(1);

    let stride = frame.stride();
    let pixels = frame.pixels();
    let mut out = Vec::with_capacity((dst_w * dst_h * 4) as usize);
    for y in 0..dst_h {
        let src_y = (y * src_h / dst_h).min(src_h.saturating_sub(1));
        for x in 0..dst_w {
            let src_x = (x * src_w / dst_w).min(src_w.saturating_sub(1));
            let start = src_y as usize * stride + src_x as usize * 4;
            match pixels.get(start..start + 4) {
                Some(px) => out.extend_from_slice(px),
                // Unreachable in practice (`src_x`/`src_y` are always
                // in-bounds by construction above), but the no-panic rule
                // still wants a value here rather than an indexing panic —
                // opaque black is an inert filler no real frame produces.
                None => out.extend_from_slice(&[0, 0, 0, 0xff]),
            }
        }
    }

    image::Handle::from_rgba(dst_w, dst_h, out)
}

// ---------------------------------------------------------------------
// One toast, and the stack
// ---------------------------------------------------------------------

/// What a card is about.
///
/// **Stage 11** split this out of [`Toast`]: a recording that dies mid-stream
/// has to reach the user somehow (PLAN.md Stage 11 task 2 — "disk-full and
/// mid-stream-death surfaced as `Error` signal + toast"), and it has no file,
/// no thumbnail, and nothing an editor could open.
#[derive(Debug, Clone)]
enum ToastKind {
    /// A saved screenshot: the §6 card with the capture's own thumbnail in
    /// the icon tile, clickable to open the editor.
    Capture {
        path: PathBuf,
        thumbnail: image::Handle,
    },
    /// A message with no artefact behind it. Clicking does nothing —
    /// deliberately: a card whose click target does nothing *visible* is
    /// better than one that opens an editor on a file that was never written.
    ///
    /// **Style note (§11 checklist item 7).** The style guide's generic
    /// notification puts a Lucide glyph in the 36 px ivory icon tile. This
    /// crate still has no `src/icons.rs` (see CLAUDE.md's Design language
    /// section — the capture toast substitutes the screenshot's own thumbnail
    /// and therefore never needed one), so a notice renders the tile as a
    /// plain ivory square. Recorded as the same *deliberate substitution*
    /// the capture card already documents, not an oversight; whichever stage
    /// first needs a real icon set fills it in here.
    Notice { title: String, body: String },
    /// **Stage 12.** A recording that ended cleanly and was saved —
    /// `RecordingFailed`'s success-side sibling. No thumbnail (a video's
    /// first frame is not free to decode the way a screenshot's own pixels
    /// already-in-hand are — Stage 11 deliberately writes recordings without
    /// keeping a `Frame` around), so this renders the same plain-ivory tile
    /// [`ToastKind::Notice`] does, for the same "no `src/icons.rs` yet"
    /// reason. Clicking opens the **containing directory** (PLAN.md task 3:
    /// "videos open containing dir for now") rather than the editor, which
    /// has no video support at all.
    Recording { path: PathBuf },
    /// **Stage 16.** `PickColor` resolved. The icon tile is the picked
    /// color itself (a real swatch — the one `ToastKind` with content it can
    /// paint directly, unlike `Notice`/`Recording`'s "no `src/icons.rs` yet"
    /// plain-ivory substitute), and the body is the hex string in the
    /// design system's monospace family (PLAN.md task 2: "swatch toast + hex
    /// (mono font)"). Clicking does nothing — same posture as `Notice`: the
    /// clipboard copy already happened at pick time
    /// (`dbus.rs::CaptureService::pick_color`), before this toast is even
    /// pushed, so there is nothing left for a click to *do*.
    Swatch { hex: String, rgb: (f64, f64, f64) },
}

/// One notification card's state.
#[derive(Debug, Clone)]
struct Toast {
    id: u64,
    kind: ToastKind,
    /// The pausable stopwatch — see this module's doc comment.
    elapsed_at_last_change: Duration,
    resumed_at: Option<Instant>,
}

impl Toast {
    fn elapsed(&self, now: Instant) -> Duration {
        match self.resumed_at {
            Some(started) => self.elapsed_at_last_change + now.saturating_duration_since(started),
            None => self.elapsed_at_last_change,
        }
    }

    fn pause(&mut self, now: Instant) {
        if let Some(started) = self.resumed_at.take() {
            self.elapsed_at_last_change += now.saturating_duration_since(started);
        }
    }

    fn resume(&mut self, now: Instant) {
        if self.resumed_at.is_none() {
            self.resumed_at = Some(now);
        }
    }
}

/// The whole stack: at most `motion.toast_max_stack` cards, newest last.
#[derive(Debug, Default)]
pub struct ToastStack {
    toasts: Vec<Toast>,
    next_id: u64,
}

/// What a [`Message`] asks `main.rs` to do beyond the state change that
/// already happened inside [`ToastStack::update`] — the same "return a
/// value instead of a `Task`" shape `saola-panel::popover::Action` uses, for
/// the same reason: it keeps the decision testable without a compositor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// Spawn the editor on this path — `main.rs` turns this into the
    /// detached `saola-capture window edit <path>` call.
    Open(PathBuf),
    /// **Stage 12.** Open the directory containing this path — `main.rs`
    /// turns this into a detached `xdg-open` on the parent directory. The
    /// recording toast's click target, since the editor has no video
    /// support (PLAN.md task 3: "videos open containing dir for now").
    OpenDir(PathBuf),
}

impl ToastStack {
    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.toasts.len()
    }

    /// Push a freshly saved capture onto the stack. If the stack is already
    /// at `motion.toast_max_stack`, the oldest card is dropped first — §6's
    /// "stack at most three; the fourth replaces the oldest".
    pub fn push(&mut self, path: PathBuf, thumbnail: image::Handle, theme: &Theme, now: Instant) {
        self.push_kind(ToastKind::Capture { path, thumbnail }, theme, now);
    }

    /// Push a message with no file behind it — **Stage 11**'s recording
    /// failures ("Recording failed" / the encoder's own last words). Same
    /// timing, same stack rule, same card; see [`ToastKind::Notice`].
    pub fn push_notice(
        &mut self,
        title: impl Into<String>,
        body: impl Into<String>,
        theme: &Theme,
        now: Instant,
    ) {
        self.push_kind(
            ToastKind::Notice {
                title: title.into(),
                body: body.into(),
            },
            theme,
            now,
        );
    }

    /// Push a finished recording onto the stack — **Stage 12**'s success
    /// half of [`Self::push_notice`]'s failure-toast precedent (PLAN.md task
    /// 3, the finish toast). Same timing, same stack rule, same card; see
    /// [`ToastKind::Recording`].
    pub fn push_recording(&mut self, path: PathBuf, theme: &Theme, now: Instant) {
        self.push_kind(ToastKind::Recording { path }, theme, now);
    }

    /// Push a `PickColor` result — **Stage 16**. Same timing, same stack
    /// rule, same card; see [`ToastKind::Swatch`].
    pub fn push_swatch(&mut self, hex: String, rgb: (f64, f64, f64), theme: &Theme, now: Instant) {
        self.push_kind(ToastKind::Swatch { hex, rgb }, theme, now);
    }

    fn push_kind(&mut self, kind: ToastKind, theme: &Theme, now: Instant) {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.toasts.push(Toast {
            id,
            kind,
            elapsed_at_last_change: Duration::ZERO,
            resumed_at: Some(now),
        });

        let max = usize::from(theme.motion.toast_max_stack).max(1);
        while self.toasts.len() > max {
            self.toasts.remove(0);
        }
    }

    /// Fold one message into the stack. `theme` is only consulted by the
    /// `Tick` arm (it needs `motion.toast_total` to know when a card has
    /// lived its full life).
    pub fn update(&mut self, message: Message, now: Instant, theme: &Theme) -> Action {
        match message {
            Message::Tick => {
                let total = Duration::from_millis(theme.motion.toast_total.into());
                self.toasts.retain(|toast| toast.elapsed(now) < total);
                Action::None
            }
            Message::Hovered(id) => {
                if let Some(toast) = self.toasts.iter_mut().find(|toast| toast.id == id) {
                    toast.pause(now);
                }
                Action::None
            }
            Message::Unhovered(id) => {
                if let Some(toast) = self.toasts.iter_mut().find(|toast| toast.id == id) {
                    toast.resume(now);
                }
                Action::None
            }
            Message::Clicked(id) => match self.toasts.iter().find(|toast| toast.id == id) {
                Some(Toast {
                    kind: ToastKind::Capture { path, .. },
                    ..
                }) => Action::Open(path.clone()),
                // **Stage 12.**
                Some(Toast {
                    kind: ToastKind::Recording { path },
                    ..
                }) => Action::OpenDir(path.clone()),
                // A notice has nothing to open (Stage 11); neither does a
                // swatch (Stage 16) — the clipboard copy already happened
                // before this toast was ever pushed.
                Some(Toast {
                    kind: ToastKind::Notice { .. } | ToastKind::Swatch { .. },
                    ..
                })
                | None => Action::None,
            },
        }
    }

    /// Ticks only while at least one toast is up — the same gated shape
    /// [`crate::modules::flash::Flash::subscription`] uses.
    pub fn subscription(&self) -> Subscription<Message> {
        if self.is_empty() {
            Subscription::none()
        } else {
            iced::time::every(TICK).map(|_instant| Message::Tick)
        }
    }

    /// The stack, newest on top. Empty renders as nothing (zero-size —
    /// `main.rs` doesn't map a surface at all while the stack is empty, so
    /// this arm is mostly defensive: the very first frame of a freshly
    /// spawned surface, before the runtime's next `update`, could still
    /// call `view` against a state that has just gone empty).
    pub fn view(&self, theme: &Theme, now: Instant) -> Element<'static, Message> {
        if self.is_empty() {
            return Space::new().into();
        }
        let mut stack = column![].spacing(theme.sizes.island_gap);
        for toast in self.toasts.iter().rev() {
            stack = stack.push(card_view(theme, toast, now));
        }
        stack.into()
    }
}

/// The layer-shell surface height for `count` stacked cards (1..=3 in
/// practice — `ToastStack::push` never lets more than `toast_max_stack`
/// accumulate). Free function so `main.rs` can also call it before a
/// `ToastStack` exists (there is none at boot).
pub fn card_stack_height(theme: &Theme, count: usize) -> u32 {
    let count = count.max(1) as f32;
    let height = count * card_height(theme) + (count - 1.0).max(0.0) * theme.sizes.island_gap;
    height.round() as u32
}

/// One card's declared height. No token names this directly (see this
/// module's doc comment); derived from `sizes.list_row` — a card holds
/// roughly two rows' worth of content (icon tile + two lines of text) — and
/// documented at the use site rather than inlined as a bare literal,
/// mirroring how `saola-lockscreen::modules::reveal::view` derived its own
/// lock/greeter field height from the nearest two tokens that do exist.
fn card_height(theme: &Theme) -> f32 {
    theme.sizes.list_row * 2.0
}

// ---------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------

/// The toast stack's own message type, nested into `main.rs`'s outer
/// `Message` as `Message::Toast(..)` — the per-module-enum pattern
/// `saola-panel::modules::clock::Message`'s doc comment describes in full.
#[derive(Debug, Clone, Copy)]
pub enum Message {
    /// One [`TICK`] elapsed — the auto-dismiss and life-rule-progress
    /// check. Carries no data: [`ToastStack::update`] reads the `now`
    /// parameter `main.rs` passes alongside the message rather than a
    /// timestamp embedded in the tick itself (same "Tick wakes the update,
    /// doesn't carry the clock" shape [`crate::modules::flash::Message::
    /// Tick`] uses, and for the same dead-field reason: nothing here ever
    /// read the embedded `Instant`).
    Tick,
    /// The pointer entered a card's `MouseArea`. Pauses that card only.
    Hovered(u64),
    /// The pointer left a card's `MouseArea`. Resumes that card only.
    Unhovered(u64),
    /// A card was clicked. `main.rs` turns the resulting [`Action::Open`]
    /// into a detached `saola-capture window edit <path>` spawn.
    Clicked(u64),
}

// ---------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------

/// `(offset_x, alpha)` for one card at `elapsed` — §5's exact three-phase
/// timing (slide in / rest / fade out). See this module's doc comment for
/// why a spacer width stands in for a CSS `translateX`.
fn phase(theme: &Theme, elapsed: Duration) -> (f32, f32) {
    let card_width = theme.sizes.notification_card_width;
    let in_dur = Duration::from_millis(theme.motion.toast_in.into());
    let idle_dur = Duration::from_millis(theme.motion.toast_idle.into());
    let out_dur = Duration::from_millis(theme.motion.toast_out.into());

    if elapsed < in_dur {
        let progress = fraction(elapsed, in_dur);
        (card_width * (1.0 - progress), progress)
    } else if elapsed < in_dur + idle_dur {
        (0.0, 1.0)
    } else {
        let fade_elapsed = elapsed.saturating_sub(in_dur + idle_dur);
        let progress = fraction(fade_elapsed, out_dur);
        (0.0, 1.0 - progress)
    }
}

/// The life rule's width fraction: `1.0` the instant a card appears,
/// linearly down to `0.0` at `motion.toast_total` — §5: "a terracotta life
/// rule under the card scales from 1 to 0 over the same span."
fn life_fraction(theme: &Theme, elapsed: Duration) -> f32 {
    let total = Duration::from_millis(theme.motion.toast_total.into());
    1.0 - fraction(elapsed, total)
}

/// `elapsed / total`, clamped to `0.0..=1.0`. A zero-duration `total` (a
/// pathological `capture.toml`-adjacent theme override, not reachable from
/// the built-in theme) returns `1.0` — "already fully elapsed" — rather
/// than dividing by zero.
fn fraction(elapsed: Duration, total: Duration) -> f32 {
    if total.is_zero() {
        return 1.0;
    }
    (elapsed.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
}

/// One card, fully composed: icon tile, title + right-aligned app name,
/// body (the file name), and the life rule — §6 verbatim, modulo the two
/// gaps this module's doc comment records.
fn card_view(theme: &Theme, toast: &Toast, now: Instant) -> Element<'static, Message> {
    let elapsed = toast.elapsed(now);
    let (offset_x, alpha) = phase(theme, elapsed);
    let life = life_fraction(theme, elapsed);

    let scale_alpha = |mut color: iced::Color| {
        color.a *= alpha;
        color
    };

    let ink = scale_alpha(theme.palette.ink.into_iced());
    let text_primary = scale_alpha(theme.on_ink.primary.into_iced());
    let text_tertiary = scale_alpha(theme.on_ink.tertiary.into_iced());
    let text_secondary = scale_alpha(theme.on_ink.secondary.into_iced());
    let accent = scale_alpha(theme.palette.accent.into_iced());
    let mut shadow = theme.shadows.popover.into_iced();
    shadow.color.a *= alpha;

    let card_width = theme.sizes.notification_card_width;
    let radius = theme.radii.card;
    let padding = theme.sizes.popover_padding;
    let title_font = saola_theme::convert::ui_font(theme);
    let body_font = saola_theme::convert::ui_font_regular(theme);
    let title_size = theme.typography.size.body;
    let meta_size = theme.typography.size.meta;
    let body_size = theme.typography.size.secondary;
    let gap = theme.sizes.pill_gap;

    let (title_text, body_text) = match &toast.kind {
        ToastKind::Capture { path, .. } => (
            "Screenshot saved".to_string(),
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
        ),
        ToastKind::Notice { title, body } => (title.clone(), body.clone()),
        // **Stage 12.**
        ToastKind::Recording { path } => (
            "Recording saved".to_string(),
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
        ),
        // **Stage 16.** The body is the hex string itself, rendered below in
        // the design system's mono family rather than `body_font` — see the
        // `tile`/mono-font branch just below this match.
        ToastKind::Swatch { hex, .. } => ("Color picked".to_string(), hex.clone()),
    };

    let tile: Element<'static, Message> = match &toast.kind {
        ToastKind::Capture { thumbnail, .. } => image(thumbnail.clone())
            .width(Length::Fixed(ICON_TILE_SIZE))
            .height(Length::Fixed(ICON_TILE_SIZE))
            .content_fit(iced::ContentFit::Cover)
            .into(),
        // See `ToastKind::Notice`/`ToastKind::Recording`: an ivory tile, no
        // glyph, until this crate has an icon set.
        ToastKind::Notice { .. } | ToastKind::Recording { .. } => {
            let paper = scale_alpha(theme.palette.paper.into_iced());
            container(Space::new())
                .width(Length::Fixed(ICON_TILE_SIZE))
                .height(Length::Fixed(ICON_TILE_SIZE))
                .style(move |_: &iced::Theme| container::Style {
                    background: Some(iced::Background::Color(paper)),
                    ..container::Style::default()
                })
                .into()
        }
        // **Stage 16.** Unlike `Notice`/`Recording`, this tile *has* real
        // content to paint with no icon set needed at all: the picked color
        // itself, at full alpha regardless of the card's own fade (a
        // desaturated swatch mid-fade would misreport the very color the
        // toast exists to show).
        ToastKind::Swatch { rgb, .. } => {
            let (r, g, b) = *rgb;
            let swatch = iced::Color::from_rgb(r as f32, g as f32, b as f32);
            container(Space::new())
                .width(Length::Fixed(ICON_TILE_SIZE))
                .height(Length::Fixed(ICON_TILE_SIZE))
                .style(move |_: &iced::Theme| container::Style {
                    background: Some(iced::Background::Color(swatch)),
                    ..container::Style::default()
                })
                .into()
        }
    };

    let thumb = container(tile)
        .width(Length::Fixed(ICON_TILE_SIZE))
        .height(Length::Fixed(ICON_TILE_SIZE));

    let header = row![
        text(title_text)
            .font(title_font)
            .size(title_size)
            .color(text_primary),
        Space::new().width(Length::Fill),
        text("saola-capture")
            .font(body_font)
            .size(meta_size)
            .color(text_tertiary),
    ]
    .align_y(Center);

    // **Stage 16.** PLAN.md task 2: "swatch toast + hex (mono font)" — the
    // one card whose body isn't a filename or free-text message, so it's the
    // one card that reaches for `saola_theme::convert::mono_font` rather
    // than `body_font`.
    let body_display_font = match &toast.kind {
        ToastKind::Swatch { .. } => saola_theme::convert::mono_font(theme),
        _ => body_font,
    };
    let body = text(body_text)
        .font(body_display_font)
        .size(body_size)
        .color(text_secondary);

    let content = row![
        thumb,
        column![header, body].spacing(4.0).width(Length::Fill),
    ]
    .spacing(gap)
    .padding(padding)
    .align_y(Center);

    let life_rule = container(
        Space::new()
            .width(Length::Fixed((card_width * life).max(0.0)))
            .height(Length::Fixed(LIFE_RULE_HEIGHT)),
    )
    .style(move |_: &iced::Theme| container::Style {
        background: Some(iced::Background::Color(accent)),
        ..container::Style::default()
    });

    let card = container(column![content, life_rule])
        .width(Length::Fixed(card_width))
        .style(ink_card_style(ink, text_primary, radius, shadow));

    let slid = row![Space::new().width(Length::Fixed(offset_x.max(0.0))), card,];

    mouse_area(slid)
        .on_press(Message::Clicked(toast.id))
        .on_enter(Message::Hovered(toast.id))
        .on_exit(Message::Unhovered(toast.id))
        .into()
}

/// The §6 notification card's own container style — see this module's doc
/// comment for why `saola_theme::style::container::card` is the wrong
/// helper here. Every argument is a plain `Copy` value rather than a
/// borrowed `&Theme`, so the returned closure is `'static` (matching
/// `saola-theme`'s own style helpers' shape — see e.g.
/// `saola_theme::style::container::card`, which extracts primitives before
/// building its closure for the same reason).
fn ink_card_style(
    background: iced::Color,
    text_color: iced::Color,
    radius: f32,
    shadow: iced::Shadow,
) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(text_color),
        background: Some(iced::Background::Color(background)),
        border: iced::Border {
            color: iced::Color::TRANSPARENT,
            width: 0.0,
            radius: radius.into(),
        },
        shadow,
        ..container::Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::saola()
    }

    fn synthetic_frame(width: u32, height: u32) -> Frame {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 0x40, 0xff]);
            }
        }
        Frame::new(width, height, 1.0, pixels).expect("well-formed synthetic frame")
    }

    fn handle_dims(handle: &image::Handle) -> (u32, u32) {
        match handle {
            image::Handle::Rgba { width, height, .. } => (*width, *height),
            _ => unreachable!("thumbnail_handle always builds Handle::Rgba"),
        }
    }

    // -- thumbnail_handle ---------------------------------------------------

    #[test]
    fn thumbnail_downscales_preserving_aspect_ratio() {
        let frame = synthetic_frame(2560, 1600);
        let handle = thumbnail_handle(&frame, 128);
        let (w, h) = handle_dims(&handle);
        assert_eq!(w, 128, "the longer side hits the cap exactly");
        assert!(h < 128 && h > 0);
        // 2560:1600 == 8:5; 128 * 5 / 8 == 80.
        assert_eq!(h, 80);
    }

    #[test]
    fn thumbnail_never_upscales_a_small_frame() {
        let frame = synthetic_frame(20, 10);
        let handle = thumbnail_handle(&frame, 128);
        assert_eq!(handle_dims(&handle), (20, 10));
    }

    #[test]
    fn thumbnail_pixel_count_matches_its_declared_dimensions() {
        let frame = synthetic_frame(300, 300);
        let handle = thumbnail_handle(&frame, 36);
        let image::Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = handle
        else {
            unreachable!("Handle::Rgba");
        };
        assert_eq!(pixels.len(), (width * height * 4) as usize);
    }

    // -- Toast's pausable stopwatch -----------------------------------------

    fn toast_at(now: Instant) -> Toast {
        Toast {
            id: 0,
            kind: ToastKind::Capture {
                path: PathBuf::from("/tmp/Screenshot_test.webp"),
                thumbnail: thumbnail_handle(&synthetic_frame(4, 4), 36),
            },
            elapsed_at_last_change: Duration::ZERO,
            resumed_at: Some(now),
        }
    }

    /// Every card's path, for the stack-order assertions. A notice has none;
    /// a recording's own path is deliberately excluded too (nothing in this
    /// module's own tests orders recordings against captures, so keeping
    /// this helper `Capture`-only rather than teaching it a second path
    /// shape it never needs to compare).
    fn stack_paths(stack: &ToastStack) -> Vec<PathBuf> {
        stack
            .toasts
            .iter()
            .filter_map(|toast| match &toast.kind {
                ToastKind::Capture { path, .. } => Some(path.clone()),
                ToastKind::Notice { .. }
                | ToastKind::Recording { .. }
                | ToastKind::Swatch { .. } => None,
            })
            .collect()
    }

    #[test]
    fn elapsed_advances_while_running() {
        let now = Instant::now();
        let toast = toast_at(now);
        assert_eq!(
            toast.elapsed(now + Duration::from_secs(2)),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn pausing_freezes_elapsed_and_resuming_continues_from_there() {
        let now = Instant::now();
        let mut toast = toast_at(now);

        toast.pause(now + Duration::from_secs(1));
        // Frozen: no further advance while paused.
        assert_eq!(
            toast.elapsed(now + Duration::from_secs(5)),
            Duration::from_secs(1)
        );

        toast.resume(now + Duration::from_secs(5));
        assert_eq!(
            toast.elapsed(now + Duration::from_secs(6)),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn pausing_twice_in_a_row_is_a_no_op() {
        let now = Instant::now();
        let mut toast = toast_at(now);
        toast.pause(now + Duration::from_secs(1));
        toast.pause(now + Duration::from_secs(3)); // already paused — ignored
        assert_eq!(
            toast.elapsed(now + Duration::from_secs(10)),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn resuming_twice_in_a_row_is_a_no_op() {
        let now = Instant::now();
        let mut toast = toast_at(now);
        toast.resume(now + Duration::from_secs(1)); // already running — ignored
        assert_eq!(
            toast.elapsed(now + Duration::from_secs(2)),
            Duration::from_secs(2)
        );
    }

    // -- ToastStack::push (the stack-of-3 rule) ------------------------------

    #[test]
    fn pushing_a_fourth_toast_drops_the_oldest() {
        let theme = theme();
        let now = Instant::now();
        let mut stack = ToastStack::default();
        let thumb = thumbnail_handle(&synthetic_frame(4, 4), 36);

        for n in 0..4 {
            stack.push(
                PathBuf::from(format!("/tmp/shot-{n}.webp")),
                thumb.clone(),
                &theme,
                now,
            );
        }

        assert_eq!(stack.len(), 3, "capped at motion.toast_max_stack");
        let paths = stack_paths(&stack);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/tmp/shot-1.webp"),
                PathBuf::from("/tmp/shot-2.webp"),
                PathBuf::from("/tmp/shot-3.webp"),
            ],
            "the oldest (shot-0) was dropped; the newest three survive, oldest-first"
        );
    }

    /// **Stage 11.** A notice shares the card, the stack rule and the timing,
    /// and differs in exactly two places: it has no file, and clicking it
    /// does nothing.
    #[test]
    fn a_notice_toast_stacks_like_a_capture_but_opens_nothing() {
        let theme = theme();
        let now = Instant::now();
        let mut stack = ToastStack::default();

        stack.push(
            PathBuf::from("/tmp/shot.webp"),
            thumbnail_handle(&synthetic_frame(4, 4), 36),
            &theme,
            now,
        );
        stack.push_notice(
            "Recording failed",
            "ffmpeg exited with status 1: No space left on device",
            &theme,
            now,
        );
        assert_eq!(stack.len(), 2);
        assert_eq!(stack_paths(&stack), vec![PathBuf::from("/tmp/shot.webp")]);

        // Clicking the capture opens it…
        let capture_id = stack.toasts[0].id;
        assert_eq!(
            stack.update(Message::Clicked(capture_id), now, &theme),
            Action::Open(PathBuf::from("/tmp/shot.webp"))
        );
        // …and clicking the notice does nothing at all.
        let notice_id = stack.toasts[1].id;
        assert_eq!(
            stack.update(Message::Clicked(notice_id), now, &theme),
            Action::None
        );
    }

    /// A notice expires on the same clock every other card does, so a failed
    /// recording does not leave a card up forever.
    #[test]
    fn a_notice_toast_expires_like_any_other() {
        let theme = theme();
        let now = Instant::now();
        let mut stack = ToastStack::default();
        stack.push_notice("Recording failed", "disk full", &theme, now);

        let total = Duration::from_millis(theme.motion.toast_total.into());
        stack.update(Message::Tick, now + total / 2, &theme);
        assert_eq!(stack.len(), 1);
        stack.update(
            Message::Tick,
            now + total + Duration::from_millis(1),
            &theme,
        );
        assert!(stack.is_empty());
    }

    // -- ToastStack::update ---------------------------------------------------

    #[test]
    fn tick_expires_a_toast_past_its_total_lifetime() {
        let theme = theme();
        let now = Instant::now();
        let mut stack = ToastStack::default();
        stack.push(
            PathBuf::from("/tmp/a.webp"),
            thumbnail_handle(&synthetic_frame(4, 4), 36),
            &theme,
            now,
        );

        let total = Duration::from_millis(theme.motion.toast_total.into());
        let action = stack.update(Message::Tick, now + total, &theme);

        assert_eq!(action, Action::None);
        assert!(stack.is_empty());
    }

    #[test]
    fn tick_never_expires_a_hovered_toast() {
        let theme = theme();
        let now = Instant::now();
        let mut stack = ToastStack::default();
        stack.push(
            PathBuf::from("/tmp/a.webp"),
            thumbnail_handle(&synthetic_frame(4, 4), 36),
            &theme,
            now,
        );
        let id = stack.toasts[0].id;

        let _ = stack.update(Message::Hovered(id), now + Duration::from_secs(1), &theme);

        let total = Duration::from_millis(theme.motion.toast_total.into());
        let far_future = now + total * 10;
        let _ = stack.update(Message::Tick, far_future, &theme);

        assert_eq!(
            stack.len(),
            1,
            "a paused toast's elapsed time never advances"
        );
    }

    #[test]
    fn unhovering_resumes_the_countdown() {
        let theme = theme();
        let now = Instant::now();
        let mut stack = ToastStack::default();
        stack.push(
            PathBuf::from("/tmp/a.webp"),
            thumbnail_handle(&synthetic_frame(4, 4), 36),
            &theme,
            now,
        );
        let id = stack.toasts[0].id;

        let _ = stack.update(Message::Hovered(id), now + Duration::from_secs(1), &theme);
        let _ = stack.update(Message::Unhovered(id), now + Duration::from_secs(4), &theme);

        let total = Duration::from_millis(theme.motion.toast_total.into());
        let far_future = now + total * 10;
        let _ = stack.update(Message::Tick, far_future, &theme);

        assert!(stack.is_empty(), "resumed, so it must eventually expire");
    }

    #[test]
    fn clicking_a_known_toast_opens_its_path() {
        let theme = theme();
        let now = Instant::now();
        let mut stack = ToastStack::default();
        stack.push(
            PathBuf::from("/tmp/click-me.webp"),
            thumbnail_handle(&synthetic_frame(4, 4), 36),
            &theme,
            now,
        );
        let id = stack.toasts[0].id;

        let action = stack.update(Message::Clicked(id), now, &theme);

        assert_eq!(action, Action::Open(PathBuf::from("/tmp/click-me.webp")));
    }

    #[test]
    fn clicking_an_unknown_id_is_a_no_op() {
        let theme = theme();
        let now = Instant::now();
        let mut stack = ToastStack::default();
        stack.push(
            PathBuf::from("/tmp/a.webp"),
            thumbnail_handle(&synthetic_frame(4, 4), 36),
            &theme,
            now,
        );

        let action = stack.update(Message::Clicked(9999), now, &theme);

        assert_eq!(action, Action::None);
    }

    // -- phase / life_fraction ------------------------------------------------

    #[test]
    fn phase_slides_in_from_a_full_card_width_to_zero() {
        let theme = theme();
        let in_dur = Duration::from_millis(theme.motion.toast_in.into());

        let (offset_start, alpha_start) = phase(&theme, Duration::ZERO);
        assert_eq!(offset_start, theme.sizes.notification_card_width);
        assert_eq!(alpha_start, 0.0);

        let (offset_end, alpha_end) = phase(&theme, in_dur);
        assert_eq!(offset_end, 0.0);
        assert_eq!(alpha_end, 1.0);
    }

    #[test]
    fn phase_is_fully_visible_and_stationary_at_rest() {
        let theme = theme();
        let in_dur = Duration::from_millis(theme.motion.toast_in.into());
        let idle_dur = Duration::from_millis(theme.motion.toast_idle.into());

        let (offset, alpha) = phase(&theme, in_dur + idle_dur / 2);
        assert_eq!(offset, 0.0);
        assert_eq!(alpha, 1.0);
    }

    #[test]
    fn phase_fades_to_zero_by_the_end_of_the_total_span() {
        let theme = theme();
        let total = Duration::from_millis(theme.motion.toast_total.into());

        let (offset, alpha) = phase(&theme, total);
        assert_eq!(offset, 0.0);
        assert_eq!(alpha, 0.0);
    }

    #[test]
    fn life_fraction_counts_down_from_one_to_zero_over_the_total_span() {
        let theme = theme();
        let total = Duration::from_millis(theme.motion.toast_total.into());

        assert_eq!(life_fraction(&theme, Duration::ZERO), 1.0);
        assert_eq!(life_fraction(&theme, total), 0.0);
        let half = life_fraction(&theme, total / 2);
        assert!((half - 0.5).abs() < 0.01, "got {half}");
    }

    #[test]
    fn fraction_clamps_and_guards_against_a_zero_total() {
        assert_eq!(fraction(Duration::from_secs(1), Duration::ZERO), 1.0);
        assert_eq!(
            fraction(Duration::from_secs(100), Duration::from_secs(1)),
            1.0
        );
        assert_eq!(fraction(Duration::ZERO, Duration::from_secs(1)), 0.0);
    }

    // -- surface sizing ---------------------------------------------------

    #[test]
    fn surface_height_grows_with_more_cards_and_never_shrinks_below_one() {
        let theme = theme();
        let one = card_stack_height(&theme, 1);
        let three = card_stack_height(&theme, 3);
        assert!(three > one);
        assert_eq!(
            card_stack_height(&theme, 0),
            one,
            "clamped to at least one card"
        );
    }
}
