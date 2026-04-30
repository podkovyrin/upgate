use super::PlannedUpdate;
use crate::interactive::apply::ApplySelection;
use crate::managers::runtime::ManagerCtx;
use crate::managers::shared::versioning::policy::VersionPolicy;
use crate::outcome::{ItemOutcome, ReasonCode, emit_text_outcome};

pub fn apply_per_item_selection<F>(ctx: &ManagerCtx, selection: ApplySelection, apply_selected: F)
where
    F: FnOnce(Vec<PlannedUpdate>),
{
    if selection.selected.is_empty() || ctx.is_dry_run() {
        return;
    }

    apply_selected(selection.selected);
}

pub fn apply_selective_or_global_selection<F, G, E>(
    ctx: &ManagerCtx,
    manager_id: &'static str,
    selection: ApplySelection,
    apply_selected: F,
    apply_all: G,
) where
    F: FnOnce(Vec<PlannedUpdate>),
    G: FnOnce() -> std::result::Result<(), E>,
    E: std::fmt::Display,
{
    if selection.selected.is_empty() || ctx.is_dry_run() {
        return;
    }

    if should_apply_all(
        ctx.policy.version_policy,
        selection.all_selected,
        selection.pinned_after_selection.is_empty(),
    ) {
        apply_all_with_error_outcome(manager_id, apply_all);
        return;
    }

    apply_selected(selection.selected);
}

fn should_apply_all(
    version_policy: VersionPolicy,
    all_selected: bool,
    has_no_pins_after_selection: bool,
) -> bool {
    version_policy == VersionPolicy::Disabled && all_selected && has_no_pins_after_selection
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
