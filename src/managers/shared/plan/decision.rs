use std::time::Duration;

use super::types::{AgeResolvedTarget, DelayedLatest, PlanDecision};
use crate::util::time::human_age;

pub trait ResolvedPlanTarget {
    fn selected_version(&self) -> Option<&str>;
    fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest>;
    fn current_blocked_by_policy(&self) -> bool {
        false
    }
}

impl ResolvedPlanTarget for AgeResolvedTarget {
    fn selected_version(&self) -> Option<&str> {
        self.selected_version.as_deref()
    }

    fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest> {
        Self::delayed_latest(self, min_age)
    }

    fn current_blocked_by_policy(&self) -> bool {
        self.current_blocked_by_policy
    }
}

pub fn plan_decision_from_resolution<T>(
    current_version: &str,
    resolved: Result<T, String>,
    min_age: Duration,
) -> PlanDecision
where
    T: ResolvedPlanTarget,
{
    match resolved {
        Ok(target) => match target.selected_version() {
            None => PlanDecision::DelayedNoEligible {
                required_age: human_age(min_age.as_secs()),
                delayed_latest: target.delayed_latest(min_age),
            },
            Some(selected) if selected == current_version && target.current_blocked_by_policy() => {
                PlanDecision::CurrentBlockedByPolicy
            }
            Some(selected) if selected == current_version => PlanDecision::NoChange,
            Some(selected) => PlanDecision::Update {
                target: selected.to_string(),
                delayed_latest: target.delayed_latest(min_age),
            },
        },
        Err(err) => PlanDecision::Error(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::shared::{DelayedLatest, PlanDecision};

    #[derive(Clone)]
    struct MockTarget {
        selected: Option<&'static str>,
        delayed_latest: Option<DelayedLatest>,
        blocked_by_policy: bool,
    }

    impl ResolvedPlanTarget for MockTarget {
        fn selected_version(&self) -> Option<&str> {
            self.selected
        }

        fn delayed_latest(&self, _min_age: Duration) -> Option<DelayedLatest> {
            self.delayed_latest.clone()
        }

        fn current_blocked_by_policy(&self) -> bool {
            self.blocked_by_policy
        }
    }

    #[test]
    fn returns_error_when_resolution_fails() {
        let decision = plan_decision_from_resolution::<MockTarget>(
            "1.0.0",
            Err("resolver failed".to_string()),
            Duration::from_secs(7_200),
        );

        match decision {
            PlanDecision::Error(err) => assert_eq!(err, "resolver failed"),
            _ => panic!("expected PlanDecision::Error"),
        }
    }

    #[test]
    fn returns_delayed_no_eligible_when_target_missing() {
        let decision = plan_decision_from_resolution(
            "1.0.0",
            Ok(MockTarget {
                selected: None,
                delayed_latest: Some(DelayedLatest {
                    latest_version: "1.1.0".to_string(),
                    latest_age: "1h".to_string(),
                    required_age: "2h".to_string(),
                }),
                blocked_by_policy: false,
            }),
            Duration::from_secs(7_200),
        );

        match decision {
            PlanDecision::DelayedNoEligible {
                required_age,
                delayed_latest,
            } => {
                assert_eq!(required_age, "2h");
                let delayed = delayed_latest.expect("expected delayed latest metadata");
                assert_eq!(delayed.latest_version, "1.1.0");
                assert_eq!(delayed.latest_age, "1h");
            }
            _ => panic!("expected PlanDecision::DelayedNoEligible"),
        }
    }

    #[test]
    fn returns_no_change_when_selected_matches_current() {
        let decision = plan_decision_from_resolution(
            "1.0.0",
            Ok(MockTarget {
                selected: Some("1.0.0"),
                delayed_latest: None,
                blocked_by_policy: false,
            }),
            Duration::from_secs(3_600),
        );

        assert!(matches!(decision, PlanDecision::NoChange));
    }

    #[test]
    fn returns_update_when_selected_differs() {
        let decision = plan_decision_from_resolution(
            "1.0.0",
            Ok(MockTarget {
                selected: Some("1.2.0"),
                delayed_latest: Some(DelayedLatest {
                    latest_version: "1.3.0".to_string(),
                    latest_age: "1h".to_string(),
                    required_age: "7d".to_string(),
                }),
                blocked_by_policy: false,
            }),
            Duration::from_secs(604_800),
        );

        match decision {
            PlanDecision::Update {
                target,
                delayed_latest,
            } => {
                assert_eq!(target, "1.2.0");
                let delayed = delayed_latest.expect("expected delayed latest metadata");
                assert_eq!(delayed.latest_version, "1.3.0");
            }
            _ => panic!("expected PlanDecision::Update"),
        }
    }

    #[test]
    fn returns_current_blocked_by_policy_when_selected_matches_current_and_policy_blocks_newer() {
        let decision = plan_decision_from_resolution(
            "1.0.0",
            Ok(MockTarget {
                selected: Some("1.0.0"),
                delayed_latest: None,
                blocked_by_policy: true,
            }),
            Duration::from_secs(3_600),
        );

        assert!(matches!(decision, PlanDecision::CurrentBlockedByPolicy));
    }
}
