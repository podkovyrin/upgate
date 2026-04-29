use std::time::Duration;

use super::types::{CandidateVersionMeta, DelayedLatest, PlanDecision, VersionPolicyMeta};
use crate::managers::shared::versioning::policy::{
    CandidateEvaluation, PolicyWarning, RecommendedOutcome, VersionPolicy, VersionPolicyResolution,
};
use crate::outcome::DelayedReason;
use crate::util::time::human_age;

pub trait ResolvedPlanTarget {
    fn recommendation(&self) -> &RecommendedOutcome;
    fn latest_version(&self) -> Option<&str>;
    fn latest_age_secs(&self) -> Option<u64>;
    fn force_candidate_version(&self) -> Option<&str> {
        self.latest_version()
    }
    fn candidate_evaluations(&self) -> &[CandidateEvaluation] {
        &[]
    }
    fn version_policy(&self) -> Option<VersionPolicy> {
        None
    }
    fn latest_blocked_by_policy_version(&self) -> Option<&str> {
        None
    }
    fn version_policy_warning(&self) -> Option<PolicyWarning> {
        None
    }
    fn blocked_by_policy_count(&self) -> usize {
        0
    }
    fn blocked_by_age_count(&self) -> usize {
        0
    }
}

impl ResolvedPlanTarget for VersionPolicyResolution {
    fn recommendation(&self) -> &RecommendedOutcome {
        &self.recommendation
    }

    fn latest_version(&self) -> Option<&str> {
        self.latest_policy_eligible_version.as_deref()
    }

    fn latest_age_secs(&self) -> Option<u64> {
        self.latest_policy_eligible_age_secs
    }

    fn force_candidate_version(&self) -> Option<&str> {
        self.latest_overall_version.as_deref()
    }

    fn candidate_evaluations(&self) -> &[CandidateEvaluation] {
        &self.evaluations
    }

    fn version_policy(&self) -> Option<VersionPolicy> {
        Self::configured_policy(self)
    }

    fn latest_blocked_by_policy_version(&self) -> Option<&str> {
        Self::latest_blocked_by_policy_version(self)
    }

    fn version_policy_warning(&self) -> Option<PolicyWarning> {
        Self::version_policy_warning(self)
    }

    fn blocked_by_policy_count(&self) -> usize {
        self.blocked_by_policy_count
    }

    fn blocked_by_age_count(&self) -> usize {
        self.blocked_by_age_count
    }
}

pub fn plan_decision_from_resolution<T>(
    resolved: Result<T, String>,
    min_age: Duration,
) -> PlanDecision
where
    T: ResolvedPlanTarget,
{
    match resolved {
        Ok(target) => match target.recommendation() {
            RecommendedOutcome::Update { target_version } => PlanDecision::Update {
                target: target_version.clone(),
                delayed_latest: delayed_latest_for(&target, Some(target_version.as_str()), min_age),
                version_policy: version_policy_meta(&target),
                candidate_versions: candidate_versions_for(&target),
            },
            RecommendedOutcome::DelayedByAge => PlanDecision::DelayedNoEligible {
                required_age: human_age(min_age.as_secs()),
                delayed_latest: delayed_latest_for(&target, None, min_age),
                delayed_reason: delayed_reason_for(&target),
                version_policy: version_policy_meta(&target),
                force_target: target.force_candidate_version().map(str::to_string),
                candidate_versions: candidate_versions_for(&target),
            },
            RecommendedOutcome::CurrentNoNewer => PlanDecision::NoChange,
            RecommendedOutcome::CurrentBlockedByPolicy => PlanDecision::CurrentBlockedByPolicy {
                version_policy: version_policy_meta(&target).unwrap_or(VersionPolicyMeta {
                    policy: VersionPolicy::Disabled,
                    latest_blocked_version: None,
                    warning: None,
                }),
                force_target: target.force_candidate_version().map(str::to_string),
                candidate_versions: candidate_versions_for(&target),
            },
        },
        Err(err) => PlanDecision::Error(err),
    }
}

