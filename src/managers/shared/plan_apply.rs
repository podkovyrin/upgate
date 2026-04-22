use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Error, Result};

use super::{
    PlanMeta, PlannedUpdate, ResolvedPlanTarget, emit_manager_level_error,
    emit_plan_and_collect_upgradable, plan_decision_from_resolution,
};
use crate::managers::runtime::ManagerCtx;
use crate::util::time::now_unix_secs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepFailurePolicy {
    EmitAndContinue,
    FailManager,
}

#[derive(Debug, Clone, Copy)]
pub struct PlanApplyFrameworkPolicy {
    pub fetch_failure: StepFailurePolicy,
    pub resolve_failure: StepFailurePolicy,
}

#[derive(Debug)]
pub struct PlanApplyRuntime<'a> {
    pub now_unix_secs: u64,
    pub min_age: Duration,
    pub max_parallel_checks: usize,
    pub suppress_update_outcomes: bool,
    pub pinned: &'a BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedPlanItem<T> {
    pub name: String,
    pub current: String,
    pub resolved: Result<T, String>,
}

impl<T> ResolvedPlanItem<T> {
    pub fn new(
        name: impl Into<String>,
        current: impl Into<String>,
        resolved: Result<T, String>,
    ) -> Self {
        Self {
            name: name.into(),
            current: current.into(),
            resolved,
        }
    }
}

impl PlanApplyFrameworkPolicy {
    pub const SOFT_FETCH_STRICT_RESOLVE: Self = Self {
        fetch_failure: StepFailurePolicy::EmitAndContinue,
        resolve_failure: StepFailurePolicy::FailManager,
    };

    pub const SOFT_FETCH_SOFT_RESOLVE: Self = Self {
        fetch_failure: StepFailurePolicy::EmitAndContinue,
        resolve_failure: StepFailurePolicy::EmitAndContinue,
    };

    pub const STRICT_FETCH_STRICT_RESOLVE: Self = Self {
        fetch_failure: StepFailurePolicy::FailManager,
        resolve_failure: StepFailurePolicy::FailManager,
    };
}

