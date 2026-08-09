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
//! `Daemon`'s `SurfaceRole` registry. Stages 11/12 add the recording module
//! PLAN.md's tree sketch names.

pub mod app;
pub mod countdown;
pub mod flash;
pub mod overlay;
pub mod recorder;
pub mod toast;