fn candidate_versions_for<T>(target: &T) -> Vec<CandidateVersionMeta>
where
    T: ResolvedPlanTarget,
{
    target
        .candidate_evaluations()
        .iter()
        .map(|candidate| CandidateVersionMeta {
            version: candidate.version.clone(),
            age: human_age(candidate.age_secs),
            policy_allowed: candidate.policy_allowed,
            age_allowed: candidate.age_allowed,
            policy_block_reason: candidate.policy_block_reason,
            policy_warning: candidate.policy_warning,
        })
        .collect()
}

fn delayed_latest_for<T>(
    target: &T,
    selected_version: Option<&str>,
    min_age: Duration,
) -> Option<DelayedLatest>
where
    T: ResolvedPlanTarget,
{
    let latest_version = target.latest_version()?;
    let latest_age_secs = target.latest_age_secs()?;

    if selected_version == Some(latest_version) {
        return None;
    }

    DelayedLatest::new_if_fresh(latest_version, latest_age_secs, min_age)
}

fn version_policy_meta<T>(target: &T) -> Option<VersionPolicyMeta>
where
    T: ResolvedPlanTarget,
{
    target.version_policy().map(|policy| VersionPolicyMeta {
        policy,
        latest_blocked_version: target
            .latest_blocked_by_policy_version()
            .map(str::to_string),
        warning: target.version_policy_warning(),
    })
}

