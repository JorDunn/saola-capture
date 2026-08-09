//! Daemon-side surface modules — the sibling shape PLAN.md's Architecture
//! names ("Every module maps to a signal, not a poll"): a state struct,
//! `view(&Theme) -> Element`, `subscription()`, and a nested `Message` enum.
//!
//! Stage 6 (PLAN.md, "Flash + toast: the PrintScr MVP") added the first two —
//! [`flash`] (the camera-flash overlay) and [`toast`] (the notification
//! stack) — Stage 7 added the third, [`overlay`] (region selection), and
//! Stage 8 adds the fourth, [`countdown`] (the delayed-capture pill). All
//! four are owned by `main.rs`'s `Daemon` and mapped onto layer-shell
//! surfaces via the `SurfaceRole` registry there. Stages 9/11/12 add the
//! window-process and recording modules PLAN.md's tree sketch names.

pub mod countdown;
pub mod flash;
pub mod overlay;
pub mod toast;
