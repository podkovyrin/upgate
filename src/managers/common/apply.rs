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
    let selected = crate::interactive::apply::select_upgradable_items(ctx, manager_id, upgradable)?;
    if selected.is_empty() || ctx.is_dry_run() {
        return Ok(());
    }

    apply_selected(selected);
    Ok(())
}

pub fn run_global_apply_flow<F, E>(
    ctx: &ManagerCtx,
    manager_id: &'static str,
    source: &'static str,
    upgradable: Vec<PlannedUpdate>,
    apply_all: F,
) -> Result<()>
where
    F: FnOnce() -> std::result::Result<(), E>,
    E: std::fmt::Display,
{
    if upgradable.is_empty() || ctx.is_dry_run() {
        return Ok(());
    }

    if ctx.is_interactive_apply() {
        if !crate::interactive::apply::should_apply_global_manager(ctx, manager_id, &upgradable)? {
            return Ok(());
        }

        for item in upgradable {
            emit_text_outcome(&item.to_update_outcome());
        }
    }

    if let Err(err) = apply_all() {
        let outcome = ItemOutcome::error(
            manager_id,
            "*",
            "*",
            "*",
            source,
            ReasonCode::CommandFailed,
            err.to_string(),
        );
        emit_text_outcome(&outcome);
    }

    Ok(())
}
