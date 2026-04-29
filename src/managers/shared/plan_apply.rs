use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Error, Result};

use super::{
    ApplyCandidate, PlanMeta, ResolvedPlanTarget, emit_manager_level_error,
    emit_plan_and_collect_apply_candidates, plan_decision_from_resolution,
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

pub fn collect_apply_candidates_from_resolved_plan<T>(
    manager: &'static str,
    plan: Vec<ResolvedPlanItem<T>>,
    min_age: Duration,
    suppress_update_outcomes: bool,
    pinned: &BTreeSet<String>,
    supports_exact_versions: bool,
) -> Vec<ApplyCandidate>
where
    T: ResolvedPlanTarget,
{
    emit_plan_and_collect_apply_candidates(
        plan,
        |item| {
            let ResolvedPlanItem {
                name,
                current,
                resolved,
            } = item;
            let decision = plan_decision_from_resolution(resolved, min_age);

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
        supports_exact_versions,
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
    use crate::managers::shared::ResolvedPlanTarget;
    use crate::managers::shared::versioning::policy::RecommendedOutcome;

    #[derive(Clone)]
    struct MockTarget {
        recommendation: RecommendedOutcome,
    }

    impl ResolvedPlanTarget for MockTarget {
        fn recommendation(&self) -> &RecommendedOutcome {
            &self.recommendation
        }

        fn latest_version(&self) -> Option<&str> {
            None
        }

        fn latest_age_secs(&self) -> Option<u64> {
            None
        }
    }

    #[derive(Clone)]
    struct ForceMockTarget {
        recommendation: RecommendedOutcome,
        latest_version: Option<&'static str>,
    }

    impl ResolvedPlanTarget for ForceMockTarget {
        fn recommendation(&self) -> &RecommendedOutcome {
            &self.recommendation
        }

        fn latest_version(&self) -> Option<&str> {
            self.latest_version
        }

        fn latest_age_secs(&self) -> Option<u64> {
            Some(0)
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
                    recommendation: RecommendedOutcome::Update {
                        target_version: "1.1.0".to_string(),
                    },
                }),
            ),
            ResolvedPlanItem::new(
                "bar",
                "2.0.0",
                Ok(MockTarget {
                    recommendation: RecommendedOutcome::Update {
                        target_version: "2.1.0".to_string(),
                    },
                }),
            ),
        ];

        let upgradable: Vec<_> = collect_apply_candidates_from_resolved_plan(
            "mock", plan, min_age, false, &pinned, true,
        )
        .into_iter()
        .filter(ApplyCandidate::is_visible_by_default)
        .map(ApplyCandidate::into_update)
        .collect();

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
                    recommendation: RecommendedOutcome::Update {
                        target_version: "1.1.0".to_string(),
                    },
                }),
            ),
            ResolvedPlanItem::new(
                "bar",
                "2.0.0",
                Ok(MockTarget {
                    recommendation: RecommendedOutcome::Update {
                        target_version: "2.1.0".to_string(),
                    },
                }),
            ),
        ];

        let upgradable: Vec<_> =
            collect_apply_candidates_from_resolved_plan("mock", plan, min_age, true, &pinned, true)
                .into_iter()
                .filter(ApplyCandidate::is_visible_by_default)
                .map(ApplyCandidate::into_update)
                .collect();

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
                    recommendation: RecommendedOutcome::CurrentNoNewer,
                }),
            ),
            ResolvedPlanItem::new("broken", "1.0.0", Err("boom".to_string())),
        ];

        let upgradable: Vec<_> = collect_apply_candidates_from_resolved_plan(
            "mock", plan, min_age, false, &pinned, true,
        )
        .into_iter()
        .filter(ApplyCandidate::is_visible_by_default)
        .map(ApplyCandidate::into_update)
        .collect();

        assert!(upgradable.is_empty());
    }

    #[test]
    fn collects_delayed_force_candidate_as_hidden() {
        let min_age = Duration::from_secs(3_600);
        let pinned = BTreeSet::new();
        let plan = vec![ResolvedPlanItem::new(
            "fresh",
            "1.0.0",
            Ok(ForceMockTarget {
                recommendation: RecommendedOutcome::DelayedByAge,
                latest_version: Some("1.1.0"),
            }),
        )];

        let candidates = collect_apply_candidates_from_resolved_plan(
            "mock", plan, min_age, false, &pinned, true,
        );

        assert_eq!(candidates.len(), 1);
        assert!(!candidates[0].is_visible_by_default());
        let update = candidates.into_iter().next().unwrap().into_update();
        assert_eq!(update.name, "fresh");
        assert_eq!(update.target, "1.1.0");
    }

    #[test]
    fn skips_force_candidate_when_target_is_unknown() {
        let min_age = Duration::from_secs(3_600);
        let pinned = BTreeSet::new();
        let plan = vec![ResolvedPlanItem::new(
            "unknown",
            "1.0.0",
            Ok(ForceMockTarget {
                recommendation: RecommendedOutcome::DelayedByAge,
                latest_version: None,
            }),
        )];

        let candidates = collect_apply_candidates_from_resolved_plan(
            "mock", plan, min_age, false, &pinned, true,
        );

        assert!(candidates.is_empty());
    }
}
