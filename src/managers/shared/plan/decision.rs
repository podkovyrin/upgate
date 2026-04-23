use std::time::Duration;

use super::types::{AgeResolvedTarget, DelayedLatest, PlanDecision, VersionPolicyMeta};
use crate::util::time::human_age;

pub trait ResolvedPlanTarget {
    fn selected_version(&self) -> Option<&str>;
    fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest>;
    fn current_blocked_by_policy(&self) -> bool {
        false
    }
    fn version_policy(&self) -> Option<&str> {
        None
    }
    fn latest_blocked_by_policy_version(&self) -> Option<&str> {
        None
    }
    fn version_policy_warning(&self) -> Option<&str> {
        None
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

    fn version_policy(&self) -> Option<&str> {
        self.version_policy.as_deref()
    }

    fn latest_blocked_by_policy_version(&self) -> Option<&str> {
        self.latest_blocked_by_policy_version.as_deref()
    }

    fn version_policy_warning(&self) -> Option<&str> {
        self.version_policy_warning.as_deref()
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
                version_policy: version_policy_meta(&target),
            },
            Some(selected) if selected == current_version && target.current_blocked_by_policy() => {
                PlanDecision::CurrentBlockedByPolicy {
                    version_policy: version_policy_meta(&target).unwrap_or_else(|| {
                        VersionPolicyMeta {
                            policy: "unknown".to_string(),
                            latest_blocked_version: None,
                            warning: None,
                        }
                    }),
                }
            }
            Some(selected) if selected == current_version => PlanDecision::NoChange,
            Some(selected) => PlanDecision::Update {
                target: selected.to_string(),
                delayed_latest: target.delayed_latest(min_age),
                version_policy: version_policy_meta(&target),
            },
        },
        Err(err) => PlanDecision::Error(err),
    }
}

fn version_policy_meta<T>(target: &T) -> Option<VersionPolicyMeta>
where
    T: ResolvedPlanTarget,
{
    target.version_policy().map(|policy| VersionPolicyMeta {
        policy: policy.to_string(),
        latest_blocked_version: target
            .latest_blocked_by_policy_version()
            .map(str::to_string),
        warning: target.version_policy_warning().map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::shared::DelayedLatest;

    #[derive(Clone)]
    struct MockTarget {
        selected: Option<&'static str>,
        delayed_latest: Option<DelayedLatest>,
        blocked_by_policy: bool,
        version_policy: Option<&'static str>,
        latest_blocked_by_policy_version: Option<&'static str>,
        version_policy_warning: Option<&'static str>,
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

        fn version_policy(&self) -> Option<&str> {
            self.version_policy
        }

        fn latest_blocked_by_policy_version(&self) -> Option<&str> {
            self.latest_blocked_by_policy_version
        }

        fn version_policy_warning(&self) -> Option<&str> {
            self.version_policy_warning
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
                version_policy: None,
                latest_blocked_by_policy_version: None,
                version_policy_warning: None,
            }),
            Duration::from_secs(7_200),
        );

        match decision {
            PlanDecision::DelayedNoEligible {
                required_age,
                delayed_latest,
                version_policy,
            } => {
                assert_eq!(required_age, "2h");
                let delayed = delayed_latest.expect("expected delayed latest metadata");
                assert_eq!(delayed.latest_version, "1.1.0");
                assert_eq!(delayed.latest_age, "1h");
                assert!(version_policy.is_none());
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
                version_policy: None,
                latest_blocked_by_policy_version: None,
                version_policy_warning: None,
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
                version_policy: None,
                latest_blocked_by_policy_version: None,
                version_policy_warning: None,
            }),
            Duration::from_secs(604_800),
        );

        match decision {
            PlanDecision::Update {
                target,
                delayed_latest,
                version_policy,
            } => {
                assert_eq!(target, "1.2.0");
                let delayed = delayed_latest.expect("expected delayed latest metadata");
                assert_eq!(delayed.latest_version, "1.3.0");
                assert!(version_policy.is_none());
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
                version_policy: Some("stable"),
                latest_blocked_by_policy_version: Some("1.3.0-beta.1"),
                version_policy_warning: Some(
                    "same-track fell back to stable because installed track is unknown",
                ),
            }),
            Duration::from_secs(3_600),
        );

        match decision {
            PlanDecision::CurrentBlockedByPolicy { version_policy } => {
                assert_eq!(version_policy.policy, "stable");
                assert_eq!(
                    version_policy.latest_blocked_version.as_deref(),
                    Some("1.3.0-beta.1")
                );
                assert_eq!(
                    version_policy.warning.as_deref(),
                    Some("same-track fell back to stable because installed track is unknown")
                );
            }
            _ => panic!("expected PlanDecision::CurrentBlockedByPolicy"),
        }
    }
}
