//! The daemon-side SNI tray item — PLAN.md Stage 12, task 1/2/3's non-UI
//! half: `org.kde.StatusNotifierItem` plus a small `com.canonical.dbusmenu`,
//! served over the *same* `io.saola.Capture1` connection (`dbus::serve`
//! spawns [`install`] once the bus name is claimed).
//!
//! # Why this module doesn't look like `flash`/`toast`/`overlay`/`countdown`
//!
//! Every other file in `modules/` follows the sibling shape `modules/mod.rs`
//! documents: a state struct, `view(&Theme) -> Element`, `subscription()`,
//! nested `Message`. Those are all layer-shell surfaces `main.rs`'s `Daemon`
//! draws pixels onto. A tray icon draws **no pixels of its own** — the panel
//! (or whatever else is watching `org.kde.StatusNotifierWatcher`) renders it,
//! entirely over D-Bus (`~/Developer/saola-panel/src/modules/tray/`, the
//! counterpart "host" this stage was told to read first). So this module has
//! no `view`, no `Message`, and is never registered against `Daemon`'s
//! `SurfaceRole`. What it *does* have, mirroring the shape as closely as the
//! problem allows: one entry point ([`install`]) that is the daemon's
//! standing "keep the tray registered and current" worker, the served-object
//! equivalent of a `subscription()`.
//!
//! # Absent host — the one sibling rule that still applies unmodified
//!
//! "The panel is the live host; degrade silently if no host" (PLAN.md Stage
//! 12, task 1) is this crate's own restatement of the sibling
//! absent-service rule (CLAUDE.md: "Absent services ... degrade gracefully
//! ... never crashes"). [`install`] tries once to register with
//! `org.kde.StatusNotifierWatcher`; if nothing owns that name yet (no panel
//! running, or a bare niri session with no tray host at all), it does not
//! error, retry-loop noisily, or block anything else in the daemon — it
//! watches `NameOwnerChanged` for the watcher name and retries the one
//! registration call if and when a host ever appears. If one never does,
//! this task simply sits idle for the daemon's whole life, at the cost of
//! nothing more than the one subscription.
//!
//! # The icon: a procedural glyph, not an asset (a deliberate substitution)
//!
//! This crate still has no `src/icons.rs` (CLAUDE.md Design language notes
//! this at every prior surface that could have wanted one — the overlay's
//! toolbar, the toast's notice tile). Building one now, for exactly two
//! glyph states, would be new-icon-set-for-four-buttons-sized scope creep of
//! the kind `modules::overlay`'s own doc comment already declined. Instead
//! [`record_glyph`] draws the "solid record dot" CLAUDE.md's Design language
//! sanctions (`"Record/stop/play are among the only solid icons"`)
//! procedurally, straight into SNI's `IconPixmap` wire format: a filled
//! terracotta circle while recording, an outlined (unfilled) one at rest —
//! the same accent-fills-when-live idiom every other Saola control uses
//! (`"On, selected, focused, live"` — `saola_theme::tokens::Palette::accent`'s
//! own doc comment), read directly off `Theme::saola()` so no color is
//! hardcoded here either.
//!
//! # Elapsed time: `Title`/`ToolTip`, not a live push (PLAN.md task 2)
//!
//! `RecorderState::elapsed` exists "for the tray tooltip Stage 12 adds"
//! (its own doc comment) and both [`title_for`] and [`SniItem::tool_tip`] use
//! it.
//! What this module does **not** do is emit a signal every second to keep
//! that value fresh on hosts that are actively watching: the panel — the one
//! real host this crate targets — has no rendering path for either `Title`
//! or `ToolTip` at all today (verified against its source: `Tray::view`
//! never reads an item's `Title`, and grepping the whole `tray/` directory
//! for "tooltip" finds nothing), so a per-second `NewTitle`/`NewToolTip`
//! would be D-Bus chatter with no observer. Every *other* real SNI host
//! (GNOME Shell's appindicator extension, KDE's own panel) reads `Title`/
//! `ToolTip` live when a hover tooltip is actually requested, which is a
//! property **read**, not a push — so the value is correct the moment
//! anyone asks, with no background timer needed to keep it so. What *does*
//! get pushed (`NewIcon`/`NewTitle`, not `NewStatus` — `Status` itself never
//! changes, see [`SniItem::status`]) is the idle/recording transition
//! itself, via [`watch_and_emit`]'s poll — see that function's doc comment
//! for why a poll, here, is the honest answer rather than a violation of
//! "every module maps to a signal, not a poll".
//!
//! # The chip surface PLAN.md task 2 asks about
//!
//! "Add the small chip surface only if Stage 2/7 evidence says a layer-shell
//! pill is cheap — otherwise note it as future work." Stage 7's own
//! measurement (CLAUDE.md Architecture: "~450-560 ms from `shot --region`
//! starting to the overlay's first composited frame ... survivable
//! precisely because the overlay stays up until the user acts") is about a
//! surface the user is actively looking at for its whole (short) life; a
//! recording elapsed-time chip would instead be a *permanently-mapped,
//! continuously-redrawing* surface for the potentially-minutes-long duration
//! of every recording — closer in shape to the flash's "spawn once, keep
//! mapped" trick than to the overlay's reactive one, but for content that
//! changes every second rather than the flash's single opacity fade. Nothing
//! in Stage 2/7's evidence measures *that* cost (a per-second redraw of a
//! small always-on-top surface, for however long a recording runs), and
//! Stage 12 has no session-safe way to gather it live (mapping a new
//! full-output layer-shell surface is exactly the class of thing CLAUDE.md's
//! nested-niri rule exists for, and this stage cannot spend a live-verify
//! slot on a surface PLAN.md itself made conditional). **Recorded here as
//! future work, not built**: `Title`/`ToolTip` are this stage's answer to
//! "where does the elapsed time live", and a layer-shell chip remains an
//! open surface-latency question for whichever later stage wants to spend a
//! nested-niri session measuring it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::Serialize;
use zbus::object_server::{InterfaceRef, SignalEmitter};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Type, Value};
use zbus::Connection;

