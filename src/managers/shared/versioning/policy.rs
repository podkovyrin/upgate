#![allow(dead_code)]

use std::cmp::Ordering;
use std::time::Duration;

use anyhow::{Result, bail};

#[allow(unused_imports)]
pub use super::classification::{ReleaseClass, classify_pep440_release, classify_semver_release};

/// Version-policy gate values used by manager policy config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VersionPolicy {
    /// Preserve current behavior: skip version-policy filtering.
    #[default]
    Disabled,
    Stable,
    SameTrack,
    Any,
}

impl VersionPolicy {
    /// Parse manager-scoped optional config value.
    /// `None` intentionally means "policy disabled" to preserve legacy behavior.
    pub fn parse_optional_for(manager_id: &str, raw: Option<&str>) -> Result<Self> {
        raw.map_or(Ok(Self::Disabled), |raw| Self::parse_for(manager_id, raw))
    }

    /// Parse a required manager-scoped config value and attach manager context
    /// to the validation error string.
    pub fn parse_for(manager_id: &str, raw: &str) -> Result<Self> {
        Self::parse_config_value(raw)
            .map_err(|err| anyhow::anyhow!("Invalid version_policy for [{manager_id}]: {err}"))
    }

    fn parse_config_value(raw: &str) -> Result<Self> {
        match raw {
            "stable" => Ok(Self::Stable),
            "same-track" => Ok(Self::SameTrack),
            "any" => Ok(Self::Any),
            _ => bail!("expected one of \"stable\", \"same-track\", \"any\", got \"{raw}\""),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Stable => "stable",
            Self::SameTrack => "same-track",
            Self::Any => "any",
        }
    }
}

/// Conservative fallback states used when `same-track` cannot safely infer an installed track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyWarning {
    InstalledTrackUnknownFallbackStable,
}

/// Reasons why the version-policy gate denied a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyBlockReason {
    NotFinal,
    LessStableThanInstalled,
    UnknownStability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyDecision {
    pub allowed: bool,
    pub effective_policy: VersionPolicy,
    pub block_reason: Option<PolicyBlockReason>,
    pub warning: Option<PolicyWarning>,
}

impl PolicyDecision {
    const fn allow(policy: VersionPolicy, warning: Option<PolicyWarning>) -> Self {
        Self {
            allowed: true,
            effective_policy: policy,
            block_reason: None,
            warning,
        }
    }

    const fn block(
        policy: VersionPolicy,
        reason: PolicyBlockReason,
        warning: Option<PolicyWarning>,
    ) -> Self {
        Self {
            allowed: false,
            effective_policy: policy,
            block_reason: Some(reason),
            warning,
        }
    }
}

/// One candidate version plus classification/timestamp metadata.
#[derive(Debug, Clone)]
pub struct OrderedCandidate<T> {
    /// Original display string for this candidate version.
    pub version: String,
    /// Parsed form used for ordering/newer-than checks.
    pub parsed: T,
    /// Precomputed release-class classification for this version string.
    pub release_class: ReleaseClass,
    /// Publish timestamp used by the min-release-age gate.
    pub published_unix: u64,
}

/// One-run bypass switches for future interactive-force flows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GateBypass {
    /// When true, treat version-policy gate as pass for all candidates.
    pub version_policy: bool,
    /// When true, treat min-release-age gate as pass for all candidates.
    pub min_release_age: bool,
}

impl GateBypass {
    pub const NONE: Self = Self {
        version_policy: false,
        min_release_age: false,
    };

