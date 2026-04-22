use anyhow::Result;

use super::PlannedUpdate;
use crate::managers::runtime::ManagerCtx;
use crate::managers::shared::versioning::policy::VersionPolicy;
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

    // When version-policy filtering is enabled, prefer per-item apply so the
    // exact selected target is honored and cannot widen via manager-global logic.
    if should_apply_all(
        ctx.policy.version_policy,
        selection.all_selected,
        selection.pinned_after_selection.is_empty(),
    ) {
        apply_all_with_error_outcome(manager_id, apply_all);
        return Ok(());
    }

    apply_selected(selection.selected);
    Ok(())
}

fn should_apply_all(
    version_policy: VersionPolicy,
    all_selected: bool,
    has_no_pins_after_selection: bool,
) -> bool {
    version_policy == VersionPolicy::Disabled && all_selected && has_no_pins_after_selection
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_apply_allowed_when_policy_disabled_and_everything_selected() {
        assert!(should_apply_all(VersionPolicy::Disabled, true, true));
    }

    #[test]
    fn global_apply_blocked_when_policy_enabled() {
        for policy in [
            VersionPolicy::Stable,
            VersionPolicy::SameTrack,
            VersionPolicy::Any,
        ] {
            assert!(!should_apply_all(policy, true, true), "policy={policy:?}");
        }
    }

    #[test]
    fn global_apply_blocked_when_not_everything_selected_or_pins_remain() {
        assert!(!should_apply_all(VersionPolicy::Disabled, false, true));
        assert!(!should_apply_all(VersionPolicy::Disabled, true, false));
    }
}
