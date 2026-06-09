use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use upgate_domain::{
    AuditLookupResult, AuditQuery, DomainError, ManagerId, ManagerUpdateInput, PlanItem,
    PlanItemId, PlanSelection, SelectedItem, ToolId, UpdatePlan, UpdateSelectionPolicy,
    VersionPolicy,
};

use crate::{audit_queries_for_seed, evaluate_seed_with_audit};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanningSettings {
    pub policy: VersionPolicy,
    pub now: SystemTime,
    pub min_release_age: Duration,
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
    finalize_plan_from_inputs(manager_id, inputs, settings, &BTreeMap::new())
}

pub fn derive_audit_queries(inputs: &[ManagerUpdateInput]) -> Vec<AuditQuery> {
    inputs
        .iter()
        .filter_map(|input| match input {
            ManagerUpdateInput::Seed(seed) => Some(audit_queries_for_seed(seed)),
            ManagerUpdateInput::Skipped { .. } | ManagerUpdateInput::ResolverError { .. } => None,
        })
        .flatten()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Builds a plan from manager inputs and previously looked-up audit evidence.
///
/// # Errors
///
/// Returns an error when generated plan item ids are invalid or duplicated.
pub fn finalize_plan_from_inputs(
    manager_id: ManagerId,
    inputs: Vec<ManagerUpdateInput>,
    settings: PlanningSettings,
    audit_results: &BTreeMap<AuditQuery, AuditLookupResult>,
) -> Result<UpdatePlan, DomainError> {
    let mut items = Vec::new();
    for input in inputs {
        match input {
            ManagerUpdateInput::Seed(seed) => {
                let id = plan_item_id(&manager_id, &seed.installed.tool_id)?;
                items.push(evaluate_seed_with_audit(
                    id,
                    seed,
                    settings.policy,
                    settings.now,
                    settings.min_release_age,
                    audit_results,
                ));
            }
            ManagerUpdateInput::Skipped { installed, reason } => {
                let id = plan_item_id(&manager_id, &installed.tool_id)?;
                items.push(PlanItem::Skipped {
                    id,
                    installed,
                    reason,
                });
            }
            ManagerUpdateInput::ResolverError { installed, message } => {
                let id = plan_item_id(&manager_id, &installed.tool_id)?;
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

fn plan_item_id(manager_id: &ManagerId, tool_id: &ToolId) -> Result<PlanItemId, DomainError> {
    PlanItemId::new(format!("{manager_id}:{tool_id}"))
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
