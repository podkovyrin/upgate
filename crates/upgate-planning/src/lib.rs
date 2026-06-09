//! Planning crate for the `upgate` rebuild.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

pub mod evaluate;
pub mod planner;

pub use evaluate::{audit_queries_for_seed, evaluate_seed, evaluate_seed_with_audit};
pub use planner::{
    PlanningSettings, default_batch_selection, derive_audit_queries, finalize_plan_from_inputs,
    update_plan_from_inputs,
};
