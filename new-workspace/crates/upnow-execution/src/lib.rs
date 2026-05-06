//! Execution behavior for the `upnow` rebuild.

use std::fmt::{self, Display};

use upnow_domain::{
    ExecutionEligibility, ManagerId, PackageName, PlanItem, PlanItemId, PlanSelection,
    UpdateCandidate, UpdatePlan, VersionPolicy, VersionText,
};
use upnow_infra::{CommandCheck, CommandSpec, InfraError, ProcessRunner};

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionCapabilities {
    pub exact_target: bool,
    pub native_update: bool,
    pub native_global_update: bool,
    pub resolver_native_update: bool,
    pub resolver_native_global_update: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedExecutionPlan {
    pub intents: Vec<ExecutionCommandIntent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionCommandIntent {
    Exact(ResolvedExecutionItem),
    NativeSelected(ResolvedExecutionItem),
    NativeGlobal(Vec<ResolvedExecutionItem>),
    ResolverNative(ResolvedExecutionItem),
    ResolverNativeGlobal(Vec<ResolvedExecutionItem>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedExecutionItem {
    pub plan_item_id: PlanItemId,
    pub package_name: PackageName,
    pub installed_version: VersionText,
    pub target_version: VersionText,
    pub execution_eligibility: ExecutionEligibility,
    pub forced: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionSelectionError {
    UnknownPlanItem(String),
    ItemNotExecutable(String),
    ExactTargetUnsupported(String),
}

impl Display for ExecutionSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPlanItem(id) => write!(formatter, "unknown selected plan item `{id}`"),
            Self::ItemNotExecutable(id) => write!(formatter, "plan item `{id}` is not executable"),
            Self::ExactTargetUnsupported(id) => {
                write!(
                    formatter,
                    "plan item `{id}` does not support exact target execution"
                )
            }
        }
    }
}

impl std::error::Error for ExecutionSelectionError {}

/// Resolves a typed plan selection into executable command intents.
///
/// # Errors
///
/// Returns an error when a selected item is not executable or exact execution
/// is required but unsupported.
pub fn resolve_selection_for_execution(
    plan: &UpdatePlan,
    selection: &PlanSelection,
    capabilities: ExecutionCapabilities,
    version_policy: VersionPolicy,
) -> Result<ResolvedExecutionPlan, ExecutionSelectionError> {
    let selected = selected_execution_items(plan, selection)?;
    if should_use_native_global_update(plan, &selected, capabilities) {
        return Ok(ResolvedExecutionPlan {
            intents: vec![ExecutionCommandIntent::NativeGlobal(selected)],
        });
    }
    if should_use_resolver_native_global_update(plan, &selected, capabilities, version_policy) {
        return Ok(ResolvedExecutionPlan {
            intents: vec![ExecutionCommandIntent::ResolverNativeGlobal(selected)],
        });
    }

    let mut intents = Vec::new();
    for item in selected {
        if should_use_resolver_native_update(&item, capabilities, version_policy) {
            intents.push(ExecutionCommandIntent::ResolverNative(item));
        } else if should_use_native_selected_update(&item, capabilities, version_policy) {
            intents.push(ExecutionCommandIntent::NativeSelected(item));
        } else if supports_exact_target(&item) && capabilities.exact_target {
            intents.push(ExecutionCommandIntent::Exact(item));
        } else {
            return Err(ExecutionSelectionError::ExactTargetUnsupported(
                item.plan_item_id.as_str().to_owned(),
            ));
        }
    }
    Ok(ResolvedExecutionPlan { intents })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReport {
    pub manager_id: ManagerId,
    pub items: Vec<ExecutionItemResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionItemResult {
    pub plan_item_id: PlanItemId,
    pub package_name: PackageName,
    pub installed_version: VersionText,
    pub target_version: VersionText,
    pub status: ExecutionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionStatus {
    Succeeded {
        command: String,
        skipped_mutation: bool,
    },
    Failed {
        command: String,
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionCommand {
    pub items: Vec<ExecutionCommandItem>,
    pub command: CommandSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionCommandItem {
    pub plan_item_id: PlanItemId,
    pub package_name: PackageName,
    pub installed_version: VersionText,
    pub target_version: VersionText,
}

/// Executes concrete commands produced by a manager.
///
/// # Errors
///
/// Returns an infrastructure error when command execution is interrupted.
pub fn execute_commands(
    manager_id: ManagerId,
    commands: Vec<ExecutionCommand>,
    process: &ProcessRunner,
) -> Result<ExecutionReport, InfraError> {
    let mut items = Vec::new();
    for command in commands {
        let command_display = command.command.display();
        let status = match process.run(&command.command, &CommandCheck::Success) {
            Ok(output) => ExecutionStatus::Succeeded {
                command: command_display,
                skipped_mutation: output.skipped_mutation(),
            },
            Err(err) if err.is_interruption() => return Err(err),
            Err(err) => ExecutionStatus::Failed {
                command: command_display,
                detail: err.to_string(),
            },
        };
        for item in command.items {
            items.push(ExecutionItemResult {
                plan_item_id: item.plan_item_id,
                package_name: item.package_name,
                installed_version: item.installed_version,
                target_version: item.target_version,
                status: status.clone(),
            });
        }
    }

    Ok(ExecutionReport { manager_id, items })
}

fn selected_execution_items(
    plan: &UpdatePlan,
    selection: &PlanSelection,
) -> Result<Vec<ResolvedExecutionItem>, ExecutionSelectionError> {
    let mut items = Vec::new();
    for selected in &selection.selected_items {
        let item = plan.item(&selected.plan_item_id).ok_or_else(|| {
            ExecutionSelectionError::UnknownPlanItem(selected.plan_item_id.as_str().to_owned())
        })?;
        let candidate = match item {
            PlanItem::Update { candidate, .. } => candidate,
            PlanItem::Delayed { candidate, .. }
                if selected.forced && candidate.execution_eligibility.supports_exact_target() =>
            {
                candidate
            }
            PlanItem::Delayed { .. } if selected.forced => {
                return Err(ExecutionSelectionError::ExactTargetUnsupported(
                    item.id().as_str().to_owned(),
                ));
            }
            _ => {
                return Err(ExecutionSelectionError::ItemNotExecutable(
                    item.id().as_str().to_owned(),
                ));
            }
        };
        items.push(resolved_item(
            selected.plan_item_id.clone(),
            candidate,
            selected.forced,
        ));
    }
    Ok(items)
}

fn resolved_item(
    plan_item_id: PlanItemId,
    candidate: &UpdateCandidate,
    forced: bool,
) -> ResolvedExecutionItem {
    ResolvedExecutionItem {
        plan_item_id,
        package_name: candidate.package_name.clone(),
        installed_version: candidate.installed_version.clone(),
        target_version: candidate.target_version.clone(),
        execution_eligibility: candidate.execution_eligibility,
        forced,
    }
}

fn should_use_native_global_update(
    plan: &UpdatePlan,
    selected: &[ResolvedExecutionItem],
    capabilities: ExecutionCapabilities,
) -> bool {
    if !capabilities.native_global_update
        || selected.is_empty()
        || selected.iter().any(|item| item.forced)
    {
        return false;
    }
    selected_matches_all_updates(plan, selected)
}

fn should_use_resolver_native_global_update(
    plan: &UpdatePlan,
    selected: &[ResolvedExecutionItem],
    capabilities: ExecutionCapabilities,
    version_policy: VersionPolicy,
) -> bool {
    capabilities.resolver_native_global_update
        && version_policy == VersionPolicy::None
        && !selected.is_empty()
        && plan
            .items
            .iter()
            .all(|item| matches!(item, PlanItem::Update { .. }))
        && selected
            .iter()
            .all(|item| !item.forced && item.execution_eligibility.supports_resolver_native())
        && selected_matches_all_updates(plan, selected)
}

fn selected_matches_all_updates(plan: &UpdatePlan, selected: &[ResolvedExecutionItem]) -> bool {
    let update_ids = plan
        .items
        .iter()
        .filter_map(|item| match item {
            PlanItem::Update { id, .. } => Some(id),
            _ => None,
        })
        .collect::<Vec<_>>();
    update_ids.len() == selected.len()
        && update_ids.iter().all(|id| {
            selected
                .iter()
                .any(|selected_item| selected_item.plan_item_id == **id)
        })
}

fn should_use_resolver_native_update(
    item: &ResolvedExecutionItem,
    capabilities: ExecutionCapabilities,
    version_policy: VersionPolicy,
) -> bool {
    capabilities.resolver_native_update
        && version_policy == VersionPolicy::None
        && item.execution_eligibility.supports_resolver_native()
}

fn should_use_native_selected_update(
    item: &ResolvedExecutionItem,
    capabilities: ExecutionCapabilities,
    version_policy: VersionPolicy,
) -> bool {
    capabilities.native_update
        && version_policy == VersionPolicy::None
        && !item.forced
        && matches!(
            item.execution_eligibility,
            upnow_domain::ExecutionEligibility::NativeOrExact
                | upnow_domain::ExecutionEligibility::NativeOnly
        )
}

fn supports_exact_target(item: &ResolvedExecutionItem) -> bool {
    item.execution_eligibility.supports_exact_target()
}
