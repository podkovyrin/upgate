use crate::manager::Manager;
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};

pub(crate) struct PlanMeta {
    pub(crate) manager: Manager,
    pub(crate) source: &'static str,
    pub(crate) name: String,
    pub(crate) current: String,
}

pub(crate) struct DelayedLatest {
    pub(crate) latest_version: String,
    pub(crate) latest_age: String,
    pub(crate) required_age: String,
}

pub(crate) enum PlanDecision {
    Error(String),
    DelayedNoEligible {
        required_age: String,
    },
    NoChange,
    Update {
        target: String,
        delayed_latest: Option<DelayedLatest>,
    },
}

pub(crate) fn emit_plan_and_collect_upgradable<T, M, D>(
    items: Vec<T>,
    mut meta_fn: M,
    mut decision_fn: D,
) -> Vec<(String, String, String)>
where
    M: FnMut(&T) -> PlanMeta,
    D: FnMut(&T) -> PlanDecision,
{
    let mut upgradable = Vec::new();

    for item in items {
        let PlanMeta {
            manager,
            source,
            name,
            current,
        } = meta_fn(&item);

        match decision_fn(&item) {
            PlanDecision::Error(err) => {
                let outcome = ItemOutcome::error(
                    manager,
                    name,
                    current.clone(),
                    current,
                    source,
                    REASON_COMMAND_FAILED,
                    err,
                );
                emit_text_outcome(&outcome);
            }
            PlanDecision::DelayedNoEligible { required_age } => {
                let outcome =
                    ItemOutcome::delayed_no_eligible(manager, name, current, source, required_age);
                emit_text_outcome(&outcome);
            }
            PlanDecision::NoChange => {
                let outcome = ItemOutcome::skipped_no_change(manager, name, current, source);
                emit_text_outcome(&outcome);
            }
            PlanDecision::Update {
                target,
                delayed_latest,
            } => {
                let outcome = if let Some(DelayedLatest {
                    latest_version,
                    latest_age,
                    required_age,
                }) = delayed_latest
                {
                    ItemOutcome::update_with_delayed_latest(
                        manager,
                        name.clone(),
                        current.clone(),
                        target.clone(),
                        source,
                        latest_version,
                        latest_age,
                        required_age,
                    )
                } else {
                    ItemOutcome::update(
                        manager,
                        name.clone(),
                        current.clone(),
                        target.clone(),
                        source,
                    )
                };

                emit_text_outcome(&outcome);
                upgradable.push((name, current, target));
            }
        }
    }

    upgradable
}
