use std::collections::BTreeSet;

use super::types::{
    ApplyCandidate, ApplyCandidateVersion, CandidateVersionMeta, DelayedLatest, PlanDecision,
    PlanMeta, PlannedUpdate, VersionPolicyMeta,
};
use crate::config::is_pinned;
use crate::managers::shared::versioning::policy::{GateBypass, PolicyBlockReason, VersionPolicy};
use crate::outcome::{DelayedReason, ItemOutcome, ReasonCode, emit_text_outcome};
use crate::ui::output_theme;

const MIN_RELEASE_AGE_BYPASS: GateBypass = GateBypass {
    version_policy: false,
    min_release_age: true,
};
const VERSION_POLICY_BYPASS: GateBypass = GateBypass {
    version_policy: true,
    min_release_age: false,
};

pub fn emit_plan_and_collect_apply_candidates<T, A>(
    items: Vec<T>,
    mut analyze_fn: A,
    suppress_update_outcomes: bool,
    pinned: Option<&BTreeSet<String>>,
    supports_exact_versions: bool,
) -> Vec<ApplyCandidate>
where
    A: FnMut(T) -> (PlanMeta, PlanDecision),
{
    let mut candidates = Vec::new();

    for item in items {
        let (
            PlanMeta {
                manager,
                name,
                current,
            },
            decision,
        ) = analyze_fn(item);

        if pinned.is_some_and(|set| is_pinned(&name, set)) {
            handle_pinned_decision(
                &mut candidates,
                manager,
                name,
                current,
                decision,
                suppress_update_outcomes,
                supports_exact_versions,
            );
            continue;
        }

        handle_regular_decision(
            &mut candidates,
            manager,
            name,
            current,
            decision,
            suppress_update_outcomes,
            supports_exact_versions,
        );
    }

    candidates
}

fn handle_pinned_decision(
    candidates: &mut Vec<ApplyCandidate>,
    manager: &'static str,
    name: String,
    current: String,
    decision: PlanDecision,
    suppress_update_outcomes: bool,
    supports_exact_versions: bool,
) {
    if suppress_update_outcomes {
        match decision {
            PlanDecision::Update {
                target,
                delayed_latest,
                version_policy,
                candidate_versions,
            } => {
                let planned = planned_update(
                    manager,
                    name,
                    current,
                    target,
                    delayed_latest,
                    version_policy,
                );
                candidates.push(recommended_candidate(
                    planned,
                    candidate_versions,
                    supports_exact_versions,
                ));
            }
            PlanDecision::DelayedNoEligible {
                required_age,
                delayed_latest,
                delayed_reason,
                version_policy,
                force_target: Some(target),
                candidate_versions,
            } => {
                let outcome = delayed_outcome(
                    manager,
                    name.clone(),
                    current.clone(),
                    required_age,
                    delayed_latest,
                    delayed_reason,
                    version_policy.clone(),
                );
                let note = outcome_note(&outcome);
                let planned = planned_update(manager, name, current, target, None, version_policy);
                candidates.push(force_candidate(
                    planned,
                    candidate_versions,
                    supports_exact_versions,
                    note,
                    MIN_RELEASE_AGE_BYPASS,
                ));
            }
            PlanDecision::CurrentBlockedByPolicy {
                version_policy,
                force_target: Some(target),
                candidate_versions,
            } => {
                let mut outcome = ItemOutcome::current(manager, name.clone(), current.clone());
                version_policy.apply_to_outcome(&mut outcome);
                let note = outcome_note(&outcome);
                let planned =
                    planned_update(manager, name, current, target, None, Some(version_policy));
                candidates.push(force_candidate(
                    planned,
                    candidate_versions,
                    supports_exact_versions,
                    note,
                    VERSION_POLICY_BYPASS,
                ));
            }
            PlanDecision::DelayedNoEligible { .. }
            | PlanDecision::CurrentBlockedByPolicy { .. }
            | PlanDecision::NoChange
            | PlanDecision::Error(_) => {}
        }
        return;
    }

    let target = match decision {
        PlanDecision::Update { target, .. } => target,
        _ => current.clone(),
    };
    let outcome =
        ItemOutcome::skipped(manager, name, current, target, ReasonCode::Pinned, "pinned");
    emit_text_outcome(&outcome);
}

fn handle_regular_decision(
    candidates: &mut Vec<ApplyCandidate>,
    manager: &'static str,
    name: String,
    current: String,
    decision: PlanDecision,
    suppress_update_outcomes: bool,
    supports_exact_versions: bool,
) {
    match decision {
        PlanDecision::Error(err) => {
            let outcome = ItemOutcome::resolver_error(manager, name, current.clone(), current, err);
            emit_text_outcome(&outcome);
        }
        PlanDecision::DelayedNoEligible {
            required_age,
            delayed_latest,
            delayed_reason,
            version_policy,
            force_target,
            candidate_versions,
        } => {
            let outcome = delayed_outcome(
                manager,
                name.clone(),
                current.clone(),
                required_age,
                delayed_latest,
                delayed_reason,
                version_policy.clone(),
            );
            let note = outcome_note(&outcome);
            emit_text_outcome(&outcome);
            if let Some(target) = force_target {
                let planned = planned_update(manager, name, current, target, None, version_policy);
                candidates.push(force_candidate(
                    planned,
                    candidate_versions,
                    supports_exact_versions,
                    note,
                    MIN_RELEASE_AGE_BYPASS,
                ));
            }
        }
        PlanDecision::CurrentBlockedByPolicy {
            version_policy,
            force_target,
            candidate_versions,
        } => {
            let mut outcome = ItemOutcome::current(manager, name.clone(), current.clone());
            version_policy.apply_to_outcome(&mut outcome);
            let note = outcome_note(&outcome);
            emit_text_outcome(&outcome);
            if let Some(target) = force_target {
                let planned =
                    planned_update(manager, name, current, target, None, Some(version_policy));
                candidates.push(force_candidate(
                    planned,
                    candidate_versions,
                    supports_exact_versions,
                    note,
                    VERSION_POLICY_BYPASS,
                ));
            }
        }
        PlanDecision::NoChange => {
            let outcome = ItemOutcome::current_no_newer(manager, name, current);
            emit_text_outcome(&outcome);
        }
        PlanDecision::Update {
            target,
            delayed_latest,
            version_policy,
            candidate_versions,
        } => {
            let planned = planned_update(
                manager,
                name,
                current,
                target,
                delayed_latest,
                version_policy,
            );
            if !suppress_update_outcomes {
                emit_text_outcome(&planned.to_update_outcome());
            }
            candidates.push(recommended_candidate(
                planned,
                candidate_versions,
                supports_exact_versions,
            ));
        }
    }
}

