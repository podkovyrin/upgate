use crate::config::PIN_ALL;
use crate::interactive;
use crate::manager::ManagerCtx;
use crate::managers::common::PlannedUpdate;
use crate::outcome::{ItemOutcome, REASON_PINNED, emit_text_outcome};

pub(crate) fn select_upgradable_items(
    ctx: &ManagerCtx,
    manager_id: &'static str,
    upgradable: Vec<PlannedUpdate>,
) -> anyhow::Result<Vec<PlannedUpdate>> {
    if upgradable.is_empty() {
        return Ok(upgradable);
    }

    let mut pinned = ctx.policy.pinned.clone();

    if !ctx.is_interactive_apply() {
        let selected = upgradable
            .into_iter()
            .filter(|item| !is_item_pinned(&item.name, &pinned))
            .collect();
        return Ok(selected);
    }

    let chosen_items = interactive::choose_items_for_manager(manager_id, &upgradable, &pinned)?;
    assert_eq!(chosen_items.len(), upgradable.len());

    let mut selected_items = Vec::new();
    let mut pinned_items = Vec::new();

    for (item, chosen) in std::iter::zip(upgradable, chosen_items) {
        if chosen {
            pinned.remove(&item.name);
            selected_items.push(item);
        } else {
            pinned.insert(item.name.clone());
            pinned_items.push(item);
        }
    }

    for item in &selected_items {
        emit_text_outcome(&item.to_update_outcome());
    }

    for item in &pinned_items {
        emit_text_outcome(&ItemOutcome::skipped(
            manager_id,
            item.name.clone(),
            item.current.clone(),
            item.target.clone(),
            "selected",
            REASON_PINNED,
            "pinned",
        ));
    }

    ctx.record_pending_pins_if_changed(pinned);

    Ok(selected_items)
}

pub(crate) fn should_apply_global_manager(
    ctx: &ManagerCtx,
    manager_id: &'static str,
    upgradable: &[PlannedUpdate],
) -> anyhow::Result<bool> {
    if upgradable.is_empty() {
        return Ok(false);
    }

    let apply =
        interactive::confirm_global_manager_apply(manager_id, ctx.policy.pinned.is_empty())?;
    if !apply {
        emit_global_skipped_items(upgradable);
        let next_pins = std::iter::once(PIN_ALL.to_string()).collect();
        ctx.record_pending_pins_if_changed(next_pins);
        return Ok(false);
    }

    ctx.record_pending_pins_if_changed(std::collections::BTreeSet::new());

    Ok(true)
}

fn emit_global_skipped_items(upgradable: &[PlannedUpdate]) {
    for item in upgradable {
        emit_text_outcome(&ItemOutcome::skipped(
            item.manager,
            item.name.clone(),
            item.current.clone(),
            item.target.clone(),
            item.source,
            REASON_PINNED,
            "pinned",
        ));
    }
}

fn is_item_pinned(name: &str, pinned: &std::collections::BTreeSet<String>) -> bool {
    pinned.contains(name) || pinned.contains(PIN_ALL)
}