use crate::dbus::{DaemonEvent, SharedRecorder};

/// Where the SNI item lives — the well-known-bus-name form of
/// `RegisterStatusNotifierItem`'s argument (see `item.rs`'s module doc
/// comment on the panel side for the registration-string quirk this
/// sidesteps entirely): passing `io.saola.Capture1` itself as the argument
/// means the watcher resolves the item to exactly this path, the protocol's
/// own default, with no ambiguity to normalize.
const ITEM_OBJECT_PATH: &str = "/StatusNotifierItem";
/// Where this item's `com.canonical.dbusmenu` object lives — on the same bus
/// name, a sibling path to [`ITEM_OBJECT_PATH`] rather than nested under it
/// (SNI's `Menu` property is a bare object path; nothing about the protocol
/// asks for a parent/child relationship between the two paths, and keeping
/// them siblings is one fewer thing to get wrong copying this elsewhere).
const MENU_OBJECT_PATH: &str = "/StatusNotifierMenu";

const WATCHER_BUS_NAME: &str = "org.kde.StatusNotifierWatcher";

/// How often [`watch_and_emit`] checks whether the recording state flipped.
/// Cheap (one `Mutex` lock, no I/O) and coarse on purpose — this is not the
/// clock the *toast*/countdown surfaces run on (those redraw a visible
/// pixel), it only has to notice "idle vs recording" changed sometime soon
/// after it happens, and 750 ms is imperceptible against "the whole tray
/// icon just changed shape".
const POLL_INTERVAL: Duration = Duration::from_millis(750);