#[allow(clippy::too_many_arguments)]
pub fn run_plan_apply_framework<Discovered, Resolved, Collected, Fetch, IsEmpty>(
    ctx: &ManagerCtx,
    manager: &'static str,
    policy: PlanApplyFrameworkPolicy,
    fetch_discovered: Fetch,
    is_empty: IsEmpty,
    resolve_plan: impl FnOnce(&Discovered, &PlanApplyRuntime<'_>) -> Result<Resolved>,
    collect_result: impl FnOnce(&Discovered, Resolved, &PlanApplyRuntime<'_>) -> Result<Collected>,
    apply_updates: impl FnOnce(&ManagerCtx, &Discovered, Collected) -> Result<()>,
) -> Result<()>
where
    Fetch: FnOnce() -> Result<Discovered>,
    IsEmpty: Fn(&Discovered) -> bool,
{
    let discovered = match fetch_discovered() {
        Ok(discovered) => discovered,
        Err(detail) => return handle_step_error(manager, detail, policy.fetch_failure),
    };

    if is_empty(&discovered) {
        return Ok(());
    }

    let runtime = PlanApplyRuntime {
        now_unix_secs: now_unix_secs()?,
        min_age: ctx.policy.min_release_age.duration(),
        max_parallel_checks: ctx.max_parallel_checks,
        suppress_update_outcomes: ctx.is_interactive_apply(),
        pinned: &ctx.policy.pinned,
    };

    let resolved = match resolve_plan(&discovered, &runtime) {
        Ok(resolved) => resolved,
        Err(detail) => return handle_step_error(manager, detail, policy.resolve_failure),
    };

    let collected = collect_result(&discovered, resolved, &runtime)?;

    apply_updates(ctx, &discovered, collected)
}

pub fn collect_upgradable_from_resolved_plan<T>(
    manager: &'static str,
    plan: Vec<ResolvedPlanItem<T>>,
    min_age: Duration,
    suppress_update_outcomes: bool,
    pinned: &BTreeSet<String>,
) -> Vec<PlannedUpdate>
where
    T: ResolvedPlanTarget,
{
    emit_plan_and_collect_upgradable(
        plan,
        |item| {
            let ResolvedPlanItem {
                name,
                current,
                resolved,
            } = item;
            let decision = plan_decision_from_resolution(&current, resolved, min_age);

            (
                PlanMeta {
                    manager,
                    name,
                    current,
                },
                decision,
            )
        },
        suppress_update_outcomes,
        Some(pinned),
    )
}

fn handle_step_error(manager: &'static str, err: Error, policy: StepFailurePolicy) -> Result<()> {
    match policy {
        StepFailurePolicy::EmitAndContinue => {
            emit_manager_level_error(manager, format!("{err:#}"));
            Ok(())
        }
        StepFailurePolicy::FailManager => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::shared::{DelayedLatest, ResolvedPlanTarget};

    #[derive(Clone)]
    struct MockTarget {
        selected: Option<&'static str>,
        delayed_latest: Option<DelayedLatest>,
    }

    impl ResolvedPlanTarget for MockTarget {
        fn selected_version(&self) -> Option<&str> {
            self.selected
        }

        fn delayed_latest(&self, _min_age: Duration) -> Option<DelayedLatest> {
            self.delayed_latest.clone()
        }
    }

    #[test]
    fn collects_updates_and_skips_pinned_when_not_suppressed() {
        let min_age = Duration::from_secs(3_600);
        let pinned = BTreeSet::from(["bar".to_string()]);
        let plan = vec![
            ResolvedPlanItem::new(
                "foo",
                "1.0.0",
                Ok(MockTarget {
                    selected: Some("1.1.0"),
                    delayed_latest: None,
                }),
            ),
            ResolvedPlanItem::new(
                "bar",
                "2.0.0",
                Ok(MockTarget {
                    selected: Some("2.1.0"),
                    delayed_latest: None,
                }),
            ),
        ];

        let upgradable =
            collect_upgradable_from_resolved_plan("mock", plan, min_age, false, &pinned);

        assert_eq!(upgradable.len(), 1);
        assert_eq!(upgradable[0].name, "foo");
        assert_eq!(upgradable[0].target, "1.1.0");
    }

    #[test]
    fn keeps_pinned_updates_when_suppressed_for_interactive_mode() {
        let min_age = Duration::from_secs(3_600);
        let pinned = BTreeSet::from(["bar".to_string()]);
        let plan = vec![
            ResolvedPlanItem::new(
                "foo",
                "1.0.0",
                Ok(MockTarget {
                    selected: Some("1.1.0"),
                    delayed_latest: None,
                }),
            ),
            ResolvedPlanItem::new(
                "bar",
                "2.0.0",
                Ok(MockTarget {
                    selected: Some("2.1.0"),
                    delayed_latest: None,
                }),
            ),
        ];

        let upgradable =
            collect_upgradable_from_resolved_plan("mock", plan, min_age, true, &pinned);

        assert_eq!(upgradable.len(), 2);
        assert_eq!(upgradable[0].name, "foo");
        assert_eq!(upgradable[1].name, "bar");
    }

    #[test]
    fn does_not_collect_no_change_or_error_items() {
        let min_age = Duration::from_secs(3_600);
        let pinned = BTreeSet::new();
        let plan = vec![
            ResolvedPlanItem::new(
                "same",
                "1.0.0",
                Ok(MockTarget {
                    selected: Some("1.0.0"),
                    delayed_latest: None,
                }),
            ),
            ResolvedPlanItem::new("broken", "1.0.0", Err("boom".to_string())),
        ];

        let upgradable =
            collect_upgradable_from_resolved_plan("mock", plan, min_age, false, &pinned);

        assert!(upgradable.is_empty());
    }
}
