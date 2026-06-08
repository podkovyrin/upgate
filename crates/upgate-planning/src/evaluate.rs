use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use pep440_rs::{PrereleaseKind as Pep440PrereleaseKind, Version as Pep440Version};
use semver::Version as SemverVersion;
use upgate_domain::{
    AdvisoryLatestFact, AdvisoryReleaseLookup, AuditLookupResult, AuditQuery, AuditSubject,
    BlockReason, CandidateAgeFact, CandidateAgeSource, CandidateAuditFact, CandidateEvaluationFact,
    DelayReason, ManagerSelectedTarget, MissingMetadataKind, PlanDiagnostics, PlanItem, PlanItemId,
    PlannedTarget, PolicyBlockReason, PolicyWarning, ReleaseEntry, ReleaseEvidenceSource,
    ReleaseLookupResult, ReleaseTimeline, TargetAgeEvidence, TargetAgeLookupResult,
    TargetSelection, UpdateCandidate, UpdateSeed, VersionPolicy, VersionReleaseEvidence,
    VersionScheme, VersionText,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseClass {
    Dev,
    Alpha,
    Beta,
    Rc,
    Final,
    UnknownPrerelease,
    Unknown,
}

impl ReleaseClass {
    const fn is_final(self) -> bool {
        matches!(self, Self::Final)
    }

    const fn stability_rank(self) -> Option<u8> {
        match self {
            Self::Dev => Some(0),
            Self::Alpha => Some(1),
            Self::Beta => Some(2),
            Self::Rc => Some(3),
            Self::Final => Some(4),
            Self::UnknownPrerelease | Self::Unknown => None,
        }
    }
}

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
    const fn allowed(&self) -> bool {
        matches!(self, Self::Allowed { .. })
    }

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

/// Evaluate one manager-discovered update seed into a typed plan item.
pub fn evaluate_seed(
    id: PlanItemId,
    seed: UpdateSeed,
    policy: VersionPolicy,
    now: SystemTime,
    min_release_age: Duration,
) -> PlanItem {
    evaluate_seed_with_audit(id, seed, policy, now, min_release_age, &BTreeMap::new())
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

pub fn audit_queries_for_seed(seed: &UpdateSeed) -> Vec<AuditQuery> {
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
            audit_queries_for_semver_timeline(seed, subject, discovered_target, timeline)
        }
        VersionScheme::Pep440 => {
            audit_queries_for_pep440_timeline(seed, subject, discovered_target, timeline)
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

fn audit_queries_for_semver_timeline(
    seed: &UpdateSeed,
    subject: &AuditSubject,
    discovered_target: &VersionText,
    timeline: &ReleaseTimeline,
) -> Vec<AuditQuery> {
    let Ok(installed_version) = parse_semver(seed.installed.installed_version.as_str()) else {
        return Vec::new();
    };
    let Ok(parsed_discovered_target) = parse_semver(discovered_target.as_str()) else {
        return Vec::new();
    };
    if !timeline.versions.iter().any(|entry| {
        parse_semver(entry.version.as_str())
            .map(|parsed| parsed == parsed_discovered_target)
            .unwrap_or(false)
    }) {
        return Vec::new();
    }

    timeline
        .versions
        .iter()
        .filter_map(|entry| {
            let parsed = parse_semver(entry.version.as_str()).ok()?;
            if parsed <= installed_version {
                return None;
            }
            Some(AuditQuery::new(subject.clone(), entry.version.clone()))
        })
        .collect()
}

fn audit_queries_for_pep440_timeline(
    seed: &UpdateSeed,
    subject: &AuditSubject,
    discovered_target: &VersionText,
    timeline: &ReleaseTimeline,
) -> Vec<AuditQuery> {
    let Ok(installed_version) = parse_pep440(seed.installed.installed_version.as_str()) else {
        return Vec::new();
    };
    let Ok(parsed_discovered_target) = parse_pep440(discovered_target.as_str()) else {
        return Vec::new();
    };
    if !timeline.versions.iter().any(|entry| {
        parse_pep440(entry.version.as_str())
            .map(|parsed| parsed == parsed_discovered_target)
            .unwrap_or(false)
    }) {
        return Vec::new();
    }

    timeline
        .versions
        .iter()
        .filter_map(|entry| {
            let parsed = parse_pep440(entry.version.as_str()).ok()?;
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
            VersionScheme::SemVer => evaluate_semver_seed(
                id,
                seed,
                discovered_target,
                &timeline,
                policy,
                now,
                min_release_age,
                audit_results,
            ),
            VersionScheme::Pep440 => evaluate_pep440_seed(
                id,
                seed,
                discovered_target,
                &timeline,
                policy,
                now,
                min_release_age,
                audit_results,
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
        policy_allowed: policy_decision.allowed(),
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
                candidate_age_source(&evidence),
                VersionReleaseEvidence::from_target_age(selected_target.clone(), &evidence),
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
fn evaluate_semver_seed(
    id: PlanItemId,
    seed: UpdateSeed,
    discovered_target: &VersionText,
    timeline: &ReleaseTimeline,
    policy: VersionPolicy,
    now: SystemTime,
    min_release_age: Duration,
    audit_results: &BTreeMap<AuditQuery, AuditLookupResult>,
) -> PlanItem {
    let Ok(installed_version) = parse_semver(seed.installed.installed_version.as_str()) else {
        return PlanItem::ResolverError {
            id,
            installed: seed.installed,
            message: "failed to parse installed version".to_owned(),
        };
    };
    let Ok(parsed_discovered_target) = parse_semver(discovered_target.as_str()) else {
        return PlanItem::ResolverError {
            id,
            installed: seed.installed,
            message: "failed to parse discovered target version".to_owned(),
        };
    };
    let target_metadata_found = timeline.versions.iter().any(|entry| {
        let Ok(parsed) = parse_semver(entry.version.as_str()) else {
            return false;
        };
        parsed == parsed_discovered_target
    });
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

    let mut newest_overall = None::<(SemverVersion, CandidateFact)>;
    let mut newest_policy_eligible = None::<(SemverVersion, CandidateFact)>;
    let mut newest_age_eligible = None::<(SemverVersion, CandidateFact)>;
    let mut newest_audit_eligible = None::<(SemverVersion, CandidateFact)>;
    let mut candidate_facts = Vec::<(SemverVersion, CandidateFact)>::new();
    let installed_class = classify_semver_release(seed.installed.installed_version.as_str());

    for entry in &timeline.versions {
        let Ok(parsed) = parse_semver(entry.version.as_str()) else {
            continue;
        };
        if parsed <= installed_version {
            continue;
        }

        let candidate_class = classify_semver_release(entry.version.as_str());
        let policy_decision = evaluate_policy(policy, installed_class, candidate_class);
        let policy_allowed = policy_decision.allowed();
        let mut fact = CandidateFact::new(entry, now, min_release_age, &policy_decision);
        fact.audit = audit_fact_for(
            seed.installed.audit_subject.as_ref(),
            &entry.version,
            audit_results,
        );
        candidate_facts.push((parsed.clone(), fact.clone()));
        if newest_overall
            .as_ref()
            .is_none_or(|current| candidate_is_newer_by_date(&parsed, &fact, current))
        {
            newest_overall = Some((parsed.clone(), fact.clone()));
        }

        if policy_allowed {
            if newest_policy_eligible
                .as_ref()
                .is_none_or(|current| candidate_is_newer_by_date(&parsed, &fact, current))
            {
                newest_policy_eligible = Some((parsed.clone(), fact.clone()));
            }
            if fact.age_allowed
                && newest_age_eligible
                    .as_ref()
                    .is_none_or(|current| candidate_is_newer_by_date(&parsed, &fact, current))
            {
                newest_age_eligible = Some((parsed.clone(), fact.clone()));
            }
            if fact.age_allowed
                && audit_allows_optional_target(fact.audit.as_ref())
                && newest_audit_eligible
                    .as_ref()
                    .is_none_or(|current| candidate_is_newer_by_date(&parsed, &fact, current))
            {
                newest_audit_eligible = Some((parsed, fact));
            }
        }
    }

    let diagnostics = diagnostics_from_candidate_facts(
        min_release_age,
        candidate_facts,
        newest_overall.as_ref().map(|(_, fact)| fact),
        newest_policy_eligible.as_ref().map(|(_, fact)| fact),
        newest_age_eligible.as_ref().map(|(_, fact)| fact),
    );

    let Some((_, newest_overall)) = newest_overall else {
        return PlanItem::Current {
            id,
            installed: seed.installed,
        };
    };

    let Some((_, policy_candidate)) = newest_policy_eligible else {
        let target_class = classify_semver_release(newest_overall.version.as_str());
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

    let mut diagnostics = diagnostics;
    diagnostics.selected_target = Some(policy_candidate.candidate_age());

    let Some(_) = newest_age_eligible.as_ref() else {
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
        let audit = newest_age_eligible
            .as_ref()
            .and_then(|(_, fact)| fact.audit.clone())
            .unwrap_or_else(|| CandidateAuditFact::LookupFailed {
                detail: "audit lookup result missing".to_owned(),
            });
        let mut diagnostics = diagnostics;
        diagnostics.audit_blocking_target = Some(audit.clone());
        diagnostics.audit_blocking_candidate = newest_age_eligible
            .as_ref()
            .map(|(_, fact)| fact.candidate_evaluation());
        return PlanItem::Blocked {
            id,
            seed,
            reason: audit_block_reason(&audit),
            policy_warnings: Vec::new(),
            diagnostics,
        };
    };

    let selected_target = audit_candidate.candidate_age();
    PlanItem::Update {
        id,
        candidate: candidate_from_seed(&seed, audit_candidate.version, audit_candidate.warnings, {
            let mut diagnostics = diagnostics;
            diagnostics.selected_target = Some(selected_target);
            diagnostics
        }),
    }
}

#[expect(clippy::too_many_arguments)]
#[expect(clippy::too_many_lines)]
fn evaluate_pep440_seed(
    id: PlanItemId,
    seed: UpdateSeed,
    discovered_target: &VersionText,
    timeline: &ReleaseTimeline,
    policy: VersionPolicy,
    now: SystemTime,
    min_release_age: Duration,
    audit_results: &BTreeMap<AuditQuery, AuditLookupResult>,
) -> PlanItem {
    let Ok(installed_version) = parse_pep440(seed.installed.installed_version.as_str()) else {
        return PlanItem::ResolverError {
            id,
            installed: seed.installed,
            message: "failed to parse installed version".to_owned(),
        };
    };
    let Ok(parsed_discovered_target) = parse_pep440(discovered_target.as_str()) else {
        return PlanItem::ResolverError {
            id,
            installed: seed.installed,
            message: "failed to parse discovered target version".to_owned(),
        };
    };
    let mut target_metadata_found = false;
    for entry in &timeline.versions {
        let Ok(parsed) = parse_pep440(entry.version.as_str()) else {
            let bad_version = entry.version.as_str().to_owned();
            return PlanItem::ResolverError {
                id,
                installed: seed.installed,
                message: format!("failed to parse release version `{bad_version}`"),
            };
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

    let mut newest_overall = None::<(Pep440Version, CandidateFact)>;
    let mut newest_policy_eligible = None::<(Pep440Version, CandidateFact)>;
    let mut newest_age_eligible = None::<(Pep440Version, CandidateFact)>;
    let mut newest_audit_eligible = None::<(Pep440Version, CandidateFact)>;
    let mut candidate_facts = Vec::<(Pep440Version, CandidateFact)>::new();
    let installed_class = classify_pep440_release(seed.installed.installed_version.as_str());

    for entry in &timeline.versions {
        let Ok(parsed) = parse_pep440(entry.version.as_str()) else {
            let bad_version = entry.version.as_str().to_owned();
            return PlanItem::ResolverError {
                id,
                installed: seed.installed,
                message: format!("failed to parse release version `{bad_version}`"),
            };
        };
        if parsed <= installed_version {
            continue;
        }

        let candidate_class = classify_pep440_release(entry.version.as_str());
        let policy_decision = evaluate_policy(policy, installed_class, candidate_class);
        let policy_allowed = policy_decision.allowed();
        let mut fact = CandidateFact::new(entry, now, min_release_age, &policy_decision);
        fact.audit = audit_fact_for(
            seed.installed.audit_subject.as_ref(),
            &entry.version,
            audit_results,
        );
        candidate_facts.push((parsed.clone(), fact.clone()));
        if newest_overall
            .as_ref()
            .is_none_or(|current| candidate_is_newer_by_date(&parsed, &fact, current))
        {
            newest_overall = Some((parsed.clone(), fact.clone()));
        }

        if policy_allowed {
            if newest_policy_eligible
                .as_ref()
                .is_none_or(|current| candidate_is_newer_by_date(&parsed, &fact, current))
            {
                newest_policy_eligible = Some((parsed.clone(), fact.clone()));
            }
            if fact.age_allowed
                && newest_age_eligible
                    .as_ref()
                    .is_none_or(|current| candidate_is_newer_by_date(&parsed, &fact, current))
            {
                newest_age_eligible = Some((parsed.clone(), fact.clone()));
            }
            if fact.age_allowed
                && audit_allows_optional_target(fact.audit.as_ref())
                && newest_audit_eligible
                    .as_ref()
                    .is_none_or(|current| candidate_is_newer_by_date(&parsed, &fact, current))
            {
                newest_audit_eligible = Some((parsed, fact));
            }
        }
    }

    let diagnostics = diagnostics_from_candidate_facts(
        min_release_age,
        candidate_facts,
        newest_overall.as_ref().map(|(_, fact)| fact),
        newest_policy_eligible.as_ref().map(|(_, fact)| fact),
        newest_age_eligible.as_ref().map(|(_, fact)| fact),
    );

    let Some((_, newest_overall)) = newest_overall else {
        return PlanItem::Current {
            id,
            installed: seed.installed,
        };
    };

    let Some((_, policy_candidate)) = newest_policy_eligible else {
        let target_class = classify_pep440_release(newest_overall.version.as_str());
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

    let mut diagnostics = diagnostics;
    diagnostics.selected_target = Some(policy_candidate.candidate_age());

    let Some(_) = newest_age_eligible.as_ref() else {
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
        let audit = newest_age_eligible
            .as_ref()
            .and_then(|(_, fact)| fact.audit.clone())
            .unwrap_or_else(|| CandidateAuditFact::LookupFailed {
                detail: "audit lookup result missing".to_owned(),
            });
        let mut diagnostics = diagnostics;
        diagnostics.audit_blocking_target = Some(audit.clone());
        diagnostics.audit_blocking_candidate = newest_age_eligible
            .as_ref()
            .map(|(_, fact)| fact.candidate_evaluation());
        return PlanItem::Blocked {
            id,
            seed,
            reason: audit_block_reason(&audit),
            policy_warnings: Vec::new(),
            diagnostics,
        };
    };

    let selected_target = audit_candidate.candidate_age();
    PlanItem::Update {
        id,
        candidate: candidate_from_seed(&seed, audit_candidate.version, audit_candidate.warnings, {
            let mut diagnostics = diagnostics;
            diagnostics.selected_target = Some(selected_target);
            diagnostics
        }),
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
) -> Option<CandidateAuditFact> {
    let subject = subject?;
    let query = AuditQuery::new(subject.clone(), version.clone());
    Some(audit_results.get(&query).map_or_else(
        || CandidateAuditFact::LookupFailed {
            detail: "audit lookup result missing".to_owned(),
        },
        CandidateAuditFact::from,
    ))
}

const fn audit_allows_optional_target(audit: Option<&CandidateAuditFact>) -> bool {
    match audit {
        None | Some(CandidateAuditFact::Clean) => true,
        Some(CandidateAuditFact::Vulnerable { .. } | CandidateAuditFact::LookupFailed { .. }) => {
            false
        }
    }
}

const fn audit_allows_target(audit: &CandidateAuditFact) -> bool {
    matches!(audit, CandidateAuditFact::Clean)
}

const fn audit_block_reason(audit: &CandidateAuditFact) -> BlockReason {
    match audit {
        CandidateAuditFact::Vulnerable { .. } => BlockReason::AuditVulnerable,
        CandidateAuditFact::Clean | CandidateAuditFact::LookupFailed { .. } => {
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

const fn candidate_age_source(evidence: &TargetAgeEvidence) -> CandidateAgeSource {
    match evidence {
        TargetAgeEvidence::PublishedAt(_) => CandidateAgeSource::PublishedAt,
        TargetAgeEvidence::ManagerNativeTimestamp(_) => CandidateAgeSource::ManagerNativeTimestamp,
    }
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
                .map(|entry| {
                    CandidateAgeFact::new(
                        entry.version.clone(),
                        release_age(entry, now),
                        CandidateAgeSource::ReleaseTimeline,
                        VersionReleaseEvidence::new(
                            entry.version.clone(),
                            entry.published_at.clone(),
                            ReleaseEvidenceSource::ReleaseTimeline,
                        ),
                    )
                })
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
    latest_policy_eligible: Option<&CandidateFact>,
    latest_age_eligible: Option<&CandidateFact>,
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
        latest_policy_eligible: latest_policy_eligible.map(CandidateFact::candidate_age),
        latest_age_eligible: latest_age_eligible.map(CandidateFact::candidate_age),
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
    policy_allowed: bool,
    policy_block_reason: Option<PolicyBlockReason>,
    warnings: Vec<PolicyWarning>,
    audit: Option<CandidateAuditFact>,
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
            policy_allowed: policy_decision.allowed(),
            policy_block_reason: policy_decision.block_reason(),
            warnings: policy_decision.warning().into_iter().collect(),
            audit: None,
        }
    }

    fn candidate_age(&self) -> CandidateAgeFact {
        CandidateAgeFact::new(
            self.version.clone(),
            self.age,
            CandidateAgeSource::ReleaseTimeline,
            VersionReleaseEvidence::new(
                self.version.clone(),
                self.published_at.clone(),
                ReleaseEvidenceSource::ReleaseTimeline,
            ),
        )
    }

    fn candidate_evaluation(&self) -> CandidateEvaluationFact {
        CandidateEvaluationFact {
            version: self.version.clone(),
            age: Some(self.age),
            policy_allowed: self.policy_allowed,
            age_allowed: self.age_allowed,
            policy_block_reason: self.policy_block_reason.clone(),
            policy_warning: self.warnings.first().copied(),
            audit: self.audit.clone(),
        }
    }
}

fn parse_semver(raw: &str) -> Result<SemverVersion, String> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    SemverVersion::parse(trimmed)
        .or_else(|_| {
            let parts = trimmed.split('.').collect::<Vec<_>>();
            if parts.is_empty()
                || parts.len() > 3
                || parts.iter().any(|part| part.is_empty())
                || !parts
                    .iter()
                    .all(|part| part.chars().all(|ch| ch.is_ascii_digit()))
            {
                return SemverVersion::parse(trimmed);
            }
            let mut padded = parts;
            while padded.len() < 3 {
                padded.push("0");
            }
            SemverVersion::parse(&padded.join("."))
        })
        .map_err(|err| err.to_string())
}

fn parse_pep440(raw: &str) -> Result<Pep440Version, String> {
    Pep440Version::from_str(raw).map_err(|err| err.to_string())
}

fn classify_semver_release(raw: &str) -> ReleaseClass {
    let raw = raw.trim();
    let Ok(parsed) = parse_semver(raw) else {
        return classify_semver_like_fallback(raw);
    };
    if parsed.pre.is_empty() {
        return ReleaseClass::Final;
    }
    classify_prerelease_text(parsed.pre.as_str()).unwrap_or(ReleaseClass::UnknownPrerelease)
}

fn classify_pep440_release(raw: &str) -> ReleaseClass {
    let Ok(parsed) = Pep440Version::from_str(raw) else {
        return ReleaseClass::Unknown;
    };
    if parsed.is_dev() {
        return ReleaseClass::Dev;
    }
    if let Some(pre) = parsed.pre() {
        return match pre.kind {
            Pep440PrereleaseKind::Alpha => ReleaseClass::Alpha,
            Pep440PrereleaseKind::Beta => ReleaseClass::Beta,
            Pep440PrereleaseKind::Rc => ReleaseClass::Rc,
        };
    }
    ReleaseClass::Final
}

fn classify_semver_like_fallback(raw: &str) -> ReleaseClass {
    let raw = raw.trim();
    let raw = raw.strip_prefix(['v', 'V']).unwrap_or(raw);
    if raw.is_empty() {
        return ReleaseClass::Unknown;
    }
    if let Some((core, prerelease)) = raw.split_once('-')
        && is_numeric_dot_core(core)
    {
        return classify_prerelease_text(prerelease).unwrap_or(ReleaseClass::UnknownPrerelease);
    }
    if is_numeric_dot_core(raw) {
        return ReleaseClass::Final;
    }
    ReleaseClass::Unknown
}

fn classify_manager_native_release(raw: &str) -> ReleaseClass {
    let normalized = normalize_brew_version(raw);
    let version = normalized.trim();
    if version.is_empty()
        || version.eq_ignore_ascii_case("latest")
        || !version.chars().any(|ch| ch.is_ascii_alphanumeric())
    {
        return ReleaseClass::Unknown;
    }
    classify_brew_prerelease(version).unwrap_or(ReleaseClass::Final)
}

fn normalize_brew_version(raw: &str) -> &str {
    let trimmed = raw.trim();
    let without_cask_build = trimmed
        .split_once(',')
        .map_or(trimmed, |(head, _)| head.trim());
    strip_brew_revision_suffix(without_cask_build)
}

fn strip_brew_revision_suffix(raw: &str) -> &str {
    let Some((head, revision)) = raw.rsplit_once('_') else {
        return raw;
    };
    if !head.is_empty() && revision.chars().all(|ch| ch.is_ascii_digit()) {
        head
    } else {
        raw
    }
}

fn classify_brew_prerelease(version: &str) -> Option<ReleaseClass> {
    let mut best_match = None;
    let mut token_start = None;
    for (idx, ch) in version.char_indices() {
        if ch.is_ascii_alphanumeric() {
            token_start.get_or_insert(idx);
        } else if let Some(start) = token_start.take() {
            best_match = select_less_stable(best_match, classify_brew_token(version, start, idx));
        }
    }
    if let Some(start) = token_start {
        best_match = select_less_stable(
            best_match,
            classify_brew_token(version, start, version.len()),
        );
    }
    best_match
}

fn classify_brew_token(version: &str, start: usize, end: usize) -> Option<ReleaseClass> {
    let token = &version[start..end];
    let normalized = token.to_ascii_lowercase();
    let marker = normalized
        .trim_start_matches(|ch: char| ch.is_ascii_digit())
        .trim();
    let label = leading_alpha_prefix(marker);
    if label.is_empty() {
        return None;
    }
    if matches!(
        label,
        "canary"
            | "nightly"
            | "snapshot"
            | "dev"
            | "devel"
            | "development"
            | "next"
            | "edge"
            | "preview"
            | "experimental"
            | "exp"
    ) {
        return Some(ReleaseClass::Dev);
    }
    if label == "alpha" {
        return Some(ReleaseClass::Alpha);
    }
    if label == "beta" {
        return Some(ReleaseClass::Beta);
    }
    if matches!(label, "prerelease" | "pre" | "rc") {
        return Some(ReleaseClass::Rc);
    }
    if label == "a" && has_short_brew_prerelease_context(version, start, token, marker) {
        return Some(ReleaseClass::Alpha);
    }
    if label == "b" && has_short_brew_prerelease_context(version, start, token, marker) {
        return Some(ReleaseClass::Beta);
    }
    None
}

fn has_short_brew_prerelease_context(
    version: &str,
    token_start: usize,
    token: &str,
    marker: &str,
) -> bool {
    if marker.len() < token.len() {
        return true;
    }
    let prefix = version[..token_start]
        .trim_start_matches(['v', 'V'])
        .trim_matches(|ch: char| matches!(ch, '.' | '-' | '_' | '+'));
    prefix.is_empty()
        || (prefix.chars().any(|ch| ch.is_ascii_digit())
            && prefix
                .chars()
                .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '_' | '+')))
}

fn classify_prerelease_text(raw: &str) -> Option<ReleaseClass> {
    let mut best = None;
    for token in raw.split(['.', '-', '_']) {
        let normalized = token.to_ascii_lowercase();
        let label = leading_alpha_prefix(&normalized);
        let next = match label {
            "canary" | "nightly" | "snapshot" | "dev" | "devel" | "development" | "next"
            | "edge" | "preview" | "experimental" | "exp" => Some(ReleaseClass::Dev),
            "alpha" | "a" => Some(ReleaseClass::Alpha),
            "beta" | "b" => Some(ReleaseClass::Beta),
            "prerelease" | "pre" | "rc" => Some(ReleaseClass::Rc),
            _ => None,
        };
        best = select_less_stable(best, next);
    }
    best
}

const fn select_less_stable(
    current: Option<ReleaseClass>,
    next: Option<ReleaseClass>,
) -> Option<ReleaseClass> {
    let Some(next) = next else {
        return current;
    };
    let Some(current) = current else {
        return Some(next);
    };
    match (current.stability_rank(), next.stability_rank()) {
        (Some(current_rank), Some(next_rank)) if next_rank < current_rank => Some(next),
        _ => Some(current),
    }
}

fn leading_alpha_prefix(token: &str) -> &str {
    let end = token
        .char_indices()
        .find_map(|(idx, ch)| (!ch.is_ascii_alphabetic()).then_some(idx))
        .unwrap_or(token.len());
    &token[..end]
}

fn is_numeric_dot_core(raw: &str) -> bool {
    raw.split('.')
        .all(|segment| !segment.is_empty() && segment.chars().all(|ch| ch.is_ascii_digit()))
}
