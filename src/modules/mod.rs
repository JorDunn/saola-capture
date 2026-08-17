//! Daemon-side surface modules — the sibling shape PLAN.md's Architecture
//! names ("Every module maps to a signal, not a poll"): a state struct,
//! `view(&Theme) -> Element`, `subscription()`, and a nested `Message` enum.
//!
//! Stage 6 (PLAN.md, "Flash + toast: the PrintScr MVP") added the first two —
//! [`flash`] (the camera-flash overlay) and [`toast`] (the notification
//! stack) — Stage 7 added the third, [`overlay`] (region selection), and
//! Stage 8 added the fourth, [`countdown`] (the delayed-capture pill). All
//! four are owned by `main.rs`'s `Daemon` and mapped onto layer-shell
//! surfaces via the `SurfaceRole` registry there.
//!
//! [`app`] (PLAN.md Stage 9) is a different shape from the other four: it is
//! not a daemon surface at all, but the **separate-process** window/editor —
//! a plain `iced::application`, run by `main.rs::run_window`, never touching
//! `Daemon`'s `SurfaceRole` registry. Stage 11 adds [`recorder`], the
//! recording state machine PLAN.md's tree sketch names — also not a
//! `SurfaceRole` (it draws nothing). Stage 12 adds [`tray`], which is
//! neither: no surface, no state-machine-with-a-view, just a served D-Bus
//! object — see that module's own doc comment for why it doesn't follow the
//! `view`/`subscription`/`Message` shape at all. Stage 14 adds [`editor`],
//! the real annotation editor rendered inside [`app`]'s `ViewState::Editor`
//! (also not a `SurfaceRole` — it's a body widget of the app window, not a
//! surface of its own); see its own doc comment for the canvas architecture.
//! Stage 16 adds two more window-process modules, following [`editor`]'s
//! shape rather than a daemon `SurfaceRole`'s: [`history`] (the capture
//! library, rendered inside [`app`]'s `ViewState::History`) and [`picker`]
//! (the `PickColor` client — its own async orchestration plus the pure
//! hex/decode helpers, called from *both* `dbus.rs`'s served method and, for
//! the CLI's stdout formatting, `main.rs`).

pub mod app;
pub mod countdown;
pub mod editor;
pub mod flash;
pub mod history;
pub mod overlay;
pub mod picker;
pub mod recorder;
pub mod toast;
pub mod tray;
