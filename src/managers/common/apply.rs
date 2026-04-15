use super::PlannedUpdate;
use crate::manager::ManagerCtx;
use crate::outcome::{ItemOutcome, ReasonCode, emit_text_outcome};
use anyhow::Result;

pub fn run_per_item_apply_flow<F>(
    ctx: &ManagerCtx,
    manager_id: &'static str,
    upgradable: Vec<PlannedUpdate>,
    apply_selected: F,
) -> Result<()>
where
    F: FnOnce(Vec<PlannedUpdate>),
{
    let selection =
        crate::interactive::apply::select_upgradable_items_with_meta(ctx, manager_id, upgradable)?;
    if selection.selected.is_empty() || ctx.is_dry_run() {
        return Ok(());
    }

    apply_selected(selection.selected);
    Ok(())
}

pub fn run_selective_or_global_apply_flow<F, G, E>(
    ctx: &ManagerCtx,
    manager_id: &'static str,
    source: &'static str,
    upgradable: Vec<PlannedUpdate>,
    apply_selected: F,
    apply_all: G,
) -> Result<()>
where
    F: FnOnce(Vec<PlannedUpdate>),
    G: FnOnce() -> std::result::Result<(), E>,
    E: std::fmt::Display,
{
    let selection =
        crate::interactive::apply::select_upgradable_items_with_meta(ctx, manager_id, upgradable)?;
    if selection.selected.is_empty() || ctx.is_dry_run() {
        return Ok(());
    }

    if selection.all_selected && selection.pinned_after_selection.is_empty() {
        apply_all_with_error_outcome(manager_id, source, apply_all);
        return Ok(());
    }

    apply_selected(selection.selected);
    Ok(())
}

fn apply_all_with_error_outcome<F, E>(manager_id: &'static str, source: &'static str, apply_all: F)
where
    F: FnOnce() -> std::result::Result<(), E>,
    E: std::fmt::Display,
{
    if let Err(err) = apply_all() {
        emit_global_apply_error(manager_id, source, err.to_string());
    }
}

fn emit_global_apply_error(manager_id: &'static str, source: &'static str, detail: String) {
    let outcome = ItemOutcome::error(
        manager_id,
        "*",
        "*",
        "*",
        source,
        ReasonCode::CommandFailed,
        detail,
    );
    emit_text_outcome(&outcome);
}