fn recommended_candidate(
    planned: PlannedUpdate,
    candidate_versions: Vec<CandidateVersionMeta>,
    supports_exact_versions: bool,
) -> ApplyCandidate {
    let note = outcome_note(&planned.to_update_outcome());
    ApplyCandidate::recommended(planned.clone())
        .with_note(note)
        .with_versions(apply_candidate_versions(
            &planned,
            candidate_versions,
            supports_exact_versions,
        ))
}

fn force_candidate(
    mut planned: PlannedUpdate,
    candidate_versions: Vec<CandidateVersionMeta>,
    supports_exact_versions: bool,
    note: String,
    fallback_bypass: GateBypass,
) -> ApplyCandidate {
    planned.gate_bypass =
        gate_bypass_for_target(&planned.target, &candidate_versions).unwrap_or(fallback_bypass);
    ApplyCandidate::force_candidate(planned.clone())
        .with_note(note)
        .with_versions(apply_candidate_versions(
            &planned,
            candidate_versions,
            supports_exact_versions,
        ))
}

fn apply_candidate_versions(
    planned: &PlannedUpdate,
    candidate_versions: Vec<CandidateVersionMeta>,
    supports_exact_versions: bool,
) -> Vec<ApplyCandidateVersion> {
    if !supports_exact_versions {
        return Vec::new();
    }

    candidate_versions
        .into_iter()
        .map(|candidate| {
            let mut update = planned.clone();
            update.target = candidate.version.clone();
            update.gate_bypass = gate_bypass_for_candidate(&candidate);
            let note = candidate_note(planned.version_policy.as_ref(), &candidate);
            let force = !candidate.policy_allowed || !candidate.age_allowed;
            ApplyCandidateVersion::new(update, note, force)
        })
        .collect()
}

fn gate_bypass_for_target(
    target: &str,
    candidate_versions: &[CandidateVersionMeta],
) -> Option<GateBypass> {
    candidate_versions
        .iter()
        .find(|candidate| candidate.version == target)
        .map(gate_bypass_for_candidate)
}

fn gate_bypass_for_candidate(candidate: &CandidateVersionMeta) -> GateBypass {
    GateBypass {
        version_policy: !candidate.policy_allowed,
        min_release_age: !candidate.age_allowed,
    }
}

fn candidate_note(policy: Option<&VersionPolicyMeta>, candidate: &CandidateVersionMeta) -> String {
    let mut parts = vec![format!("released: {}", candidate.age)];

    if !candidate.age_allowed {
        parts.push("too fresh".to_string());
    }

    if !candidate.policy_allowed {
        let policy_name = policy.map_or(VersionPolicy::Disabled.as_str(), |meta| {
            meta.policy.as_str()
        });
        let reason = candidate
            .policy_block_reason
            .map(policy_block_reason_note)
            .unwrap_or("blocked");
        parts.push(format!("version policy: {policy_name} {reason}"));
    }

    if let Some(warning) = candidate.policy_warning {
        parts.push(format!("version policy warning: {}", warning.as_note()));
    }

    parts.join("; ")
}

const fn policy_block_reason_note(reason: PolicyBlockReason) -> &'static str {
    match reason {
        PolicyBlockReason::NotFinal => "blocks non-final release",
        PolicyBlockReason::LessStableThanInstalled => "blocks less-stable release",
        PolicyBlockReason::UnknownStability => "blocks unknown stability",
    }
}

fn outcome_note(outcome: &ItemOutcome) -> String {
    crate::outcome::outcome_note(outcome, output_theme())
}

fn planned_update(
    manager: &'static str,
    name: String,
    current: String,
    target: String,
    delayed_latest: Option<DelayedLatest>,
    version_policy: Option<VersionPolicyMeta>,
) -> PlannedUpdate {
    PlannedUpdate {
        manager,
        name,
        current,
        target,
        delayed_latest,
        version_policy,
        apply_spec_base: None,
        gate_bypass: GateBypass::NONE,
    }
}

fn delayed_outcome(
    manager: &'static str,
    name: String,
    current: String,
    required_age: String,
    delayed_latest: Option<DelayedLatest>,
    delayed_reason: DelayedReason,
    version_policy: Option<VersionPolicyMeta>,
) -> ItemOutcome {
    let mut outcome = if let Some(DelayedLatest {
        latest_version,
        latest_age,
        required_age,
    }) = delayed_latest
    {
        ItemOutcome::delayed_no_eligible_with_latest(
            manager,
            name,
            current,
            latest_version,
            latest_age,
            required_age,
        )
    } else {
        ItemOutcome::delayed_no_eligible(manager, name, current, required_age)
    };

    outcome.set_delayed_reason(delayed_reason);

    if let Some(policy) = version_policy {
        policy.apply_to_outcome(&mut outcome);
    }

    outcome
}
