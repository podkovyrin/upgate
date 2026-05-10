use std::time::{Duration, SystemTime};

use upnow_domain::{
    DomainError, ManagerId, ManagerUpdateInput, PlanItem, PlanItemId, PlanSelection, SelectedItem,
    UpdatePlan, UpdateSeed, UpdateSelectionPolicy, VersionPolicy,
};

use crate::evaluate_seed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanningSettings {
    pub policy: VersionPolicy,
    pub now: SystemTime,
    pub min_release_age: Duration,
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
    update_plan_from_inputs(
        manager_id,
        seeds.into_iter().map(ManagerUpdateInput::Seed).collect(),
        settings,
    )
}

/// Builds a typed manager update plan from manager-owned planning inputs.
///
/// # Errors
///
/// Returns an error when generated plan item ids are invalid or duplicated.
pub fn update_plan_from_inputs(
    manager_id: ManagerId,
    inputs: Vec<ManagerUpdateInput>,
    settings: PlanningSettings,
) -> Result<UpdatePlan, DomainError> {
    let mut items = Vec::new();
    for input in inputs {
        match input {
            ManagerUpdateInput::Seed(seed) => {
                let id = plan_item_id(&manager_id, seed.installed.package_name.as_str())?;
                items.push(evaluate_seed(
                    id,
                    seed,
                    settings.policy,
                    settings.now,
                    settings.min_release_age,
                ));
            }
            ManagerUpdateInput::Skipped { installed, reason } => {
                let id = plan_item_id(&manager_id, installed.package_name.as_str())?;
                items.push(PlanItem::Skipped {
                    id,
                    installed,
                    reason,
                });
            }
            ManagerUpdateInput::ResolverError { installed, message } => {
                let id = plan_item_id(&manager_id, installed.package_name.as_str())?;
                items.push(PlanItem::ResolverError {
                    id,
                    installed,
                    message,
                });
            }
        }
    }
    UpdatePlan::new(manager_id, items)
}

fn plan_item_id(manager_id: &ManagerId, package_name: &str) -> Result<PlanItemId, DomainError> {
    PlanItemId::new(format!("{}:{package_name}", manager_id.as_str()))
}

/// Selects default batch apply items according to the manager selection policy.
///
/// # Errors
///
/// Returns an error if the generated selection does not reference the plan.
pub fn default_batch_selection(
    plan: &UpdatePlan,
    selection_policy: &UpdateSelectionPolicy,
) -> Result<PlanSelection, DomainError> {
    let selected_items = plan
        .items
        .iter()
        .filter_map(|item| match item {
            PlanItem::Update { id, candidate }
                if selection_policy.includes(&candidate.package_name) =>
            {
                Some(SelectedItem::recommended(id.clone()))
            }
            _ => None,
        })
        .collect();

    PlanSelection::new(plan, selected_items, selection_policy.clone())
}
