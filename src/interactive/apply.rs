use std::collections::BTreeSet;

use super::choose_items_for_manager;
use crate::config::{PIN_ALL, is_pinned};
use crate::managers::{ManagerCtx, PlannedUpdate};
use crate::outcome::{ItemOutcome, ReasonCode, emit_text_outcome};

pub struct ApplySelection {
    pub selected: Vec<PlannedUpdate>,
    pub all_selected: bool,
    pub pinned_after_selection: BTreeSet<String>,
}

pub fn select_upgradable_items_with_meta(
    ctx: &ManagerCtx,
    manager_id: &'static str,
    upgradable: Vec<PlannedUpdate>,
) -> anyhow::Result<ApplySelection> {
    if upgradable.is_empty() {
        return Ok(ApplySelection {
            selected: upgradable,
            all_selected: true,
            pinned_after_selection: ctx.policy.pinned.clone(),
        });
    }

    let mut pinned = ctx.policy.pinned.clone();

    if !ctx.is_interactive_apply() {
        let total = upgradable.len();
        let selected: Vec<PlannedUpdate> = upgradable
            .into_iter()
            .filter(|item| !is_pinned(&item.name, &pinned))
            .collect();
        return Ok(ApplySelection {
            all_selected: selected.len() == total,
            selected,
            pinned_after_selection: pinned,
        });
    }

    let chosen_items = choose_items_for_manager(manager_id, &upgradable, &pinned)?;
    assert_eq!(chosen_items.len(), upgradable.len());
    let all_selected = chosen_items.iter().all(|chosen| *chosen);

    if chosen_items.iter().any(|chosen| *chosen) {
        pinned.remove(PIN_ALL);
    }

    let mut selected_items = Vec::with_capacity(upgradable.len());
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
            ReasonCode::Pinned,
            "pinned",
        ));
    }

    ctx.record_pending_pins_if_changed(&pinned);

    Ok(ApplySelection {
        selected: selected_items,
        all_selected,
        pinned_after_selection: pinned,
    })
}
