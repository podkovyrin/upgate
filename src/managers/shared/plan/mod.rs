mod collect;
mod decision;
mod emit;
mod types;

pub use collect::emit_plan_and_collect_upgradable;
pub use decision::{ResolvedPlanTarget, plan_decision_from_resolution};
pub use emit::{
    emit_manager_level_error, emit_manager_level_error_with, emit_scan_current,
    emit_version_scan_outcomes, soft_fail, soft_fail_or, verbose_now_unix_secs,
};
pub use types::{PlanMeta, PlannedUpdate};