    pub const ALL: Self = Self {
        version_policy: true,
        min_release_age: true,
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateEvaluation {
    /// Candidate version string.
    pub version: String,
    /// Candidate release class used by policy evaluation.
    pub release_class: ReleaseClass,
    /// Candidate age in seconds at evaluation time.
    pub age_secs: u64,
    /// Raw version-policy decision without bypass.
    pub policy_allowed: bool,
    /// Raw age-gate decision without bypass.
    pub age_allowed: bool,
    /// Final decision after applying bypass flags to both gates.
    pub effective_allowed: bool,
    /// Present when policy gate blocked this candidate without bypass.
    pub policy_block_reason: Option<PolicyBlockReason>,
    /// Policy warning emitted during policy evaluation, if any.
    /// Note: we intentionally do not duplicate an `effective_policy` field here.
    /// The configured policy is a run-level resolver input; this warning carries
    /// fallback context (for example `SameTrack` -> `Stable`) when needed.
    pub policy_warning: Option<PolicyWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecommendedOutcome {
    Update { target_version: String },
    DelayedByAge,
    CurrentNoNewer,
    CurrentBlockedByPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionPolicyResolution {
    /// Resolver recommendation after applying policy and age gates.
    pub recommendation: RecommendedOutcome,
    /// Newest newer candidate regardless of policy/age eligibility.
    pub latest_overall_version: Option<String>,
    /// Age of `latest_overall_version` in seconds.
    pub latest_overall_age_secs: Option<u64>,
    /// Newest candidate that passed the effective policy gate.
    pub latest_policy_eligible_version: Option<String>,
    /// Newest candidate that passed both effective policy and effective age gates.
    pub latest_age_eligible_version: Option<String>,
    /// Whether at least one strictly newer candidate exists.
    pub has_newer_versions: bool,
    /// Count of newer candidates blocked by effective policy gate.
    pub blocked_by_policy_count: usize,
    /// Count of newer candidates blocked by effective age gate after policy pass.
    pub blocked_by_age_count: usize,
    /// Per-candidate gate results for all newer candidates, sorted newest-first.
    pub evaluations: Vec<CandidateEvaluation>,
}

/// Evaluate policy eligibility for one candidate class.
pub const fn evaluate_version_policy(
    policy: VersionPolicy,
    installed_class: ReleaseClass,
    candidate_class: ReleaseClass,
) -> PolicyDecision {
    match policy {
        VersionPolicy::Disabled | VersionPolicy::Any => PolicyDecision::allow(policy, None),
        VersionPolicy::Stable => {
            if candidate_class.is_final() {
                PolicyDecision::allow(policy, None)
            } else {
                PolicyDecision::block(policy, PolicyBlockReason::NotFinal, None)
            }
        }
        VersionPolicy::SameTrack => {
            match installed_class {
                ReleaseClass::UnknownPrerelease | ReleaseClass::Unknown => {
                    return evaluate_version_policy_fallback_stable(candidate_class);
                }
                _ => {}
            }

            let Some(installed_rank) = installed_class.stability_rank() else {
                return evaluate_version_policy_fallback_stable(candidate_class);
            };

            if candidate_class.is_final() {
                return PolicyDecision::allow(policy, None);
            }

            let Some(candidate_rank) = candidate_class.stability_rank() else {
                return PolicyDecision::block(policy, PolicyBlockReason::UnknownStability, None);
            };

            if candidate_rank >= installed_rank {
                PolicyDecision::allow(policy, None)
            } else {
                PolicyDecision::block(policy, PolicyBlockReason::LessStableThanInstalled, None)
            }
        }
    }
}

/// Evaluate and select candidate versions using option-A ordering:
/// newer candidates -> version policy -> release-age gate -> newest remaining.
///
/// The returned `evaluations` field keeps every newer candidate with gate verdicts,
/// so future interactive flows can surface ineligible candidates and bypass one or both gates.
pub fn evaluate_candidates<T>(
    current: &T,
    candidates: &[OrderedCandidate<T>],
    installed_class: ReleaseClass,
    policy: VersionPolicy,
    now_unix_secs: u64,
    min_age: Duration,
    bypass: GateBypass,
) -> VersionPolicyResolution
where
    T: Ord + Clone,
{
    let mut selected: Option<(T, String)> = None;
    let mut newest_overall: Option<(T, String, u64)> = None;
    let mut newest_policy_eligible: Option<(T, String)> = None;
    let mut newest_age_eligible: Option<(T, String)> = None;
    let mut has_effective_policy_eligible = false;

    let mut blocked_by_policy_count = 0usize;
    let mut blocked_by_age_count = 0usize;

    let mut evaluated: Vec<(T, CandidateEvaluation)> = Vec::new();

    for candidate in candidates {
        if candidate.parsed <= *current {
            continue;
        }

        let policy_decision =
            evaluate_version_policy(policy, installed_class, candidate.release_class);
        let age_secs = now_unix_secs.saturating_sub(candidate.published_unix);
        let age_allowed = age_secs >= min_age.as_secs();

        let effective_policy_allowed = bypass.version_policy || policy_decision.allowed;
        let effective_age_allowed = bypass.min_release_age || age_allowed;
        if effective_policy_allowed {
            has_effective_policy_eligible = true;
        }

        if effective_policy_allowed {
            update_newest_pair(
                &mut newest_policy_eligible,
                &candidate.parsed,
                &candidate.version,
            );
            if effective_age_allowed {
                update_newest_pair(
                    &mut newest_age_eligible,
                    &candidate.parsed,
                    &candidate.version,
                );
            }
        }

        if !effective_policy_allowed {
            blocked_by_policy_count += 1;
        }
        if effective_policy_allowed && !effective_age_allowed {
            blocked_by_age_count += 1;
        }

        if effective_policy_allowed && effective_age_allowed {
            update_newest_pair(&mut selected, &candidate.parsed, &candidate.version);
        }

        update_newest_triplet(
            &mut newest_overall,
            &candidate.parsed,
            &candidate.version,
            age_secs,
        );

        evaluated.push((
            candidate.parsed.clone(),
            CandidateEvaluation {
                version: candidate.version.clone(),
                release_class: candidate.release_class,
                age_secs,
                policy_allowed: policy_decision.allowed,
                age_allowed,
                effective_allowed: effective_policy_allowed && effective_age_allowed,
                policy_block_reason: policy_decision.block_reason,
                policy_warning: policy_decision.warning,
            },
        ));
    }

    evaluated.sort_by(|(left_parsed, _), (right_parsed, _)| right_parsed.cmp(left_parsed));

    let evaluations = evaluated
        .into_iter()
        .map(|(_, eval)| eval)
        .collect::<Vec<_>>();

    let has_newer_versions = newest_overall.is_some();
    let (latest_overall_version, latest_overall_age_secs) = newest_overall
        .as_ref()
        .map_or((None, None), |(_, version, age_secs)| {
            (Some(version.clone()), Some(*age_secs))
        });
    let recommendation = if let Some((_, target_version)) = selected {
        RecommendedOutcome::Update { target_version }
    } else if !has_newer_versions {
        RecommendedOutcome::CurrentNoNewer
    } else if has_effective_policy_eligible {
        RecommendedOutcome::DelayedByAge
    } else {
        RecommendedOutcome::CurrentBlockedByPolicy
    };

    VersionPolicyResolution {
        recommendation,
        latest_overall_version,
        latest_overall_age_secs,
        latest_policy_eligible_version: newest_policy_eligible.map(|(_, version)| version),
        latest_age_eligible_version: newest_age_eligible.map(|(_, version)| version),
        has_newer_versions,
        blocked_by_policy_count,
        blocked_by_age_count,
        evaluations,
    }
}

const fn evaluate_version_policy_fallback_stable(candidate_class: ReleaseClass) -> PolicyDecision {
    if candidate_class.is_final() {
        return PolicyDecision::allow(
            VersionPolicy::Stable,
            Some(PolicyWarning::InstalledTrackUnknownFallbackStable),
        );
    }

    PolicyDecision::block(
        VersionPolicy::Stable,
        PolicyBlockReason::NotFinal,
        Some(PolicyWarning::InstalledTrackUnknownFallbackStable),
    )
}

fn update_newest_pair<T>(slot: &mut Option<(T, String)>, parsed: &T, version: &str)
where
    T: Ord + Clone,
{
    if slot
        .as_ref()
        .is_none_or(|(best_parsed, _)| parsed.cmp(best_parsed) == Ordering::Greater)
    {
        *slot = Some((parsed.clone(), version.to_string()));
    }
}

fn update_newest_triplet<T>(
    slot: &mut Option<(T, String, u64)>,
    parsed: &T,
    version: &str,
    age_secs: u64,
) where
    T: Ord + Clone,
{
    if slot
        .as_ref()
        .is_none_or(|(best_parsed, _, _)| parsed.cmp(best_parsed) == Ordering::Greater)
    {
        *slot = Some((parsed.clone(), version.to_string(), age_secs));
    }
}

#[cfg(test)]
mod tests {
    use semver::Version as SemverVersion;

    use super::*;

    #[test]
    fn parse_optional_policy_defaults_to_disabled() {
        let policy = VersionPolicy::parse_optional_for("npm", None).expect("should parse");
        assert_eq!(policy, VersionPolicy::Disabled);
    }

    #[test]
    fn parse_config_policy_rejects_unknown_values() {
        let err = VersionPolicy::parse_for("npm", "beta-only")
            .expect_err("invalid policy value should fail");
        assert_eq!(
            err.to_string(),
            "Invalid version_policy for [npm]: expected one of \"stable\", \"same-track\", \"any\", got \"beta-only\""
        );
    }

    #[test]
    fn disabled_policy_keeps_prerelease_candidates_eligible() {
        let current = SemverVersion::parse("1.2.0").expect("current semver should parse");
        let candidates = vec![OrderedCandidate {
            version: "1.3.0-beta.1".to_string(),
            parsed: SemverVersion::parse("1.3.0-beta.1").expect("candidate should parse"),
            release_class: ReleaseClass::Beta,
            published_unix: 9_000,
        }];

        let resolved = evaluate_candidates(
            &current,
            &candidates,
            ReleaseClass::Final,
            VersionPolicy::Disabled,
            10_000,
            Duration::from_secs(0),
            GateBypass::NONE,
        );

        assert_eq!(
            resolved.recommendation,
            RecommendedOutcome::Update {
                target_version: "1.3.0-beta.1".to_string(),
            }
        );
        assert_eq!(resolved.blocked_by_policy_count, 0);
    }

    #[test]
    fn any_policy_allows_unknown_prerelease_candidate_classes() {
        let decision = evaluate_version_policy(
            VersionPolicy::Any,
            ReleaseClass::Final,
            ReleaseClass::UnknownPrerelease,
        );

        assert!(decision.allowed);
        assert_eq!(decision.block_reason, None);
        assert_eq!(decision.warning, None);
    }

    #[test]
    fn stable_policy_allows_only_final() {
        let final_decision = evaluate_version_policy(
            VersionPolicy::Stable,
            ReleaseClass::Final,
            ReleaseClass::Final,
        );
        let beta_decision = evaluate_version_policy(
            VersionPolicy::Stable,
            ReleaseClass::Final,
            ReleaseClass::Beta,
        );

        assert!(final_decision.allowed);
        assert!(!beta_decision.allowed);
        assert_eq!(
            beta_decision.block_reason,
            Some(PolicyBlockReason::NotFinal)
        );
    }

    #[test]
    fn stable_policy_blocks_unknown_candidate_classes_as_not_final() {
        for candidate_class in [ReleaseClass::UnknownPrerelease, ReleaseClass::Unknown] {
            let decision = evaluate_version_policy(
                VersionPolicy::Stable,
                ReleaseClass::Final,
                candidate_class,
            );

            assert!(!decision.allowed, "candidate={candidate_class:?}");
            assert_eq!(
                decision.block_reason,
                Some(PolicyBlockReason::NotFinal),
                "candidate={candidate_class:?}"
            );
            assert_eq!(decision.warning, None, "candidate={candidate_class:?}");
            assert_eq!(
                decision.effective_policy,
                VersionPolicy::Stable,
                "candidate={candidate_class:?}"
            );
        }
    }

    #[test]
    fn same_track_blocks_less_stable_candidates() {
        let allow_rc = evaluate_version_policy(
            VersionPolicy::SameTrack,
            ReleaseClass::Beta,
            ReleaseClass::Rc,
        );
        let block_alpha = evaluate_version_policy(
            VersionPolicy::SameTrack,
            ReleaseClass::Beta,
            ReleaseClass::Alpha,
        );

        assert!(allow_rc.allowed);
        assert!(!block_alpha.allowed);
        assert_eq!(
            block_alpha.block_reason,
            Some(PolicyBlockReason::LessStableThanInstalled)
        );
    }

    #[test]
    fn same_track_respects_stability_ladder_boundaries_for_known_tracks() {
        let known_tracks = [
            ReleaseClass::Dev,
            ReleaseClass::Alpha,
            ReleaseClass::Beta,
            ReleaseClass::Rc,
            ReleaseClass::Final,
        ];

        for (installed_idx, installed_class) in known_tracks.iter().copied().enumerate() {
            for (candidate_idx, candidate_class) in known_tracks.iter().copied().enumerate() {
                let decision = evaluate_version_policy(
                    VersionPolicy::SameTrack,
                    installed_class,
                    candidate_class,
                );

                let should_allow = candidate_idx >= installed_idx;
                assert_eq!(
                    decision.allowed, should_allow,
                    "installed={installed_class:?} candidate={candidate_class:?}"
                );
                assert_eq!(
                    decision.block_reason,
                    if should_allow {
                        None
                    } else {
                        Some(PolicyBlockReason::LessStableThanInstalled)
                    },
                    "installed={installed_class:?} candidate={candidate_class:?}"
                );
                assert_eq!(
                    decision.effective_policy,
                    VersionPolicy::SameTrack,
                    "installed={installed_class:?} candidate={candidate_class:?}"
                );
                assert_eq!(
                    decision.warning, None,
                    "installed={installed_class:?} candidate={candidate_class:?}"
                );
            }

            for candidate_class in [ReleaseClass::UnknownPrerelease, ReleaseClass::Unknown] {
                let decision = evaluate_version_policy(
                    VersionPolicy::SameTrack,
                    installed_class,
                    candidate_class,
                );

                assert!(
                    !decision.allowed,
                    "installed={installed_class:?} candidate={candidate_class:?}"
                );
                assert_eq!(
                    decision.block_reason,
                    Some(PolicyBlockReason::UnknownStability),
                    "installed={installed_class:?} candidate={candidate_class:?}"
                );
                assert_eq!(
                    decision.effective_policy,
                    VersionPolicy::SameTrack,
                    "installed={installed_class:?} candidate={candidate_class:?}"
                );
                assert_eq!(
                    decision.warning, None,
                    "installed={installed_class:?} candidate={candidate_class:?}"
                );
            }
        }
    }

    #[test]
    fn same_track_falls_back_to_stable_when_installed_track_unknown() {
        let decision = evaluate_version_policy(
            VersionPolicy::SameTrack,
            ReleaseClass::UnknownPrerelease,
            ReleaseClass::Rc,
        );

        assert!(!decision.allowed);
        assert_eq!(decision.effective_policy, VersionPolicy::Stable);
        assert_eq!(
            decision.warning,
            Some(PolicyWarning::InstalledTrackUnknownFallbackStable)
        );
        assert_eq!(decision.block_reason, Some(PolicyBlockReason::NotFinal));
    }

    #[test]
    fn same_track_falls_back_to_stable_when_installed_class_is_unknown() {
        let decision = evaluate_version_policy(
            VersionPolicy::SameTrack,
            ReleaseClass::Unknown,
            ReleaseClass::Rc,
        );

        assert!(!decision.allowed);
        assert_eq!(decision.effective_policy, VersionPolicy::Stable);
        assert_eq!(
            decision.warning,
            Some(PolicyWarning::InstalledTrackUnknownFallbackStable)
        );
        assert_eq!(decision.block_reason, Some(PolicyBlockReason::NotFinal));
    }

    #[test]
    fn same_track_blocks_unknown_candidate_stability_when_installed_track_is_known() {
        let decision = evaluate_version_policy(
            VersionPolicy::SameTrack,
            ReleaseClass::Beta,
            ReleaseClass::UnknownPrerelease,
        );

        assert!(!decision.allowed);
        assert_eq!(
            decision.block_reason,
            Some(PolicyBlockReason::UnknownStability)
        );
        assert_eq!(decision.warning, None);
    }

    #[test]
    fn option_a_order_blocks_prerelease_before_age() {
        let current = SemverVersion::parse("1.2.0").expect("current semver should parse");
        let candidates = vec![
            OrderedCandidate {
                version: "1.3.0-beta.1".to_string(),
                parsed: SemverVersion::parse("1.3.0-beta.1").expect("candidate should parse"),
                release_class: ReleaseClass::Beta,
                published_unix: 9_900,
            },
            OrderedCandidate {
                version: "1.2.5".to_string(),
                parsed: SemverVersion::parse("1.2.5").expect("candidate should parse"),
                release_class: ReleaseClass::Final,
                published_unix: 9_997,
            },
        ];

        let resolved = evaluate_candidates(
            &current,
            &candidates,
            ReleaseClass::Final,
            VersionPolicy::Stable,
            10_000,
            Duration::from_secs(5),
            GateBypass::NONE,
        );

        assert_eq!(resolved.recommendation, RecommendedOutcome::DelayedByAge);
        assert_eq!(
            resolved.latest_overall_version.as_deref(),
            Some("1.3.0-beta.1")
        );
        assert_eq!(
            resolved.latest_policy_eligible_version.as_deref(),
            Some("1.2.5")
        );
        assert_eq!(resolved.latest_age_eligible_version, None);
        assert_eq!(resolved.blocked_by_policy_count, 1);
        assert_eq!(resolved.blocked_by_age_count, 1);
    }

    #[test]
    fn candidate_evaluations_are_sorted_newest_first_even_if_input_order_differs() {
        let current = SemverVersion::parse("1.2.0").expect("current semver should parse");
        let candidates = vec![
            OrderedCandidate {
                version: "1.2.5".to_string(),
                parsed: SemverVersion::parse("1.2.5").expect("candidate should parse"),
                release_class: ReleaseClass::Final,
                published_unix: 9_995,
            },
            OrderedCandidate {
                version: "1.3.0-beta.1".to_string(),
                parsed: SemverVersion::parse("1.3.0-beta.1").expect("candidate should parse"),
                release_class: ReleaseClass::Beta,
                published_unix: 9_999,
            },
            OrderedCandidate {
                version: "1.2.7".to_string(),
                parsed: SemverVersion::parse("1.2.7").expect("candidate should parse"),
                release_class: ReleaseClass::Final,
                published_unix: 9_996,
            },
        ];

        let resolved = evaluate_candidates(
            &current,
            &candidates,
            ReleaseClass::Final,
            VersionPolicy::Any,
            10_000,
            Duration::from_secs(0),
            GateBypass::NONE,
        );

        let ordered_versions = resolved
            .evaluations
            .iter()
            .map(|eval| eval.version.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ordered_versions, vec!["1.3.0-beta.1", "1.2.7", "1.2.5"]);
    }

    #[test]
    fn bypass_allows_selecting_otherwise_ineligible_candidate() {
        let current = SemverVersion::parse("1.2.0").expect("current semver should parse");
        let candidates = vec![
            OrderedCandidate {
                version: "1.3.0-beta.1".to_string(),
                parsed: SemverVersion::parse("1.3.0-beta.1").expect("candidate should parse"),
                release_class: ReleaseClass::Beta,
                published_unix: 9_999,
            },
            OrderedCandidate {
                version: "1.2.5".to_string(),
                parsed: SemverVersion::parse("1.2.5").expect("candidate should parse"),
                release_class: ReleaseClass::Final,
                published_unix: 9_997,
            },
        ];

        let resolved = evaluate_candidates(
            &current,
            &candidates,
            ReleaseClass::Final,
            VersionPolicy::Stable,
            10_000,
            Duration::from_secs(7),
            GateBypass::ALL,
        );

        assert_eq!(
            resolved.recommendation,
            RecommendedOutcome::Update {
                target_version: "1.3.0-beta.1".to_string(),
            }
        );
        assert_eq!(
            resolved.latest_policy_eligible_version.as_deref(),
            Some("1.3.0-beta.1")
        );
        assert_eq!(
            resolved.latest_age_eligible_version.as_deref(),
            Some("1.3.0-beta.1")
        );
        assert_eq!(resolved.blocked_by_policy_count, 0);
        assert_eq!(resolved.blocked_by_age_count, 0);
    }

    #[test]
    fn bypass_policy_only_keeps_delayed_by_age_and_consistent_policy_metadata() {
        let current = SemverVersion::parse("1.2.0").expect("current semver should parse");
        let candidates = vec![OrderedCandidate {
            version: "1.3.0-beta.1".to_string(),
            parsed: SemverVersion::parse("1.3.0-beta.1").expect("candidate should parse"),
            release_class: ReleaseClass::Beta,
            published_unix: 9_999,
        }];

        let resolved = evaluate_candidates(
            &current,
            &candidates,
            ReleaseClass::Final,
            VersionPolicy::Stable,
            10_000,
            Duration::from_secs(7),
            GateBypass {
                version_policy: true,
                min_release_age: false,
            },
        );

        assert_eq!(resolved.recommendation, RecommendedOutcome::DelayedByAge);
        assert_eq!(
            resolved.latest_policy_eligible_version.as_deref(),
            Some("1.3.0-beta.1")
        );
        assert_eq!(resolved.latest_age_eligible_version, None);
        assert_eq!(resolved.blocked_by_policy_count, 0);
        assert_eq!(resolved.blocked_by_age_count, 1);
    }

    #[test]
    fn all_newer_candidates_blocked_by_policy_keeps_current() {
        let current = SemverVersion::parse("1.2.0").expect("current semver should parse");
        let candidates = vec![OrderedCandidate {
            version: "1.3.0-beta.1".to_string(),
            parsed: SemverVersion::parse("1.3.0-beta.1").expect("candidate should parse"),
            release_class: ReleaseClass::Beta,
            published_unix: 9_000,
        }];

        let resolved = evaluate_candidates(
            &current,
            &candidates,
            ReleaseClass::Final,
            VersionPolicy::Stable,
            10_000,
            Duration::from_secs(5),
            GateBypass::NONE,
        );

        assert_eq!(
            resolved.recommendation,
            RecommendedOutcome::CurrentBlockedByPolicy
        );
        assert_eq!(resolved.blocked_by_policy_count, 1);
        assert_eq!(resolved.latest_policy_eligible_version, None);
    }

    #[test]
    fn no_newer_candidates_keeps_current() {
        let current = SemverVersion::parse("1.2.0").expect("current semver should parse");
        let candidates = vec![OrderedCandidate {
            version: "1.2.0".to_string(),
            parsed: SemverVersion::parse("1.2.0").expect("candidate should parse"),
            release_class: ReleaseClass::Final,
            published_unix: 9_000,
        }];

        let resolved = evaluate_candidates(
            &current,
            &candidates,
            ReleaseClass::Final,
            VersionPolicy::Stable,
            10_000,
            Duration::from_secs(5),
            GateBypass::NONE,
        );

        assert_eq!(resolved.recommendation, RecommendedOutcome::CurrentNoNewer);
        assert!(!resolved.has_newer_versions);
        assert_eq!(resolved.latest_overall_version, None);
    }
}
