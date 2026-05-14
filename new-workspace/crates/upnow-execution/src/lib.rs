//! Execution behavior for the `upnow` rebuild.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

pub mod progress;

use std::fmt::{self, Display};

use upnow_domain::{
    BlockReason, ExecutionSupport, ExecutionTargetKind, ManagerCapabilities, ManagerId,
    MinAgeConstraintSupport, MissingMetadataKind, PackageName, PlanDiagnostics, PlanItem,
    PlanItemId, PlanSelection, PlannedTargetRef, SelectedUpdate, UpdateCandidate, UpdatePlan,
    UpdateSeed, VersionPolicy, VersionText,
};
use upnow_infra::{CommandCheck, CommandSpec, InfraError, ProcessRunner};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedExecutionPlan {
    pub intents: Vec<ExecutionCommandIntent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionCommandIntent {
    /// Install a concrete target version for one selected item.
    Exact(ResolvedExecutionItem),
    /// Update one selected item with the manager's native selected-update
    /// command, letting the manager choose the final target.
    NativeSelected(ResolvedExecutionItem),
    /// Update several selected items with one grouped native command when the
    /// manager supports a typed grouped shape for those items.
    GroupedNative(Vec<ResolvedExecutionItem>),
    /// Run one manager-level native update command when the selected set is
    /// equivalent to all eligible native updates.
    NativeGlobal(Vec<ResolvedExecutionItem>),
    /// Update one selected item by re-running the manager's resolver for that
    /// item. The resolved item carries any policy-bypass flags.
    ResolverNative(ResolvedExecutionItem),
    /// Run one manager-level resolver-native update command when the selected
    /// set is equivalent to all eligible resolver-native updates.
    ResolverNativeGlobal(Vec<ResolvedExecutionItem>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Item-level execution facts after a plan selection has been resolved.
///
/// Managers consume this value to build concrete commands. It keeps the plan
/// item identity attached so command results can be reported back to the
/// selected row.
pub struct ResolvedExecutionItem {
    pub plan_item_id: PlanItemId,
    pub package_name: PackageName,
    pub installed_version: VersionText,
    pub target: ResolvedExecutionTarget,
    pub execution_support: ExecutionSupport,
    pub execution_target_kind: ExecutionTargetKind,
    pub exact_target_required: bool,
    pub bypass_min_release_age: bool,
}

impl ResolvedExecutionItem {
    pub const fn known_target_version(&self) -> Option<&VersionText> {
        self.target.known_version()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedExecutionTarget {
    Known(VersionText),
    ManagerResolved,
}

impl ResolvedExecutionTarget {
    pub const fn known_version(&self) -> Option<&VersionText> {
        match self {
            Self::Known(version) => Some(version),
            Self::ManagerResolved => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionSelectionError {
    UnknownPlanItem(String),
    ItemNotExecutable(String),
    ExactTargetUnsupported(String),
    ManagerResolvedUnsupported(String),
    KnownTargetRequired(String),
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
            Self::ManagerResolvedUnsupported(id) => {
                write!(
                    formatter,
                    "plan item `{id}` does not support manager-resolved selected execution"
                )
            }
            Self::KnownTargetRequired(id) => {
                write!(
                    formatter,
                    "plan item `{id}` requires a known target version"
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
    capabilities: ManagerCapabilities,
    version_policy: VersionPolicy,
) -> Result<ResolvedExecutionPlan, ExecutionSelectionError> {
    let selected = selected_execution_items(plan, selection)?;
    if should_use_native_global_update(plan, &selected, capabilities, version_policy) {
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
    let mut grouped_native = Vec::new();
    for item in selected {
        if item.exact_target_required {
            intents.push(ExecutionCommandIntent::Exact(item));
        } else if should_use_resolver_native_update(&item, version_policy) {
            intents.push(ExecutionCommandIntent::ResolverNative(item));
        } else if should_use_grouped_native_update(&item) {
            grouped_native.push(item);
        } else if should_use_native_selected_update(&item, version_policy) {
            intents.push(ExecutionCommandIntent::NativeSelected(item));
        } else if supports_exact_target(&item) {
            intents.push(ExecutionCommandIntent::Exact(item));
        } else {
            return Err(ExecutionSelectionError::ExactTargetUnsupported(
                item.plan_item_id.as_str().to_owned(),
            ));
        }
    }
    if !grouped_native.is_empty() {
        intents.push(ExecutionCommandIntent::GroupedNative(grouped_native));
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
    pub target: ResolvedExecutionTarget,
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
/// Concrete command plus the selected items that should receive its result.
pub struct ExecutionCommand {
    pub items: Vec<ExecutionCommandItem>,
    pub command: CommandSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Selected item metadata carried beside a concrete command for result mapping.
pub struct ExecutionCommandItem {
    pub plan_item_id: PlanItemId,
    pub package_name: PackageName,
    pub installed_version: VersionText,
    pub target: ResolvedExecutionTarget,
}

impl From<&ResolvedExecutionItem> for ExecutionCommandItem {
    fn from(item: &ResolvedExecutionItem) -> Self {
        Self {
            plan_item_id: item.plan_item_id.clone(),
            package_name: item.package_name.clone(),
            installed_version: item.installed_version.clone(),
            target: item.target.clone(),
        }
    }
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
                target: item.target,
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
        let resolved = match (item, &selected.selected_update) {
            (PlanItem::Update { candidate, .. }, SelectedUpdate::Recommended) => resolved_item(
                selected.plan_item_id.clone(),
                candidate,
                known_candidate_target(candidate)?,
                false,
                false,
            ),
            (
                PlanItem::Update { candidate, .. } | PlanItem::Delayed { candidate, .. },
                SelectedUpdate::Exact { target_version },
            ) if candidate.execution_support.exact => resolved_item(
                selected.plan_item_id.clone(),
                candidate,
                ResolvedExecutionTarget::Known(target_version.clone()),
                true,
                exact_target_bypasses_min_release_age(candidate, target_version),
            ),
            (
                PlanItem::Update { candidate, .. } | PlanItem::Delayed { candidate, .. },
                SelectedUpdate::Exact { .. },
            ) if !candidate.execution_support.exact => {
                return Err(ExecutionSelectionError::ExactTargetUnsupported(
                    item.id().as_str().to_owned(),
                ));
            }
            (PlanItem::Delayed { candidate, .. }, SelectedUpdate::ForcePlannedCandidate)
                if candidate.execution_support.exact =>
            {
                resolved_item(
                    selected.plan_item_id.clone(),
                    candidate,
                    known_candidate_target(candidate)?,
                    true,
                    true,
                )
            }
            (PlanItem::Delayed { candidate, .. }, SelectedUpdate::ForcePlannedCandidate) => {
                resolve_forced_candidate(selected.plan_item_id.clone(), candidate)?
            }
            (
                PlanItem::Blocked {
                    seed,
                    reason: BlockReason::VersionPolicy(_),
                    diagnostics,
                    ..
                },
                SelectedUpdate::ForcePlannedCandidate,
            ) if seed.execution_support.exact => resolved_seed_item(
                selected.plan_item_id.clone(),
                seed,
                known_seed_target(seed)?,
                true,
                seed.target_selection
                    .target_version()
                    .is_some_and(|target_version| {
                        target_bypasses_min_release_age(diagnostics, target_version)
                    }),
            ),
            (
                PlanItem::Blocked {
                    seed,
                    reason: BlockReason::VersionPolicy(_),
                    diagnostics,
                    ..
                },
                SelectedUpdate::ForcePlannedCandidate,
            ) => resolve_forced_seed(selected.plan_item_id.clone(), seed, diagnostics)?,
            (
                PlanItem::Blocked {
                    seed,
                    reason: BlockReason::VersionPolicy(_),
                    diagnostics,
                    ..
                },
                SelectedUpdate::Exact { target_version },
            ) if seed.execution_support.exact => resolved_seed_item(
                selected.plan_item_id.clone(),
                seed,
                ResolvedExecutionTarget::Known(target_version.clone()),
                true,
                target_bypasses_min_release_age(diagnostics, target_version),
            ),
            (
                PlanItem::Blocked {
                    seed,
                    reason: BlockReason::VersionPolicy(_),
                    ..
                },
                SelectedUpdate::Exact { .. },
            ) if !seed.execution_support.exact => {
                return Err(ExecutionSelectionError::ExactTargetUnsupported(
                    item.id().as_str().to_owned(),
                ));
            }
            (
                PlanItem::Update { candidate, .. } | PlanItem::Delayed { candidate, .. },
                SelectedUpdate::ManagerResolved,
            ) => {
                resolve_manager_resolved_candidate(selected.plan_item_id.clone(), candidate, false)?
            }
            (
                PlanItem::Blocked {
                    seed,
                    reason: BlockReason::VersionPolicy(_),
                    ..
                },
                SelectedUpdate::ManagerResolved,
            ) => resolve_manager_resolved_seed(selected.plan_item_id.clone(), seed, false)?,
            (
                PlanItem::Blocked {
                    seed,
                    reason: BlockReason::MissingReleaseMetadata,
                    diagnostics,
                    ..
                },
                SelectedUpdate::ManagerResolved,
            ) if diagnostics.missing_metadata == Some(MissingMetadataKind::SelectedUpdate) => {
                resolve_manager_resolved_seed(selected.plan_item_id.clone(), seed, false)?
            }
            _ => {
                return Err(ExecutionSelectionError::ItemNotExecutable(
                    item.id().as_str().to_owned(),
                ));
            }
        };
        items.push(resolved);
    }
    Ok(items)
}

fn resolved_item(
    plan_item_id: PlanItemId,
    candidate: &UpdateCandidate,
    target: ResolvedExecutionTarget,
    exact_target_required: bool,
    bypass_min_release_age: bool,
) -> ResolvedExecutionItem {
    ResolvedExecutionItem {
        plan_item_id,
        package_name: candidate.package_name.clone(),
        installed_version: candidate.installed_version.clone(),
        target,
        execution_support: candidate.execution_support,
        execution_target_kind: candidate.execution_target_kind,
        exact_target_required,
        bypass_min_release_age,
    }
}

fn resolved_seed_item(
    plan_item_id: PlanItemId,
    seed: &UpdateSeed,
    target: ResolvedExecutionTarget,
    exact_target_required: bool,
    bypass_min_release_age: bool,
) -> ResolvedExecutionItem {
    ResolvedExecutionItem {
        plan_item_id,
        package_name: seed.installed.package_name.clone(),
        installed_version: seed.installed.installed_version.clone(),
        target,
        execution_support: seed.execution_support,
        execution_target_kind: seed.execution_target_kind,
        exact_target_required,
        bypass_min_release_age,
    }
}

fn known_candidate_target(
    candidate: &UpdateCandidate,
) -> Result<ResolvedExecutionTarget, ExecutionSelectionError> {
    match candidate.target() {
        PlannedTargetRef::Known(version) => Ok(ResolvedExecutionTarget::Known(version.clone())),
        PlannedTargetRef::ManagerResolved => Err(ExecutionSelectionError::KnownTargetRequired(
            candidate.tool_id.as_str().to_owned(),
        )),
    }
}

fn known_seed_target(
    seed: &UpdateSeed,
) -> Result<ResolvedExecutionTarget, ExecutionSelectionError> {
    match seed.target_selection.target() {
        PlannedTargetRef::Known(version) => Ok(ResolvedExecutionTarget::Known(version.clone())),
        PlannedTargetRef::ManagerResolved => Err(ExecutionSelectionError::KnownTargetRequired(
            seed.installed.tool_id.as_str().to_owned(),
        )),
    }
}

fn resolve_forced_candidate(
    plan_item_id: PlanItemId,
    candidate: &UpdateCandidate,
) -> Result<ResolvedExecutionItem, ExecutionSelectionError> {
    let support = candidate.execution_support;
    if support.resolver_native_selected.selected
        && support.resolver_native_selected.min_age_constraint == MinAgeConstraintSupport::Optional
    {
        return Ok(resolved_item(
            plan_item_id,
            candidate,
            known_candidate_target(candidate)?,
            false,
            true,
        ));
    }
    if support.native_selected && support.supports_manager_resolved_target() {
        let target =
            known_candidate_target(candidate).unwrap_or(ResolvedExecutionTarget::ManagerResolved);
        return Ok(resolved_item(plan_item_id, candidate, target, false, true));
    }
    Err(ExecutionSelectionError::ExactTargetUnsupported(
        plan_item_id.as_str().to_owned(),
    ))
}

fn resolve_forced_seed(
    plan_item_id: PlanItemId,
    seed: &UpdateSeed,
    diagnostics: &PlanDiagnostics,
) -> Result<ResolvedExecutionItem, ExecutionSelectionError> {
    let support = seed.execution_support;
    if support.resolver_native_selected.selected
        && support.resolver_native_selected.min_age_constraint == MinAgeConstraintSupport::Optional
    {
        return Ok(resolved_seed_item(
            plan_item_id,
            seed,
            known_seed_target(seed)?,
            false,
            true,
        ));
    }
    if support.native_selected && support.supports_manager_resolved_target() {
        let bypass = seed
            .target_selection
            .target_version()
            .is_some_and(|target_version| {
                target_bypasses_min_release_age(diagnostics, target_version)
            });
        let target = known_seed_target(seed).unwrap_or(ResolvedExecutionTarget::ManagerResolved);
        return Ok(resolved_seed_item(
            plan_item_id,
            seed,
            target,
            false,
            bypass,
        ));
    }
    Err(ExecutionSelectionError::ExactTargetUnsupported(
        plan_item_id.as_str().to_owned(),
    ))
}

fn resolve_manager_resolved_candidate(
    plan_item_id: PlanItemId,
    candidate: &UpdateCandidate,
    bypass_min_release_age: bool,
) -> Result<ResolvedExecutionItem, ExecutionSelectionError> {
    if !candidate
        .execution_support
        .supports_manager_resolved_target()
    {
        return Err(ExecutionSelectionError::ManagerResolvedUnsupported(
            plan_item_id.as_str().to_owned(),
        ));
    }
    Ok(resolved_item(
        plan_item_id,
        candidate,
        ResolvedExecutionTarget::ManagerResolved,
        false,
        bypass_min_release_age,
    ))
}

fn resolve_manager_resolved_seed(
    plan_item_id: PlanItemId,
    seed: &UpdateSeed,
    bypass_min_release_age: bool,
) -> Result<ResolvedExecutionItem, ExecutionSelectionError> {
    if !seed.execution_support.supports_manager_resolved_target() {
        return Err(ExecutionSelectionError::ManagerResolvedUnsupported(
            plan_item_id.as_str().to_owned(),
        ));
    }
    Ok(resolved_seed_item(
        plan_item_id,
        seed,
        ResolvedExecutionTarget::ManagerResolved,
        false,
        bypass_min_release_age,
    ))
}

fn should_use_native_global_update(
    plan: &UpdatePlan,
    selected: &[ResolvedExecutionItem],
    capabilities: ManagerCapabilities,
    version_policy: VersionPolicy,
) -> bool {
    if !capabilities.native_global_update
        || selected.is_empty()
        || selected.iter().any(|item| item.bypass_min_release_age)
        || selected.iter().any(|item| item.exact_target_required)
        || !selected
            .iter()
            .all(|item| item.execution_support.native_global)
        || !selected
            .iter()
            .all(|item| item.execution_target_kind == ExecutionTargetKind::Standard)
    {
        return false;
    }
    if version_policy != VersionPolicy::None
        && !selected
            .iter()
            .all(|item| item.execution_support.native_selected && !item.execution_support.exact)
    {
        return false;
    }
    selected_matches_all_updates(plan, selected)
}

fn should_use_resolver_native_global_update(
    plan: &UpdatePlan,
    selected: &[ResolvedExecutionItem],
    capabilities: ManagerCapabilities,
    version_policy: VersionPolicy,
) -> bool {
    capabilities.resolver_native_global_update
        && version_policy == VersionPolicy::None
        && !selected.is_empty()
        && plan
            .items
            .iter()
            .all(|item| matches!(item, PlanItem::Update { .. }))
        && selected.iter().all(|item| {
            !item.bypass_min_release_age
                && !item.exact_target_required
                && item.execution_support.resolver_native_selected.selected
        })
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
    version_policy: VersionPolicy,
) -> bool {
    item.execution_support.resolver_native_selected.selected
        && (version_policy == VersionPolicy::None
            || item.bypass_min_release_age
            || item.target == ResolvedExecutionTarget::ManagerResolved)
}

fn should_use_native_selected_update(
    item: &ResolvedExecutionItem,
    version_policy: VersionPolicy,
) -> bool {
    if item.bypass_min_release_age || item.exact_target_required {
        return false;
    }

    if !item.execution_support.native_selected {
        false
    } else if item.target == ResolvedExecutionTarget::ManagerResolved {
        true
    } else if item.execution_support.exact {
        version_policy == VersionPolicy::None
    } else {
        true
    }
}

fn should_use_grouped_native_update(item: &ResolvedExecutionItem) -> bool {
    if item.bypass_min_release_age || item.exact_target_required {
        return false;
    }

    match item.execution_target_kind {
        ExecutionTargetKind::Standard => false,
        ExecutionTargetKind::BrewFormula | ExecutionTargetKind::BrewCask => {
            item.execution_support.grouped_native || item.execution_support.native_selected
        }
    }
}

const fn supports_exact_target(item: &ResolvedExecutionItem) -> bool {
    item.execution_support.exact && matches!(item.target, ResolvedExecutionTarget::Known(_))
}

fn exact_target_bypasses_min_release_age(
    candidate: &UpdateCandidate,
    target_version: &VersionText,
) -> bool {
    target_bypasses_min_release_age(&candidate.diagnostics, target_version)
}

fn target_bypasses_min_release_age(
    diagnostics: &PlanDiagnostics,
    target_version: &VersionText,
) -> bool {
    diagnostics
        .candidates
        .iter()
        .find(|evaluated| &evaluated.version == target_version)
        .is_some_and(|evaluated| !evaluated.age_allowed)
}
