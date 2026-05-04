use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use upnow_domain::{
    DomainError, ExecutionEligibility, ManagerId, PackageName, PinChange, PlanItem, PlanItemId,
    PlanSelection, SelectedItem, UpdatePlan, UpdateSeed, VersionPolicy,
};

pub const PIN_ALL: &str = "*";

use crate::evaluate_seed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanningSettings {
    pub policy: VersionPolicy,
    pub now: SystemTime,
    pub min_release_age: Duration,
    pub execution_eligibility: ExecutionEligibility,
}

/// Builds a typed manager update plan from manager-discovered seeds.
///
/// # Errors
///
/// Returns an error when generated plan item ids are invalid or duplicated.
pub fn update_plan_from_seeds(
    manager_id: ManagerId,
    seeds: Vec<UpdateSeed>,
    settings: PlanningSettings,
) -> Result<UpdatePlan, DomainError> {
    let mut items = Vec::new();
    for seed in seeds {
        let id = PlanItemId::new(format!(
            "{}:{}",
            manager_id.as_str(),
            seed.installed.package_name.as_str()
        ))?;
        items.push(evaluate_seed(
            id,
            seed,
            settings.policy,
            settings.now,
            settings.min_release_age,
            settings.execution_eligibility,
        ));
    }
    UpdatePlan::new(manager_id, items)
}

/// Selects default batch apply items: update candidates not currently pinned.
///
/// # Errors
///
/// Returns an error if the generated selection does not reference the plan.
pub fn default_batch_selection(
    plan: &UpdatePlan,
    pinned: &BTreeSet<PackageName>,
) -> Result<PlanSelection, DomainError> {
    let pin_all = pinned.iter().any(|pin| pin.as_str() == PIN_ALL);
    let selected_items = plan
        .items
        .iter()
        .filter_map(|item| match item {
            PlanItem::Update { id, candidate }
                if !pin_all && !pinned.contains(&candidate.package_name) =>
            {
                Some(SelectedItem::new(id.clone(), false))
            }
            _ => None,
        })
        .collect();

    PlanSelection::new(plan, selected_items, Vec::<PinChange>::new())
}
