use std::fs;
use std::path::Path;

use serde::Serialize;
use upgate_domain::{PlanItem, PlanSelection, SelectedUpdate, UpdatePlan, VersionText};

use crate::AppError;

const APPLY_SNAPSHOT_FILE: &str = "snapshot.json";

#[derive(Debug, Serialize)]
struct ApplySnapshotRow<'a> {
    manager: &'a str,
    tool_name: &'a str,
    current: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<&'a str>,
    action: &'static str,
}

pub fn write_apply_snapshot_for_selections<'a>(
    selections: impl IntoIterator<Item = (&'a UpdatePlan, &'a PlanSelection)>,
    log_dir: &Path,
) -> Result<(), AppError> {
    let rows = selections
        .into_iter()
        .flat_map(|(plan, selection)| snapshot_rows(plan, selection))
        .collect::<Vec<_>>();
    let path = log_dir.join(APPLY_SNAPSHOT_FILE);
    let bytes = serde_json::to_vec_pretty(&rows).map_err(|err| {
        AppError::Execution(format!(
            "failed to serialize apply snapshot {}: {err}",
            path.display()
        ))
    })?;
    fs::write(&path, bytes).map_err(|err| {
        AppError::Execution(format!(
            "failed to write apply snapshot {}: {err}",
            path.display()
        ))
    })
}

fn snapshot_rows<'a>(
    plan: &'a UpdatePlan,
    selection: &'a PlanSelection,
) -> Vec<ApplySnapshotRow<'a>> {
    plan.items
        .iter()
        .map(|item| {
            let selected_update = selection
                .selected_items
                .iter()
                .find(|selected| selected.plan_item_id == *item.id())
                .map(|selected| &selected.selected_update);
            snapshot_row(plan, item, selected_update)
        })
        .collect()
}

fn snapshot_row<'a>(
    plan: &'a UpdatePlan,
    item: &'a PlanItem,
    selected_update: Option<&'a SelectedUpdate>,
) -> ApplySnapshotRow<'a> {
    ApplySnapshotRow {
        manager: plan.manager_id.as_str(),
        tool_name: snapshot_tool_name(item),
        current: snapshot_current_version(item),
        target: snapshot_target(item, selected_update),
        action: snapshot_action(item, selected_update),
    }
}

const fn snapshot_action(
    item: &PlanItem,
    selected_update: Option<&SelectedUpdate>,
) -> &'static str {
    if selected_update.is_some() {
        return "update";
    }

    match item {
        PlanItem::Update { .. } | PlanItem::Skipped { .. } => "skipped",
        PlanItem::Current { .. } => "current",
        PlanItem::Delayed { .. } => "delayed",
        PlanItem::Blocked { .. } => "blocked",
        PlanItem::ResolverError { .. } => "error",
    }
}

fn snapshot_tool_name(item: &PlanItem) -> &str {
    match item {
        PlanItem::Update { candidate, .. } | PlanItem::Delayed { candidate, .. } => {
            candidate.package_name.as_str()
        }
        PlanItem::Current { installed, .. }
        | PlanItem::Skipped { installed, .. }
        | PlanItem::ResolverError { installed, .. } => installed.tool_name.as_str(),
        PlanItem::Blocked { seed, .. } => seed.installed.tool_name.as_str(),
    }
}

fn snapshot_current_version(item: &PlanItem) -> &str {
    match item {
        PlanItem::Update { candidate, .. } | PlanItem::Delayed { candidate, .. } => {
            candidate.installed_version.as_str()
        }
        PlanItem::Current { installed, .. }
        | PlanItem::Skipped { installed, .. }
        | PlanItem::ResolverError { installed, .. } => installed.installed_version.as_str(),
        PlanItem::Blocked { seed, .. } => seed.installed.installed_version.as_str(),
    }
}

fn snapshot_target<'a>(
    item: &'a PlanItem,
    selected_update: Option<&'a SelectedUpdate>,
) -> Option<&'a str> {
    match selected_update {
        Some(SelectedUpdate::Exact { target_version }) => Some(target_version.as_str()),
        Some(SelectedUpdate::ManagerResolved) => None,
        Some(SelectedUpdate::Recommended | SelectedUpdate::ForcePlannedCandidate) | None => {
            snapshot_plan_target(item)
        }
    }
}

fn snapshot_plan_target(item: &PlanItem) -> Option<&str> {
    match item {
        PlanItem::Update { candidate, .. } | PlanItem::Delayed { candidate, .. } => {
            candidate.target_version().map(VersionText::as_str)
        }
        PlanItem::Blocked { seed, .. } => seed
            .target_selection
            .target_version()
            .map(VersionText::as_str),
        PlanItem::Current { .. } | PlanItem::Skipped { .. } | PlanItem::ResolverError { .. } => {
            None
        }
    }
}
