//! Planning crate for the `upgate` rebuild.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

mod classify;
pub mod evaluate;
pub mod planner;

pub use evaluate::evaluate_seed_with_audit;
pub use planner::{
    PlanningSettings, default_batch_selection, derive_audit_queries, finalize_plan_from_inputs,
};