fn delayed_reason_for<T>(target: &T) -> DelayedReason
where
    T: ResolvedPlanTarget,
{
    if target.blocked_by_policy_count() > 0 && target.blocked_by_age_count() > 0 {
        DelayedReason::NoPolicyAndAgeEligibleRelease
    } else {
        DelayedReason::NoAgeEligibleRelease
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct MockTarget {
        recommendation: RecommendedOutcome,
        latest_version: Option<&'static str>,
        latest_age_secs: Option<u64>,
        version_policy: Option<VersionPolicy>,
        latest_blocked_by_policy_version: Option<&'static str>,
        version_policy_warning: Option<PolicyWarning>,
    }

    impl ResolvedPlanTarget for MockTarget {
        fn recommendation(&self) -> &RecommendedOutcome {
            &self.recommendation
        }

        fn latest_version(&self) -> Option<&str> {
            self.latest_version
        }

        fn latest_age_secs(&self) -> Option<u64> {
            self.latest_age_secs
        }

        fn version_policy(&self) -> Option<VersionPolicy> {
            self.version_policy
        }

        fn latest_blocked_by_policy_version(&self) -> Option<&str> {
            self.latest_blocked_by_policy_version
        }

        fn version_policy_warning(&self) -> Option<PolicyWarning> {
            self.version_policy_warning
        }
    }

    #[derive(Clone)]
    struct CountedMockTarget {
        recommendation: RecommendedOutcome,
        latest_version: Option<&'static str>,
        latest_age_secs: Option<u64>,
        blocked_by_policy_count: usize,
        blocked_by_age_count: usize,
    }

    impl ResolvedPlanTarget for CountedMockTarget {
        fn recommendation(&self) -> &RecommendedOutcome {
            &self.recommendation
        }

        fn latest_version(&self) -> Option<&str> {
            self.latest_version
        }

        fn latest_age_secs(&self) -> Option<u64> {
            self.latest_age_secs
        }

        fn blocked_by_policy_count(&self) -> usize {
            self.blocked_by_policy_count
        }

        fn blocked_by_age_count(&self) -> usize {
            self.blocked_by_age_count
        }
    }

    #[test]
    fn returns_error_when_resolution_fails() {
        let decision = plan_decision_from_resolution::<MockTarget>(
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
            Ok(MockTarget {
                recommendation: RecommendedOutcome::DelayedByAge,
                latest_version: Some("1.1.0"),
                latest_age_secs: Some(3_600),
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
                delayed_reason,
                version_policy,
                force_target,
                candidate_versions,
            } => {
                assert_eq!(required_age, "2h");
                assert_eq!(delayed_reason, DelayedReason::NoAgeEligibleRelease);
                let delayed = delayed_latest.expect("expected delayed latest metadata");
                assert_eq!(delayed.latest_version, "1.1.0");
                assert_eq!(delayed.latest_age, "1h");
                assert!(version_policy.is_none());
                assert_eq!(force_target.as_deref(), Some("1.1.0"));
                assert!(candidate_versions.is_empty());
            }
            _ => panic!("expected PlanDecision::DelayedNoEligible"),
        }
    }

    #[test]
    fn delayed_reason_tracks_combined_policy_and_age_blocks() {
        let decision = plan_decision_from_resolution(
            Ok(CountedMockTarget {
                recommendation: RecommendedOutcome::DelayedByAge,
                latest_version: Some("1.1.0"),
                latest_age_secs: Some(3_600),
                blocked_by_policy_count: 1,
                blocked_by_age_count: 1,
            }),
            Duration::from_secs(7_200),
        );

        match decision {
            PlanDecision::DelayedNoEligible { delayed_reason, .. } => {
                assert_eq!(delayed_reason, DelayedReason::NoPolicyAndAgeEligibleRelease);
            }
            _ => panic!("expected PlanDecision::DelayedNoEligible"),
        }
    }

    #[test]
    fn returns_no_change_when_recommendation_is_current_no_newer() {
        let decision = plan_decision_from_resolution(
            Ok(MockTarget {
                recommendation: RecommendedOutcome::CurrentNoNewer,
                latest_version: None,
                latest_age_secs: None,
                version_policy: None,
                latest_blocked_by_policy_version: None,
                version_policy_warning: None,
            }),
            Duration::from_secs(3_600),
        );

        assert!(matches!(decision, PlanDecision::NoChange));
    }

    #[test]
    fn returns_update_when_recommendation_has_target() {
        let decision = plan_decision_from_resolution(
            Ok(MockTarget {
                recommendation: RecommendedOutcome::Update {
                    target_version: "1.2.0".to_string(),
                },
                latest_version: Some("1.3.0"),
                latest_age_secs: Some(3_600),
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
                candidate_versions,
            } => {
                assert_eq!(target, "1.2.0");
                let delayed = delayed_latest.expect("expected delayed latest metadata");
                assert_eq!(delayed.latest_version, "1.3.0");
                assert!(version_policy.is_none());
                assert!(candidate_versions.is_empty());
            }
            _ => panic!("expected PlanDecision::Update"),
        }
    }

    #[test]
    fn returns_current_blocked_by_policy_when_recommendation_blocks_current() {
        let decision = plan_decision_from_resolution(
            Ok(MockTarget {
                recommendation: RecommendedOutcome::CurrentBlockedByPolicy,
                latest_version: None,
                latest_age_secs: None,
                version_policy: Some(VersionPolicy::Stable),
                latest_blocked_by_policy_version: Some("1.3.0-beta.1"),
                version_policy_warning: Some(PolicyWarning::InstalledTrackUnknownFallbackStable),
            }),
            Duration::from_secs(3_600),
        );

        match decision {
            PlanDecision::CurrentBlockedByPolicy {
                version_policy,
                force_target,
                candidate_versions,
            } => {
                assert_eq!(version_policy.policy, VersionPolicy::Stable);
                assert_eq!(
                    version_policy.latest_blocked_version.as_deref(),
                    Some("1.3.0-beta.1")
                );
                assert_eq!(
                    version_policy.warning,
                    Some(PolicyWarning::InstalledTrackUnknownFallbackStable)
                );
                assert!(force_target.is_none());
                assert!(candidate_versions.is_empty());
            }
            _ => panic!("expected PlanDecision::CurrentBlockedByPolicy"),
        }
    }

    #[test]
    fn update_without_fresher_latest_has_no_delayed_metadata() {
        let decision = plan_decision_from_resolution(
            Ok(MockTarget {
                recommendation: RecommendedOutcome::Update {
                    target_version: "1.2.0".to_string(),
                },
                latest_version: Some("1.2.0"),
                latest_age_secs: Some(86_400 * 10),
                version_policy: None,
                latest_blocked_by_policy_version: None,
                version_policy_warning: None,
            }),
            Duration::from_secs(86_400 * 7),
        );

        match decision {
            PlanDecision::Update { delayed_latest, .. } => {
                assert!(delayed_latest.is_none());
            }
            _ => panic!("expected PlanDecision::Update"),
        }
    }
}
