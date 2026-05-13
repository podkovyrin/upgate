//! Planning crate for the `upnow` rebuild.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

pub mod evaluate;
pub mod planner;
pub mod selection_view;

pub use evaluate::evaluate_seed;
pub use planner::{
    PlanningSettings, default_batch_selection, update_plan_from_inputs, update_plan_from_seeds,
};
pub use selection_view::{
    CandidateNoteKind, CandidateNotePart, SelectionRow, SelectionRowStatus, SelectionRowVisibility,
    SelectionView, TargetOption, selection_view,
};
