use std::collections::BTreeSet;

use super::types::{DelayedLatest, PlanDecision, PlanMeta, PlannedUpdate};
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
        } = decision
        {
            upgradable.push(PlannedUpdate {
                manager,
                name,
                current,
                target,
                delayed_latest,
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
        } => {
            let outcome = delayed_outcome(manager, name, current, required_age, delayed_latest);
            emit_text_outcome(&outcome);
        }
        PlanDecision::CurrentBlockedByPolicy => {
            let outcome = ItemOutcome::current(manager, name, current);
            emit_text_outcome(&outcome);
        }
        PlanDecision::NoChange => {
            let outcome = ItemOutcome::skipped_no_change(manager, name, current);
            emit_text_outcome(&outcome);
        }
        PlanDecision::Update {
            target,
            delayed_latest,
        } => {
            let planned = PlannedUpdate {
                manager,
                name,
                current,
                target,
                delayed_latest,
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
) -> ItemOutcome {
    if let Some(DelayedLatest {
        latest_version,
        latest_age,
        required_age,
    }) = delayed_latest
    {
        return ItemOutcome::delayed_no_eligible_with_latest(
            manager,
            name,
            current,
            latest_version,
            latest_age,
            required_age,
        );
    }

    ItemOutcome::delayed_no_eligible(manager, name, current, required_age)
}
