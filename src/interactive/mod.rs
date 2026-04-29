pub mod apply;
mod dialog;

pub use dialog::{InteractiveCancelled, choose_apply_candidates_for_manager, ensure_tty_available};