/// The pixel size every [`record_glyph`] is drawn at. 24 matches
/// `saola-panel::modules::tray::item::ICON_LOOKUP_SIZE` — not because
/// `IconPixmap` has to agree with a theme lookup (it's the fallback *from*
/// that lookup), but because the panel's [`pick_pixmap`]-equivalent chooses
/// the closest-to-24 entry among however many an item publishes, and this
/// item only ever publishes one.
const GLYPH_SIZE: i32 = 24;

/// SNI's `ToolTip` property wire shape: `(icon-name, icon-pixmap, title,
/// description)` — `(sa(iiay)ss)` on the wire. A named alias purely to keep
/// [`SniItem::tool_tip`]'s signature legible; clippy's `type_complexity`
/// lint flags the bare tuple otherwise.
type ToolTip = (String, Vec<(i32, i32, Vec<u8>)>, String, String);

// ---------------------------------------------------------------------
// The icon: a procedural glyph (see the module doc comment)
// ---------------------------------------------------------------------

/// One `IconPixmap` entry: a `size × size` circle, filled (recording) or
/// outlined (idle), in `color`, transparent everywhere else — SNI's ARGB32
/// **network byte order** (`[A, R, G, B]` per pixel; see
/// `saola-panel::modules::tray::item::argb_network_to_rgba`'s doc comment
/// for the byte-order rationale this is the write-side mirror of).
///
/// Pure and deterministic — no bus, no theme lookup, no file I/O — so it is
/// unit-tested directly rather than only ever exercised over a live
/// connection.
fn record_glyph(size: i32, filled: bool, color: saola_theme::tokens::Color) -> (i32, i32, Vec<u8>) {
    // A little inset from the full square so the glyph doesn't touch the
    // edges of whatever cell the host draws it in, and a stroke width for
    // the idle ring that reads clearly at this size without looking like a
    // smudge.
    let center = (size as f32 - 1.0) / 2.0;
    let radius = size as f32 / 2.0 - 2.0;
    let stroke = 2.5_f32;

    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let distance = (dx * dx + dy * dy).sqrt();
            let inside = if filled {
                distance <= radius
            } else {
                distance <= radius && distance >= radius - stroke
            };
            if inside {
                pixels.extend_from_slice(&[color.a, color.r, color.g, color.b]);
            } else {
                pixels.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    (size, size, pixels)
}

/// `M:SS` (no hours field — a recording running past 99 minutes is not a
/// case worth a wider format for a tray tooltip). Pure; unit-tested below.
fn format_elapsed(elapsed: Duration) -> String {
    let total_seconds = elapsed.as_secs();
    format!("{}:{:02}", total_seconds / 60, total_seconds % 60)
}

fn title_for(active: bool, elapsed: Option<Duration>) -> String {
    match (active, elapsed) {
        (true, Some(elapsed)) => format!("Saola Capture — Recording {}", format_elapsed(elapsed)),
        (true, None) => "Saola Capture — Recording".to_string(),
        (false, _) => "Saola Capture".to_string(),
    }
}

// ---------------------------------------------------------------------
// The item
// ---------------------------------------------------------------------

/// The served `org.kde.StatusNotifierItem` object. Holds the *same*
/// [`SharedRecorder`] `dbus::serve` built for its own [`crate::dbus::
/// CaptureService`] (see that type's doc comment) — every property getter
/// below reads the live recorder state fresh, the same "no cached, no second
/// source of truth" posture the `Recording` D-Bus property already has.
struct SniItem {
    recorder: SharedRecorder,
}

impl SniItem {
    fn is_active(&self) -> bool {
        crate::dbus::lock_recorder(&self.recorder).is_active()
    }

    fn elapsed(&self) -> Option<Duration> {
        crate::dbus::lock_recorder(&self.recorder).elapsed(Instant::now())
    }
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl SniItem {
    #[zbus(property)]
    fn category(&self) -> String {
        "ApplicationStatus".to_string()
    }

    #[zbus(property)]
    fn id(&self) -> String {
        "saola-capture".to_string()
    }

    #[zbus(property)]
    fn title(&self) -> String {
        title_for(self.is_active(), self.elapsed())
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        let theme = saola_theme::Theme::saola();
        vec![record_glyph(
            GLYPH_SIZE,
            self.is_active(),
            theme.palette.accent,
        )]
    }

    /// **Always `"Active"`.** PLAN.md task 1 asks for "idle and recording
    /// icon states" — both are states the icon should be *visible* in, not
    /// one where the item hides itself (`"Passive"`, which
    /// `saola-panel::modules::tray::Tray::is_present` filters out of the bar
    /// entirely). The idle/recording distinction lives entirely in
    /// [`Self::icon_pixmap`]/[`Self::title`]/[`Self::tool_tip`] instead.
    #[zbus(property)]
    fn status(&self) -> String {
        "Active".to_string()
    }

    #[zbus(property)]
    fn tool_tip(&self) -> ToolTip {
        let active = self.is_active();
        let elapsed = self.elapsed();
        let description = match (active, elapsed) {
            (true, Some(elapsed)) => format!("Recording — {} elapsed", format_elapsed(elapsed)),
            (true, None) => "Recording".to_string(),
            (false, _) => String::new(),
        };
        (
            String::new(),
            Vec::new(),
            title_for(active, elapsed),
            description,
        )
    }

    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        // A hardcoded literal this module itself defines — see
        // `saola-panel::modules::tray::watcher`'s `FAKE_MENU_PATH` for the
        // same "a compile-time-constant path is validated once, not a
        // runtime failure surface" idiom this `.expect()` follows.
        OwnedObjectPath::try_from(MENU_OBJECT_PATH).expect("a valid object path literal")
    }

    /// `false`: unlike the libdbusmenu-based items `saola-panel::modules::
    /// tray::item`'s own doc comment describes (which export no `Activate`
    /// at all), this item's left-click does something real — see
    /// [`Self::activate`].
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    /// SNI's primary interaction: raise the app window, the same thing the
    /// tray menu's own "Open Saola Capture" row does
    /// ([`spawn_window_process`](crate::dbus::spawn_window_process)) — `x`/
    /// `y` are accepted (the spec's own screen-coordinates hint some hosts
    /// send) and ignored, the same posture `saola-panel::modules::tray::
    /// item::send_activate`'s doc comment documents for its own `(0, 0)`.
    async fn activate(&self, _x: i32, _y: i32) -> zbus::fdo::Result<()> {
        if let Err(err) = crate::dbus::spawn_window_process("main") {
            eprintln!("saola-capture: daemon: tray Activate could not open the app window: {err}");
        }
        Ok(())
    }

    /// A scroll over the icon. Nothing in this app has a "wheel changes
    /// something" affordance (unlike, say, a volume applet), so this is a
    /// deliberate no-op rather than an unimplemented stub — SNI hosts expect
    /// the method to exist and answer, not to error.
    async fn scroll(&self, _delta: i32, _orientation: &str) -> zbus::fdo::Result<()> {
        Ok(())
    }

    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_title(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

// ---------------------------------------------------------------------
// The menu
// ---------------------------------------------------------------------

/// One layout node exactly as it goes on the wire: `(ia{sv}av)` — this
/// module's own declaration of the same shape
/// `saola-panel::modules::tray::menu::RawMenuNode` decodes, kept
/// independent on purpose (see that type's own doc comment on why the two
/// ends of a protocol only need to agree on the wire shape, not share a
/// source). `children` is `Vec<OwnedValue>`, not `Vec<RawMenuNode>`, for the
/// identical reason: D-Bus has no recursive types, so a child menu node has
/// to be boxed into a variant.
#[derive(Debug, Serialize, Type, Value, OwnedValue)]
struct RawMenuNode {
    id: i32,
    properties: HashMap<String, OwnedValue>,
    children: Vec<OwnedValue>,
}

fn leaf_node(id: i32, label: &str, enabled: bool) -> RawMenuNode {
    let mut properties = HashMap::new();
    properties.insert(
        "label".to_string(),
        OwnedValue::try_from(Value::from(label)).expect("a &str always converts"),
    );
    properties.insert(
        "enabled".to_string(),
        OwnedValue::try_from(Value::from(enabled)).expect("a bool always converts"),
    );
    RawMenuNode {
        id,
        properties,
        children: Vec::new(),
    }
}

/// The menu's whole content, as plain data — pure and unit-tested on its
/// own, independent of the `RawMenuNode` wire wrapping [`root_node`] adds.
/// PLAN.md Stage 12, task 1's own words: "menu: Stop recording / Open Saola
/// Capture / Quit daemon", in that order.
fn menu_rows(recording: bool) -> [(i32, &'static str, bool); 3] {
    [
        (1, "Stop recording", recording),
        (2, "Open Saola Capture", true),
        (3, "Quit daemon", true),
    ]
}

fn root_node(recording: bool) -> RawMenuNode {
    let children = menu_rows(recording)
        .into_iter()
        .map(|(id, label, enabled)| {
            OwnedValue::try_from(leaf_node(id, label, enabled)).expect("a leaf node converts")
        })
        .collect();
    RawMenuNode {
        id: 0,
        properties: HashMap::new(),
        children,
    }
}

struct TrayMenu {
    recorder: SharedRecorder,
    events: iced::futures::channel::mpsc::Sender<DaemonEvent>,
}

impl TrayMenu {
    fn is_active(&self) -> bool {
        crate::dbus::lock_recorder(&self.recorder).is_active()
    }
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl TrayMenu {
    /// The whole tree — this menu has no submenus, so `parent_id` only ever
    /// means "everything" (`0`, what every real dbusmenu host sends for a
    /// context menu) in practice; a nonzero id (a host asking about a
    /// submenu that doesn't exist) gets back a childless node with that id
    /// rather than an error, which is the same "degrade, don't refuse"
    /// posture as everything else in this file.
    fn get_layout(
        &self,
        parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, RawMenuNode)> {
        let node = if parent_id == 0 {
            root_node(self.is_active())
        } else {
            RawMenuNode {
                id: parent_id,
                properties: HashMap::new(),
                children: Vec::new(),
            }
        };
        // A fixed revision: this menu's *structure* never changes (three
        // rows, always), only the "Stop recording" row's `enabled` — and
        // that is re-read on every `GetLayout`, which the host always calls
        // right after `AboutToShow` (dbusmenu's own freshness contract; see
        // `saola-panel::modules::tray::menu`'s module doc comment), so there
        // is never a stale layout for a bumped revision to invalidate.
        Ok((1, node))
    }

    /// Always answers "yes, re-read" — trivially safe for three static rows,
    /// and the panel host ignores the answer anyway (verified against its
    /// source).
    async fn about_to_show(&self, _id: i32) -> zbus::fdo::Result<bool> {
        Ok(true)
    }

    /// One interaction reaches this whole item: a click on one of the three
    /// rows. `event_id` is checked because the spec also defines
    /// `"hovered"`, which this menu has nothing to do in response to.
    async fn event(
        &self,
        id: i32,
        event_id: &str,
        _data: Value<'_>,
        _timestamp: u32,
    ) -> zbus::fdo::Result<()> {
        if event_id != "clicked" {
            return Ok(());
        }

        match id {
            1 => {
                let recorder = self.recorder.clone();
                tokio::spawn(async move {
                    if let Err(err) = crate::dbus::stop_recording_now(&recorder).await {
                        eprintln!(
                            "saola-capture: daemon: tray \"Stop recording\" could not stop: {err}"
                        );
                    }
                });
            }
            2 => {
                if let Err(err) = crate::dbus::spawn_window_process("main") {
                    eprintln!(
                        "saola-capture: daemon: tray \"Open Saola Capture\" could not open the \
                         app window: {err}"
                    );
                }
            }
            3 => match self.events.clone().try_send(DaemonEvent::QuitRequested) {
                Ok(()) => {}
                Err(_) => {
                    eprintln!(
                        "saola-capture: daemon: tray \"Quit daemon\" could not reach the \
                         daemon's event loop"
                    );
                }
            },
            _ => {}
        }
        Ok(())
    }

    #[zbus(signal)]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn items_properties_updated(
        emitter: &SignalEmitter<'_>,
        updated: Vec<(i32, HashMap<String, OwnedValue>)>,
        removed: Vec<(i32, Vec<String>)>,
    ) -> zbus::Result<()>;
}

// ---------------------------------------------------------------------
// Registration with whatever watcher is (or later becomes) reachable
// ---------------------------------------------------------------------

#[zbus::proxy(
    interface = "org.kde.StatusNotifierWatcher",
    default_service = "org.kde.StatusNotifierWatcher",
    default_path = "/StatusNotifierWatcher"
)]
trait StatusNotifierWatcher {
    fn register_status_notifier_item(&self, service: &str) -> zbus::Result<()>;
}

/// One registration attempt. `true` on success — including "already
/// registered", which the watcher answers idempotently (see
/// `saola-panel::modules::tray::watcher`'s own `RegisterStatusNotifierItem`
/// doc comment: "re-registration is normal"). `false` for anything else (no
/// watcher on the bus at all, a transient failure), logged once rather than
/// escalated — the caller decides whether and when to retry.
async fn try_register(connection: &Connection) -> bool {
    let Ok(proxy) = StatusNotifierWatcherProxy::new(connection).await else {
        return false;
    };
    match proxy
        .register_status_notifier_item(crate::dbus::SERVICE_NAME)
        .await
    {
        Ok(()) => {
            eprintln!("saola-capture: daemon: tray item registered with {WATCHER_BUS_NAME}");
            true
        }
        Err(err) => {
            eprintln!(
                "saola-capture: daemon: no tray host answered yet ({err}) — will retry if one \
                 appears"
            );
            false
        }
    }
}

/// Register once, and if that fails (no host running yet — the common case
/// on a bare niri session before the panel starts), watch
/// `org.freedesktop.DBus`'s `NameOwnerChanged` for [`WATCHER_BUS_NAME`]
/// gaining an owner and retry exactly then. Never polls; degrades to "never
/// registered" (silently — the sibling absent-service rule) if the session
/// bus itself is unreachable, which is the same posture every other
/// best-effort worker in this daemon takes.
async fn register_with_retry(connection: Connection) {
    if try_register(&connection).await {
        return;
    }

    let Ok(dbus) = zbus::fdo::DBusProxy::new(&connection).await else {
        return;
    };
    let Ok(mut owner_changed) = dbus.receive_name_owner_changed().await else {
        return;
    };

    use iced::futures::StreamExt;
    while let Some(signal) = owner_changed.next().await {
        let Ok(args) = signal.args() else { continue };
        let became_owned = args.name()
            == &zbus::names::BusName::from_static_str(WATCHER_BUS_NAME)
                .expect("a valid well-known name literal")
            && args.new_owner().as_ref().is_some();
        if became_owned && try_register(&connection).await {
            return;
        }
    }
}

/// Poll the recorder for an idle/recording transition and emit
/// `NewIcon`/`NewTitle` when one happens — see the module doc comment's
/// "Elapsed time" section for why this is a poll rather than a push, and why
/// that is the honest answer here rather than a shortcut. Never returns;
/// this is the daemon's standing tray worker, alongside
/// [`register_with_retry`] (already finished or still watching by the time
/// this starts).
async fn watch_and_emit(item: InterfaceRef<SniItem>, recorder: SharedRecorder) {
    let mut was_active = crate::dbus::lock_recorder(&recorder).is_active();
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let is_active = crate::dbus::lock_recorder(&recorder).is_active();
        if is_active == was_active {
            continue;
        }
        was_active = is_active;

        let emitter = item.signal_emitter();
        if let Err(err) = SniItem::new_icon(emitter).await {
            eprintln!("saola-capture: daemon: could not emit the tray's NewIcon: {err}");
        }
        if let Err(err) = SniItem::new_title(emitter).await {
            eprintln!("saola-capture: daemon: could not emit the tray's NewTitle: {err}");
        }
    }
}

/// Export the tray's two objects on `connection` and keep them current for
/// the daemon's whole life — `dbus::serve`'s one call into this module, on
/// the `Serving` outcome only (a daemon that lost the name race never gets
/// here at all).
///
/// Exporting failing at all (an object path collision — unreachable in
/// practice, since [`ITEM_OBJECT_PATH`]/[`MENU_OBJECT_PATH`] are this
/// module's own constants and nothing else in this daemon claims them) logs
/// and returns rather than propagating: a tray item that can't be exported
/// is exactly the "no host, no icon" case the sibling rule already covers,
/// not a reason to take the rest of the daemon down with it.
pub(crate) async fn install(
    connection: Connection,
    recorder: SharedRecorder,
    events: iced::futures::channel::mpsc::Sender<DaemonEvent>,
) {
    let item = SniItem {
        recorder: recorder.clone(),
    };
    if let Err(err) = connection.object_server().at(ITEM_OBJECT_PATH, item).await {
        eprintln!("saola-capture: daemon: could not export the tray item: {err}");
        return;
    }

    let menu = TrayMenu {
        recorder: recorder.clone(),
        events,
    };
    if let Err(err) = connection.object_server().at(MENU_OBJECT_PATH, menu).await {
        eprintln!("saola-capture: daemon: could not export the tray menu: {err}");
        return;
    }

    let Ok(item_ref) = connection
        .object_server()
        .interface::<_, SniItem>(ITEM_OBJECT_PATH)
        .await
    else {
        eprintln!("saola-capture: daemon: could not re-acquire the just-exported tray item");
        return;
    };

    register_with_retry(connection).await;
    watch_and_emit(item_ref, recorder).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terracotta() -> saola_theme::tokens::Color {
        saola_theme::Theme::saola().palette.accent
    }

    // -- record_glyph ---------------------------------------------------

    #[test]
    fn a_filled_glyph_has_an_opaque_center_pixel() {
        let (width, height, pixels) = record_glyph(GLYPH_SIZE, true, terracotta());
        assert_eq!((width, height), (GLYPH_SIZE, GLYPH_SIZE));
        let center = (GLYPH_SIZE / 2) as usize;
        let offset = (center * GLYPH_SIZE as usize + center) * 4;
        let alpha = pixels[offset];
        assert!(
            alpha > 0,
            "the center of a filled dot must not be transparent"
        );
    }

    #[test]
    fn an_outline_glyph_has_a_transparent_center_pixel() {
        let (_, _, pixels) = record_glyph(GLYPH_SIZE, false, terracotta());
        let center = (GLYPH_SIZE / 2) as usize;
        let offset = (center * GLYPH_SIZE as usize + center) * 4;
        assert_eq!(
            pixels[offset], 0,
            "the center of an outline (unfilled) glyph must be transparent"
        );
    }

    #[test]
    fn the_four_corners_are_always_transparent() {
        // A circle inscribed in a square never reaches the corners, filled
        // or not — this is really a byte-order + bounds sanity check.
        for filled in [true, false] {
            let (width, height, pixels) = record_glyph(GLYPH_SIZE, filled, terracotta());
            let corner = 0usize;
            assert_eq!(pixels[corner * 4], 0, "top-left corner, filled={filled}");
            let bottom_right = ((height - 1) as usize * width as usize + (width - 1) as usize) * 4;
            assert_eq!(
                pixels[bottom_right], 0,
                "bottom-right corner, filled={filled}"
            );
        }
    }

    #[test]
    fn pixel_bytes_are_argb_network_order() {
        // A filled glyph's center pixel must be `[A, R, G, B]` with the
        // *color's own* channel values, matching the byte order
        // `saola-panel::modules::tray::item::argb_network_to_rgba`'s doc
        // comment documents for the read side.
        let color = saola_theme::tokens::Color::rgba(0x11, 0x22, 0x33, 0xff);
        let (width, _, pixels) = record_glyph(GLYPH_SIZE, true, color);
        let center = (GLYPH_SIZE / 2) as usize;
        let offset = (center * width as usize + center) * 4;
        assert_eq!(
            &pixels[offset..offset + 4],
            &[0xff, 0x11, 0x22, 0x33],
            "expected [A, R, G, B]"
        );
    }

    #[test]
    fn every_pixel_is_fully_opaque_or_fully_transparent() {
        // No antialiasing — a tray icon this small gains nothing from it,
        // and it would double the number of distinct alpha values a test
        // has to reason about for no visible benefit at 24x24.
        let (_, _, pixels) = record_glyph(GLYPH_SIZE, true, terracotta());
        for alpha in pixels.iter().step_by(4) {
            assert!(
                *alpha == 0 || *alpha == 0xff,
                "unexpected partial alpha {alpha}"
            );
        }
    }

    // -- format_elapsed / title_for / tool_tip -----------------------------

    #[test]
    fn format_elapsed_pads_seconds_to_two_digits() {
        assert_eq!(format_elapsed(Duration::from_secs(5)), "0:05");
        assert_eq!(format_elapsed(Duration::from_secs(65)), "1:05");
        assert_eq!(format_elapsed(Duration::from_secs(600)), "10:00");
    }

    #[test]
    fn format_elapsed_truncates_sub_second_remainders() {
        assert_eq!(format_elapsed(Duration::from_millis(59_999)), "0:59");
    }

    #[test]
    fn title_is_plain_when_idle_regardless_of_a_stray_elapsed_value() {
        assert_eq!(
            title_for(false, Some(Duration::from_secs(30))),
            "Saola Capture"
        );
        assert_eq!(title_for(false, None), "Saola Capture");
    }

    #[test]
    fn title_names_the_elapsed_time_while_recording() {
        assert_eq!(
            title_for(true, Some(Duration::from_secs(75))),
            "Saola Capture — Recording 1:15"
        );
    }

    // -- menu content -----------------------------------------------------

    #[test]
    fn the_menu_has_the_three_rows_plan_md_names_in_order() {
        let rows = menu_rows(false);
        assert_eq!(
            rows.map(|(_, label, _)| label),
            ["Stop recording", "Open Saola Capture", "Quit daemon"]
        );
        assert_eq!(rows.map(|(id, ..)| id), [1, 2, 3]);
    }

    #[test]
    fn stop_recording_is_only_enabled_while_active() {
        assert!(!menu_rows(false)[0].2, "disabled while idle");
        assert!(menu_rows(true)[0].2, "enabled while recording");
    }

    #[test]
    fn open_and_quit_are_always_enabled() {
        for recording in [false, true] {
            let rows = menu_rows(recording);
            assert!(rows[1].2, "Open Saola Capture, recording={recording}");
            assert!(rows[2].2, "Quit daemon, recording={recording}");
        }
    }

    #[test]
    fn the_root_node_carries_one_child_per_row() {
        let root = root_node(true);
        assert_eq!(root.id, 0);
        assert_eq!(root.children.len(), 3);
    }

    #[test]
    fn a_leaf_nodes_wire_properties_round_trip() {
        let node = leaf_node(1, "Stop recording", true);
        assert_eq!(node.id, 1);
        assert_eq!(
            String::try_from(node.properties["label"].clone()).unwrap(),
            "Stop recording"
        );
        assert!(bool::try_from(node.properties["enabled"].clone()).unwrap());
        assert!(node.children.is_empty());
    }
}
