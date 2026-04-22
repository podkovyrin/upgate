use std::collections::BTreeSet;

use super::types::{DelayedLatest, PlanDecision, PlanMeta, PlannedUpdate, VersionPolicyMeta};
use crate::config::is_pinned;
use crate::outcome::{ItemOutcome, ReasonCode, emit_text_outcome};

pub fn emit_plan_and_collect_upgradable<T, A>(
    items: Vec<T>,
    mut analyze_fn: A,
    suppress_update_outcomes: bool,
    pinned: Option<&BTreeSet<String>>,
) -> Vec<PlannedUpdate>
where
    A: FnMut(T) -> (PlanMeta, PlanDecision),
{
    let mut upgradable = Vec::new();

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
                &mut upgradable,
                manager,
                name,
                current,
                decision,
                suppress_update_outcomes,
            );
            continue;
        }

        handle_regular_decision(
            &mut upgradable,
            manager,
            name,
            current,
            decision,
            suppress_update_outcomes,
        );
    }

    upgradable
}

fn handle_pinned_decision(
    upgradable: &mut Vec<PlannedUpdate>,
    manager: &'static str,
    name: String,
    current: String,
    decision: PlanDecision,
    suppress_update_outcomes: bool,
) {
    if suppress_update_outcomes {
        if let PlanDecision::Update {
            target,
            delayed_latest,
            version_policy,
        } = decision
        {
            upgradable.push(PlannedUpdate {
                manager,
                name,
                current,
                target,
                delayed_latest,
                version_policy,
                apply_spec_base: None,
            });
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
    upgradable: &mut Vec<PlannedUpdate>,
    manager: &'static str,
    name: String,
    current: String,
    decision: PlanDecision,
    suppress_update_outcomes: bool,
) {
    match decision {
        PlanDecision::Error(err) => {
            let outcome = ItemOutcome::error(
                manager,
                name,
                current.clone(),
                current,
                ReasonCode::CommandFailed,
                err,
            );
            emit_text_outcome(&outcome);
        }
        PlanDecision::DelayedNoEligible {
            required_age,
            delayed_latest,
            version_policy,
        } => {
            let outcome = delayed_outcome(
                manager,
                name,
                current,
                required_age,
                delayed_latest,
                version_policy,
            );
            emit_text_outcome(&outcome);
        }
        PlanDecision::CurrentBlockedByPolicy { version_policy } => {
            let mut outcome = ItemOutcome::current(manager, name, current);
            outcome.version_policy = Some(version_policy.policy);
            outcome.latest_blocked_by_policy_version = version_policy.latest_blocked_version;
            emit_text_outcome(&outcome);
        }
        PlanDecision::NoChange => {
            let outcome = ItemOutcome::skipped_no_change(manager, name, current);
            emit_text_outcome(&outcome);
        }
        PlanDecision::Update {
            target,
            delayed_latest,
            version_policy,
        } => {
            let planned = PlannedUpdate {
                manager,
                name,
                current,
                target,
                delayed_latest,
                version_policy,
                apply_spec_base: None,
            };
            if !suppress_update_outcomes {
                emit_text_outcome(&planned.to_update_outcome());
            }
            upgradable.push(planned);
        }
    }
}

fn delayed_outcome(
    manager: &'static str,
    name: String,
    current: String,
    required_age: String,
    delayed_latest: Option<DelayedLatest>,
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

    if let Some(policy) = version_policy {
        outcome.version_policy = Some(policy.policy);
        outcome.latest_blocked_by_policy_version = policy.latest_blocked_version;
    }

    outcome
}
