use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use upgate_domain::{
    AdvisoryLatestFact, AdvisoryReleaseLookup, AuditLookupResult, AuditQuery, AuditSubject,
    BlockReason, CandidateAgeFact, CandidateEvaluationFact, DelayReason, ManagerSelectedTarget,
    MissingMetadataKind, PlanDiagnostics, PlanItem, PlanItemId, PlannedTarget, PolicyBlockReason,
    PolicyWarning, ReleaseEntry, ReleaseLookupResult, ReleaseTimeline, TargetAgeEvidence,
    TargetAgeLookupResult, TargetSelection, UpdateCandidate, UpdateSeed, VersionPolicy,
    VersionScheme, VersionText,
};

use crate::classify::{
    ReleaseClass, classify_manager_native_release, classify_pep440_release,
    classify_semver_release, parse_pep440, parse_semver,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum PolicyDecision {
    Allowed {
        warning: Option<PolicyWarning>,
    },
    Blocked {
        reason: PolicyBlockReason,
        warning: Option<PolicyWarning>,
    },
}

impl PolicyDecision {
    const fn warning(&self) -> Option<PolicyWarning> {
        match self {
            Self::Allowed { warning } | Self::Blocked { warning, .. } => *warning,
        }
    }

    fn block_reason(&self) -> Option<PolicyBlockReason> {
        match self {
            Self::Allowed { .. } => None,
            Self::Blocked { reason, .. } => Some(reason.clone()),
        }
    }
}

/// Evaluate one manager-discovered update seed into a typed plan item using
/// previously looked-up audit evidence.
pub fn evaluate_seed_with_audit(
    id: PlanItemId,
    seed: UpdateSeed,
    policy: VersionPolicy,
    now: SystemTime,
    min_release_age: Duration,
    audit_results: &BTreeMap<AuditQuery, AuditLookupResult>,
) -> PlanItem {
    match seed.target_selection.clone() {
        TargetSelection::PlannerSelectable {
            discovered_target,
            release_lookup,
        } => evaluate_planner_selectable_seed(
            id,
            seed,
            &discovered_target,
            release_lookup,
            policy,
            now,
            min_release_age,
            audit_results,
        ),
        TargetSelection::ManagerSelected(target) => evaluate_manager_selected_seed(
            id,
            seed,
            target,
            policy,
            now,
            min_release_age,
            audit_results,
        ),
    }
}

pub(crate) fn audit_queries_for_seed(seed: &UpdateSeed) -> Vec<AuditQuery> {
    match &seed.target_selection {
        TargetSelection::PlannerSelectable {
            discovered_target,
            release_lookup,
        } => audit_queries_for_planner_selectable_seed(seed, discovered_target, release_lookup),
        TargetSelection::ManagerSelected(target) => {
            audit_queries_for_manager_selected_seed(seed, target)
        }
    }
}

fn audit_queries_for_planner_selectable_seed(
    seed: &UpdateSeed,
    discovered_target: &VersionText,
    release_lookup: &ReleaseLookupResult,
) -> Vec<AuditQuery> {
    let Some(subject) = seed.installed.audit_subject.as_ref() else {
        return Vec::new();
    };
    let ReleaseLookupResult::Known(timeline) = release_lookup else {
        return Vec::new();
    };

    match seed.version_scheme {
        VersionScheme::SemVer => {
            audit_queries_for_timeline(seed, subject, discovered_target, timeline, parse_semver)
        }
        VersionScheme::Pep440 => {
            audit_queries_for_timeline(seed, subject, discovered_target, timeline, parse_pep440)
        }
        VersionScheme::ManagerNative => Vec::new(),
    }
}

fn audit_queries_for_manager_selected_seed(
    seed: &UpdateSeed,
    target: &ManagerSelectedTarget,
) -> Vec<AuditQuery> {
    let Some(subject) = seed.installed.audit_subject.as_ref() else {
        return Vec::new();
    };
    let Some(selected_target) = target.target_version() else {
        return Vec::new();
    };
    if !selected_target_is_update(seed, selected_target).unwrap_or(false) {
        return Vec::new();
    }

    vec![AuditQuery::new(subject.clone(), selected_target.clone())]
}

fn audit_queries_for_timeline<V: Ord>(
    seed: &UpdateSeed,
    subject: &AuditSubject,
    discovered_target: &VersionText,
    timeline: &ReleaseTimeline,
    parse: fn(&str) -> Result<V, String>,
) -> Vec<AuditQuery> {
    let Ok(installed_version) = parse(seed.installed.installed_version.as_str()) else {
        return Vec::new();
    };
    let Ok(parsed_discovered_target) = parse(discovered_target.as_str()) else {
        return Vec::new();
    };
    if !timeline.versions.iter().any(|entry| {
        parse(entry.version.as_str())
            .map(|parsed| parsed == parsed_discovered_target)
            .unwrap_or(false)
    }) {
        return Vec::new();
    }

    timeline
        .versions
        .iter()
        .filter_map(|entry| {
            let parsed = parse(entry.version.as_str()).ok()?;
            if parsed <= installed_version {
                return None;
            }
            Some(AuditQuery::new(subject.clone(), entry.version.clone()))
        })
        .collect()
}

#[expect(clippy::too_many_arguments)]
fn evaluate_planner_selectable_seed(
    id: PlanItemId,
    seed: UpdateSeed,
    discovered_target: &VersionText,
    release_lookup: ReleaseLookupResult,
    policy: VersionPolicy,
    now: SystemTime,
    min_release_age: Duration,
    audit_results: &BTreeMap<AuditQuery, AuditLookupResult>,
) -> PlanItem {
    match release_lookup {
        ReleaseLookupResult::MissingMetadata => PlanItem::Blocked {
            id,
            seed,
            reason: BlockReason::MissingReleaseMetadata,
            policy_warnings: Vec::new(),
            diagnostics: PlanDiagnostics::new(min_release_age)
                .with_missing_metadata(MissingMetadataKind::ReleaseTimeline),
        },
        ReleaseLookupResult::LookupFailed(err) => PlanItem::Blocked {
            id,
            seed,
            reason: BlockReason::ReleaseLookupFailed,
            policy_warnings: Vec::new(),
            diagnostics: PlanDiagnostics::new(min_release_age).with_lookup_failure(err),
        },
        ReleaseLookupResult::Known(timeline) => match seed.version_scheme {
            VersionScheme::SemVer => evaluate_timeline_seed(
                id,
                seed,
                discovered_target,
                &timeline,
                policy,
                now,
                min_release_age,
                audit_results,
                parse_semver,
                classify_semver_release,
                false,
            ),
            VersionScheme::Pep440 => evaluate_timeline_seed(
                id,
                seed,
                discovered_target,
                &timeline,
                policy,
                now,
                min_release_age,
                audit_results,
                parse_pep440,
                classify_pep440_release,
                true,
            ),
            VersionScheme::ManagerNative => PlanItem::ResolverError {
                id,
                installed: seed.installed,
                message: "manager-native evaluation requires manager-specific planner".to_owned(),
            },
        },
    }
}

#[expect(clippy::too_many_lines)]
fn evaluate_manager_selected_seed(
    id: PlanItemId,
    seed: UpdateSeed,
    target: ManagerSelectedTarget,
    policy: VersionPolicy,
    now: SystemTime,
    min_release_age: Duration,
    audit_results: &BTreeMap<AuditQuery, AuditLookupResult>,
) -> PlanItem {
    let ManagerSelectedTarget {
        target,
        target_age,
        advisory_release_lookup,
        advisory_lookup_failure,
    } = target;
    let mut diagnostics = PlanDiagnostics::new(min_release_age);
    diagnostics.advisory_latest =
        advisory_latest_diagnostics(advisory_release_lookup.as_ref(), now);
    diagnostics.advisory_lookup_failure = advisory_lookup_failure;
    let PlannedTarget::Known(selected_target) = target else {
        return PlanItem::Blocked {
            id,
            seed,
            reason: BlockReason::MissingReleaseMetadata,
            policy_warnings: Vec::new(),
            diagnostics: diagnostics.with_missing_metadata(MissingMetadataKind::SelectedUpdate),
        };
    };
    match selected_target_is_update(&seed, &selected_target) {
        Ok(false) => {
            return PlanItem::Current {
                id,
                installed: seed.installed,
            };
        }
        Ok(true) => {}
        Err(message) => {
            return PlanItem::ResolverError {
                id,
                installed: seed.installed,
                message,
            };
        }
    }

    let installed_class = classify_release(
        seed.version_scheme,
        seed.installed.installed_version.as_str(),
    );
    let target_class = classify_release(seed.version_scheme, selected_target.as_str());
    let policy_decision = evaluate_policy(policy, installed_class, target_class);
    let candidate_fact = CandidateEvaluationFact {
        version: selected_target.clone(),
        age: target_age_duration(&target_age, now),
        age_allowed: target_age_is_old_enough(&target_age, now, min_release_age),
        policy_block_reason: policy_decision.block_reason(),
        policy_warning: policy_decision.warning(),
        audit: audit_fact_for(
            seed.installed.audit_subject.as_ref(),
            &selected_target,
            audit_results,
        ),
    };
    let policy_warning = match policy_decision {
        PolicyDecision::Allowed { warning } => warning,
        PolicyDecision::Blocked { reason, warning } => {
            diagnostics.candidates.push(candidate_fact);
            return PlanItem::Blocked {
                id,
                seed,
                reason: BlockReason::VersionPolicy(reason),
                policy_warnings: warning.into_iter().collect(),
                diagnostics,
            };
        }
    };

    let target_age = match target_age {
        TargetAgeLookupResult::Known(evidence) => {
            diagnostics.selected_target = Some(CandidateAgeFact::new(
                selected_target.clone(),
                evidence_age(&evidence, now),
            ));
            evidence
        }
        TargetAgeLookupResult::MissingMetadata => {
            return PlanItem::Blocked {
                id,
                seed,
                reason: BlockReason::MissingReleaseMetadata,
                policy_warnings: Vec::new(),
                diagnostics: diagnostics.with_missing_metadata(MissingMetadataKind::SelectedUpdate),
            };
        }
        TargetAgeLookupResult::LookupFailed(err) => {
            return PlanItem::Blocked {
                id,
                seed,
                reason: BlockReason::ReleaseLookupFailed,
                policy_warnings: Vec::new(),
                diagnostics: diagnostics.with_lookup_failure(err),
            };
        }
    };

    if is_evidence_old_enough(&target_age, now, min_release_age) {
        if let Some(audit) = candidate_fact.audit.clone()
            && !audit_allows_target(&audit)
        {
            diagnostics.audit_blocking_target = Some(audit.clone());
            diagnostics.audit_blocking_candidate = Some(candidate_fact.clone());
            diagnostics.candidates.push(candidate_fact);
            return PlanItem::Blocked {
                id,
                seed,
                reason: audit_block_reason(&audit),
                policy_warnings: policy_warning.into_iter().collect(),
                diagnostics,
            };
        }
        diagnostics.candidates.push(candidate_fact);
        let candidate = candidate_from_seed(
            &seed,
            selected_target,
            policy_warning.into_iter().collect(),
            diagnostics,
        );
        PlanItem::Update { id, candidate }
    } else {
        diagnostics.candidates.push(candidate_fact);
        let candidate = candidate_from_seed(
            &seed,
            selected_target,
            policy_warning.into_iter().collect(),
            diagnostics,
        );
        PlanItem::Delayed {
            id,
            candidate,
            reason: DelayReason::ReleaseTooFresh,
        }
    }
}

#[expect(clippy::too_many_arguments)]
#[expect(clippy::too_many_lines)]
fn evaluate_timeline_seed<V: Ord + Clone>(
    id: PlanItemId,
    seed: UpdateSeed,
    discovered_target: &VersionText,
    timeline: &ReleaseTimeline,
    policy: VersionPolicy,
    now: SystemTime,
    min_release_age: Duration,
    audit_results: &BTreeMap<AuditQuery, AuditLookupResult>,
    parse: fn(&str) -> Result<V, String>,
    classify: fn(&str) -> ReleaseClass,
    unparseable_entry_is_fatal: bool,
) -> PlanItem {
    let Ok(installed_version) = parse(seed.installed.installed_version.as_str()) else {
        return PlanItem::ResolverError {
            id,
            installed: seed.installed,
            message: "failed to parse installed version".to_owned(),
        };
    };
    let Ok(parsed_discovered_target) = parse(discovered_target.as_str()) else {
        return PlanItem::ResolverError {
            id,
            installed: seed.installed,
            message: "failed to parse discovered target version".to_owned(),
        };
    };
    let mut target_metadata_found = false;
    for entry in &timeline.versions {
        let Ok(parsed) = parse(entry.version.as_str()) else {
            // SemVer timelines tolerate unparseable entries (83305cb); pep440 surfaces them.
            if unparseable_entry_is_fatal {
                return PlanItem::ResolverError {
                    id,
                    installed: seed.installed,
                    message: format!("failed to parse release version `{}`", entry.version),
                };
            }
            continue;
        };
        if parsed == parsed_discovered_target {
            target_metadata_found = true;
        }
    }
    if !target_metadata_found {
        return PlanItem::Blocked {
            id,
            seed,
            reason: BlockReason::MissingReleaseMetadata,
            policy_warnings: Vec::new(),
            diagnostics: PlanDiagnostics::new(min_release_age)
                .with_missing_metadata(MissingMetadataKind::DiscoveredTarget),
        };
    }

    let mut candidate_facts = Vec::<(V, CandidateFact)>::new();
    let installed_class = classify(seed.installed.installed_version.as_str());

    for entry in &timeline.versions {
        // Unparseable entries: fatal schemes already errored in the metadata loop above;
        // the rest skip them (83305cb).
        let Ok(parsed) = parse(entry.version.as_str()) else {
            continue;
        };
        if parsed <= installed_version {
            continue;
        }

        let candidate_class = classify(entry.version.as_str());
        let policy_decision = evaluate_policy(policy, installed_class, candidate_class);
        let mut fact = CandidateFact::new(entry, now, min_release_age, &policy_decision);
        fact.audit = audit_fact_for(
            seed.installed.audit_subject.as_ref(),
            &entry.version,
            audit_results,
        );
        candidate_facts.push((parsed, fact));
    }

    let newest_overall = candidate_facts
        .iter()
        .reduce(|current, candidate| {
            if candidate_is_newer_by_date(&candidate.0, &candidate.1, current) {
                candidate
            } else {
                current
            }
        })
        .cloned();
    let newest_policy_eligible = candidate_facts
        .iter()
        .filter(|(_, fact)| fact.policy_block_reason.is_none())
        .reduce(|current, candidate| {
            if candidate_is_newer_by_date(&candidate.0, &candidate.1, current) {
                candidate
            } else {
                current
            }
        })
        .cloned();
    let newest_age_eligible = candidate_facts
        .iter()
        .filter(|(_, fact)| fact.policy_block_reason.is_none() && fact.age_allowed)
        .reduce(|current, candidate| {
            if candidate_is_newer_by_date(&candidate.0, &candidate.1, current) {
                candidate
            } else {
                current
            }
        })
        .cloned();
    let newest_audit_eligible = candidate_facts
        .iter()
        .filter(|(_, fact)| {
            fact.policy_block_reason.is_none()
                && fact.age_allowed
                && audit_allows_optional_target(fact.audit.as_ref())
        })
        .reduce(|current, candidate| {
            if candidate_is_newer_by_date(&candidate.0, &candidate.1, current) {
                candidate
            } else {
                current
            }
        })
        .cloned();

    let mut diagnostics = diagnostics_from_candidate_facts(
        min_release_age,
        candidate_facts,
        newest_overall.as_ref().map(|(_, fact)| fact),
    );

    let Some((_, newest_overall)) = newest_overall else {
        return PlanItem::Current {
            id,
            installed: seed.installed,
        };
    };

    let Some((_, policy_candidate)) = newest_policy_eligible else {
        let target_class = classify(newest_overall.version.as_str());
        let PolicyDecision::Blocked { reason, .. } =
            evaluate_policy(policy, installed_class, target_class)
        else {
            unreachable!("missing policy candidate implies blocked newest candidate");
        };
        return PlanItem::Blocked {
            id,
            seed,
            reason: BlockReason::VersionPolicy(reason),
            policy_warnings: newest_overall.warnings,
            diagnostics,
        };
    };

    diagnostics.selected_target = Some(policy_candidate.candidate_age());

    let Some((_, age_candidate)) = newest_age_eligible else {
        let candidate = candidate_from_seed(
            &seed,
            policy_candidate.version,
            policy_candidate.warnings,
            diagnostics,
        );
        return PlanItem::Delayed {
            id,
            candidate,
            reason: DelayReason::ReleaseTooFresh,
        };
    };

    let Some((_, audit_candidate)) = newest_audit_eligible else {
        let audit =
            age_candidate
                .audit
                .clone()
                .unwrap_or_else(|| AuditLookupResult::LookupFailed {
                    detail: "audit lookup result missing".to_owned(),
                });
        diagnostics.audit_blocking_target = Some(audit.clone());
        diagnostics.audit_blocking_candidate = Some(age_candidate.candidate_evaluation());
        return PlanItem::Blocked {
            id,
            seed,
            reason: audit_block_reason(&audit),
            policy_warnings: Vec::new(),
            diagnostics,
        };
    };

    diagnostics.selected_target = Some(audit_candidate.candidate_age());
    PlanItem::Update {
        id,
        candidate: candidate_from_seed(
            &seed,
            audit_candidate.version,
            audit_candidate.warnings,
            diagnostics,
        ),
    }
}

fn candidate_from_seed(
    seed: &UpdateSeed,
    target_version: VersionText,
    policy_warnings: Vec<PolicyWarning>,
    diagnostics: PlanDiagnostics,
) -> UpdateCandidate {
    UpdateCandidate::new(
        seed.installed.tool_id.clone(),
        seed.installed.package_name.clone(),
        seed.installed.installed_version.clone(),
        target_version,
        seed.version_scheme,
        seed.execution_support,
    )
    .with_execution_target_kind(seed.execution_target_kind)
    .with_policy_warnings(policy_warnings)
    .with_diagnostics(diagnostics)
}

fn audit_fact_for(
    subject: Option<&AuditSubject>,
    version: &VersionText,
    audit_results: &BTreeMap<AuditQuery, AuditLookupResult>,
) -> Option<AuditLookupResult> {
    let subject = subject?;
    let query = AuditQuery::new(subject.clone(), version.clone());
    Some(audit_results.get(&query).map_or_else(
        || AuditLookupResult::LookupFailed {
            detail: "audit lookup result missing".to_owned(),
        },
        Clone::clone,
    ))
}

const fn audit_allows_optional_target(audit: Option<&AuditLookupResult>) -> bool {
    match audit {
        None | Some(AuditLookupResult::Clean) => true,
        Some(AuditLookupResult::Vulnerable { .. } | AuditLookupResult::LookupFailed { .. }) => {
            false
        }
    }
}

const fn audit_allows_target(audit: &AuditLookupResult) -> bool {
    matches!(audit, AuditLookupResult::Clean)
}

const fn audit_block_reason(audit: &AuditLookupResult) -> BlockReason {
    match audit {
        AuditLookupResult::Vulnerable { .. } => BlockReason::AuditVulnerable,
        AuditLookupResult::Clean | AuditLookupResult::LookupFailed { .. } => {
            BlockReason::AuditLookupFailed
        }
    }
}

const fn evaluate_policy(
    policy: VersionPolicy,
    installed_class: ReleaseClass,
    candidate_class: ReleaseClass,
) -> PolicyDecision {
    match policy {
        VersionPolicy::None => PolicyDecision::Allowed { warning: None },
        VersionPolicy::Stable => evaluate_stable_policy(candidate_class, None),
        VersionPolicy::SameTrack => evaluate_same_track_policy(installed_class, candidate_class),
    }
}

fn classify_release(version_scheme: VersionScheme, raw: &str) -> ReleaseClass {
    match version_scheme {
        VersionScheme::SemVer => classify_semver_release(raw),
        VersionScheme::Pep440 => classify_pep440_release(raw),
        VersionScheme::ManagerNative => classify_manager_native_release(raw),
    }
}

fn selected_target_is_update(seed: &UpdateSeed, target: &VersionText) -> Result<bool, String> {
    if target == &seed.installed.installed_version {
        return Ok(false);
    }

    match seed.version_scheme {
        VersionScheme::SemVer => {
            let installed = parse_semver(seed.installed.installed_version.as_str())
                .map_err(|_| "failed to parse installed version".to_owned())?;
            let target = parse_semver(target.as_str())
                .map_err(|_| "failed to parse selected target version".to_owned())?;
            Ok(target > installed)
        }
        VersionScheme::Pep440 => {
            let installed = parse_pep440(seed.installed.installed_version.as_str())
                .map_err(|_| "failed to parse installed version".to_owned())?;
            let target = parse_pep440(target.as_str())
                .map_err(|_| "failed to parse selected target version".to_owned())?;
            Ok(target > installed)
        }
        VersionScheme::ManagerNative => Ok(target != &seed.installed.installed_version),
    }
}

const fn evaluate_same_track_policy(
    installed_class: ReleaseClass,
    candidate_class: ReleaseClass,
) -> PolicyDecision {
    let Some(installed_rank) = installed_class.stability_rank() else {
        return evaluate_stable_policy(
            candidate_class,
            Some(PolicyWarning::InstalledTrackUnknownFallbackStable),
        );
    };

    if candidate_class.is_final() {
        return PolicyDecision::Allowed { warning: None };
    }

    let Some(candidate_rank) = candidate_class.stability_rank() else {
        return PolicyDecision::Blocked {
            reason: PolicyBlockReason::UnknownStability,
            warning: None,
        };
    };

    if candidate_rank >= installed_rank {
        PolicyDecision::Allowed { warning: None }
    } else {
        PolicyDecision::Blocked {
            reason: PolicyBlockReason::TrackRegression,
            warning: None,
        }
    }
}

const fn evaluate_stable_policy(
    candidate_class: ReleaseClass,
    warning: Option<PolicyWarning>,
) -> PolicyDecision {
    if candidate_class.is_final() {
        PolicyDecision::Allowed { warning }
    } else {
        PolicyDecision::Blocked {
            reason: PolicyBlockReason::PreReleaseBlocked,
            warning,
        }
    }
}

fn is_evidence_old_enough(
    evidence: &TargetAgeEvidence,
    now: SystemTime,
    min_release_age: Duration,
) -> bool {
    evidence_age(evidence, now) >= min_release_age
}

fn target_age_is_old_enough(
    target_age: &TargetAgeLookupResult,
    now: SystemTime,
    min_release_age: Duration,
) -> bool {
    match target_age {
        TargetAgeLookupResult::Known(evidence) => {
            is_evidence_old_enough(evidence, now, min_release_age)
        }
        TargetAgeLookupResult::MissingMetadata | TargetAgeLookupResult::LookupFailed(_) => false,
    }
}

fn target_age_duration(target_age: &TargetAgeLookupResult, now: SystemTime) -> Option<Duration> {
    match target_age {
        TargetAgeLookupResult::Known(evidence) => Some(evidence_age(evidence, now)),
        TargetAgeLookupResult::MissingMetadata | TargetAgeLookupResult::LookupFailed(_) => None,
    }
}

fn evidence_age(evidence: &TargetAgeEvidence, now: SystemTime) -> Duration {
    now.duration_since(*evidence.timestamp().as_system_time())
        .unwrap_or_default()
}

fn release_age(entry: &ReleaseEntry, now: SystemTime) -> Duration {
    now.duration_since(*entry.published_at.as_system_time())
        .unwrap_or_default()
}

fn advisory_latest_diagnostics(
    advisory: Option<&AdvisoryReleaseLookup>,
    now: SystemTime,
) -> Option<AdvisoryLatestFact> {
    let advisory = advisory?;
    Some(match &advisory.release_lookup {
        ReleaseLookupResult::Known(timeline) => AdvisoryLatestFact::Known {
            latest_version: advisory.latest_version.clone(),
            candidates: timeline
                .versions
                .iter()
                .map(|entry| CandidateAgeFact::new(entry.version.clone(), release_age(entry, now)))
                .collect(),
        },
        ReleaseLookupResult::MissingMetadata => AdvisoryLatestFact::MissingMetadata {
            latest_version: advisory.latest_version.clone(),
        },
        ReleaseLookupResult::LookupFailed(error) => AdvisoryLatestFact::LookupFailed {
            latest_version: advisory.latest_version.clone(),
            error: error.clone(),
        },
    })
}

fn diagnostics_from_candidate_facts<T>(
    required_age: Duration,
    mut candidates: Vec<(T, CandidateFact)>,
    latest_overall: Option<&CandidateFact>,
) -> PlanDiagnostics
where
    T: Ord,
{
    candidates.sort_by(|(left, _), (right, _)| right.cmp(left));
    PlanDiagnostics {
        required_age,
        candidates: candidates
            .iter()
            .map(|(_, fact)| fact.candidate_evaluation())
            .collect(),
        selected_target: None,
        latest_overall: latest_overall.map(CandidateFact::candidate_age),
        missing_metadata: None,
        lookup_failure: None,
        advisory_latest: None,
        advisory_lookup_failure: None,
        audit_blocking_target: None,
        audit_blocking_candidate: None,
    }
}

fn candidate_is_newer_by_date<T>(
    parsed: &T,
    fact: &CandidateFact,
    current: &(T, CandidateFact),
) -> bool
where
    T: Ord,
{
    fact.published_at
        .as_system_time()
        .cmp(current.1.published_at.as_system_time())
        .then_with(|| parsed.cmp(&current.0))
        .is_gt()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CandidateFact {
    version: VersionText,
    age: Duration,
    published_at: upgate_domain::ReleaseTimestamp,
    age_allowed: bool,
    policy_block_reason: Option<PolicyBlockReason>,
    warnings: Vec<PolicyWarning>,
    audit: Option<AuditLookupResult>,
}

impl CandidateFact {
    fn new(
        entry: &ReleaseEntry,
        now: SystemTime,
        min_release_age: Duration,
        policy_decision: &PolicyDecision,
    ) -> Self {
        let age = release_age(entry, now);
        Self {
            version: entry.version.clone(),
            age,
            published_at: entry.published_at.clone(),
            age_allowed: age >= min_release_age,
            policy_block_reason: policy_decision.block_reason(),
            warnings: policy_decision.warning().into_iter().collect(),
            audit: None,
        }
    }

    fn candidate_age(&self) -> CandidateAgeFact {
        CandidateAgeFact::new(self.version.clone(), self.age)
    }

    fn candidate_evaluation(&self) -> CandidateEvaluationFact {
        CandidateEvaluationFact {
            version: self.version.clone(),
            age: Some(self.age),
            age_allowed: self.age_allowed,
            policy_block_reason: self.policy_block_reason.clone(),
            policy_warning: self.warnings.first().copied(),
            audit: self.audit.clone(),
        }
    }
}
