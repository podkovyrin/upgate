use anyhow::Result;

use super::PlannedUpdate;
use crate::managers::runtime::ManagerCtx;
use crate::outcome::{ItemOutcome, ReasonCode, emit_text_outcome};

pub fn run_per_item_apply_flow<F>(
    ctx: &ManagerCtx,
    manager_id: &'static str,
    upgradable: Vec<PlannedUpdate>,
    apply_selected: F,
) -> Result<()>
where
    F: FnOnce(Vec<PlannedUpdate>),
{
    let Some(selection) = resolve_apply_selection(ctx, manager_id, upgradable)? else {
        return Ok(());
    };

    apply_selected(selection.selected);
    Ok(())
}

pub fn run_selective_or_global_apply_flow<F, G, E>(
    ctx: &ManagerCtx,
    manager_id: &'static str,
    upgradable: Vec<PlannedUpdate>,
    apply_selected: F,
    apply_all: G,
) -> Result<()>
where
    F: FnOnce(Vec<PlannedUpdate>),
    G: FnOnce() -> std::result::Result<(), E>,
    E: std::fmt::Display,
{
    let Some(selection) = resolve_apply_selection(ctx, manager_id, upgradable)? else {
        return Ok(());
    };

    if selection.all_selected && selection.pinned_after_selection.is_empty() {
        apply_all_with_error_outcome(manager_id, apply_all);
        return Ok(());
    }

    apply_selected(selection.selected);
    Ok(())
}

fn resolve_apply_selection(
    ctx: &ManagerCtx,
    manager_id: &'static str,
    upgradable: Vec<PlannedUpdate>,
) -> Result<Option<crate::interactive::apply::ApplySelection>> {
    let selection =
        crate::interactive::apply::select_upgradable_items_with_meta(ctx, manager_id, upgradable)?;

    if selection.selected.is_empty() || ctx.is_dry_run() {
        return Ok(None);
    }

    Ok(Some(selection))
}

fn apply_all_with_error_outcome<F, E>(manager_id: &'static str, apply_all: F)
where
    F: FnOnce() -> std::result::Result<(), E>,
    E: std::fmt::Display,
{
    if let Err(err) = apply_all() {
        emit_apply_error(manager_id, "*", "*", "*", err);
    }
}

pub fn emit_apply_error(
    manager_id: &'static str,
    name: impl Into<String>,
    current: impl Into<String>,
    target: impl Into<String>,
    detail: impl std::fmt::Display,
) {
    let outcome = ItemOutcome::error(
        manager_id,
        name,
        current,
        target,
        ReasonCode::CommandFailed,
        detail.to_string(),
    );
    emit_text_outcome(&outcome);
}
