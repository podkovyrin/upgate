use std::collections::BTreeSet;

use super::choose_apply_candidates_for_manager;
use crate::config::{PIN_ALL, is_pinned};
use crate::managers::{ApplyCandidate, ManagerCtx, PlannedUpdate};
use crate::outcome::{ItemOutcome, ReasonCode, emit_text_outcome};

pub struct ApplySelection {
    pub selected: Vec<PlannedUpdate>,
    pub all_selected: bool,
    pub pinned_after_selection: BTreeSet<String>,
}

pub fn select_apply_candidates_with_meta(
    ctx: &ManagerCtx,
    manager_id: &'static str,
    candidates: Vec<ApplyCandidate>,
) -> anyhow::Result<ApplySelection> {
    if candidates.is_empty() {
        return Ok(ApplySelection {
            selected: Vec::new(),
            all_selected: true,
            pinned_after_selection: ctx.policy.pinned.clone(),
        });
    }

    let pinned = ctx.policy.pinned.clone();

    if !ctx.is_interactive_apply() {
        let total = candidates
            .iter()
            .filter(|candidate| candidate.is_visible_by_default())
            .count();
        let selected: Vec<PlannedUpdate> = candidates
            .into_iter()
            .filter(ApplyCandidate::is_visible_by_default)
            .map(ApplyCandidate::into_update)
            .filter(|item| !is_pinned(&item.name, &pinned))
            .collect();
        return Ok(ApplySelection {
            all_selected: selected.len() == total,
            selected,
            pinned_after_selection: pinned,
        });
    }

    let chosen_items = choose_apply_candidates_for_manager(manager_id, &candidates, &pinned)?;

    let merged = merge_chosen_candidates(candidates, chosen_items, pinned);

    for item in &merged.selection.selected {
        emit_text_outcome(&item.to_update_outcome());
    }

    for item in &merged.pinned_items {
        emit_text_outcome(&ItemOutcome::skipped(
            manager_id,
            item.name.clone(),
            item.current.clone(),
            item.target.clone(),
            ReasonCode::Pinned,
            "pinned",
        ));
    }

    ctx.record_pending_pins_if_changed(&merged.selection.pinned_after_selection);

    Ok(merged.selection)
}

struct MergedApplySelection {
    selection: ApplySelection,
    pinned_items: Vec<PlannedUpdate>,
}

fn merge_chosen_candidates(
    candidates: Vec<ApplyCandidate>,
    chosen_items: Vec<Option<PlannedUpdate>>,
    mut pinned: BTreeSet<String>,
) -> MergedApplySelection {
    assert_eq!(chosen_items.len(), candidates.len());

    if std::iter::zip(&candidates, &chosen_items)
        .any(|(candidate, chosen)| candidate.is_visible_by_default() && chosen.is_some())
    {
        pinned.remove(PIN_ALL);
    }

    let mut selected_items = Vec::with_capacity(candidates.len());
    let mut pinned_items = Vec::new();
    let mut selected_recommended = 0usize;
    let mut selected_forced = 0usize;
    let mut selected_alternate = 0usize;
    let recommended_total = candidates
        .iter()
        .filter(|candidate| candidate.is_visible_by_default())
        .count();

    for (candidate, chosen) in std::iter::zip(candidates, chosen_items) {
        let is_recommended = candidate.is_visible_by_default();
        let is_force_candidate = candidate.is_force_candidate();
        let item = candidate.into_update();

        match (chosen, is_recommended, is_force_candidate) {
            (Some(selected), true, _) => {
                if selected.target != item.target {
                    selected_alternate += 1;
                }
                pinned.remove(&selected.name);
                selected_recommended += 1;
                selected_items.push(selected);
            }
            (Some(selected), false, true) => {
                selected_forced += 1;
                selected_items.push(selected);
            }
            (None, true, _) => {
                pinned.insert(item.name.clone());
                pinned_items.push(item);
            }
            _ => {}
        }
    }

    MergedApplySelection {
        selection: ApplySelection {
            selected: selected_items,
            all_selected: selected_recommended == recommended_total
                && selected_forced == 0
                && selected_alternate == 0,
            pinned_after_selection: pinned,
        },
        pinned_items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned_update(name: &str, target: &str) -> PlannedUpdate {
        PlannedUpdate {
            manager: "test",
            name: name.to_string(),
            current: "1.0.0".to_string(),
            target: target.to_string(),
            delayed_latest: None,
            version_policy: None,
            apply_spec_base: None,
            gate_bypass: Default::default(),
        }
    }

    #[test]
    fn selected_forced_candidate_is_merged_without_pin_changes() {
        let candidates = vec![
            ApplyCandidate::recommended(planned_update("ready", "1.1.0")),
            ApplyCandidate::force_candidate(planned_update("fresh", "2.0.0")),
        ];
        let pinned = BTreeSet::from(["fresh".to_string()]);

        let merged = merge_chosen_candidates(
            candidates,
            vec![
                Some(planned_update("ready", "1.1.0")),
                Some(planned_update("fresh", "2.0.0")),
            ],
            pinned.clone(),
        );

        assert_eq!(
            merged
                .selection
                .selected
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["ready", "fresh"]
        );
        assert_eq!(merged.selection.pinned_after_selection, pinned);
        assert!(merged.pinned_items.is_empty());
    }

    #[test]
    fn forced_selection_blocks_global_apply_shortcut() {
        let candidates = vec![
            ApplyCandidate::recommended(planned_update("ready", "1.1.0")),
            ApplyCandidate::force_candidate(planned_update("fresh", "2.0.0")),
        ];

        let merged = merge_chosen_candidates(
            candidates,
            vec![
                Some(planned_update("ready", "1.1.0")),
                Some(planned_update("fresh", "2.0.0")),
            ],
            BTreeSet::new(),
        );

        assert!(!merged.selection.all_selected);
    }

    #[test]
    fn deselected_recommended_candidate_updates_pins() {
        let candidates = vec![
            ApplyCandidate::recommended(planned_update("ready", "1.1.0")),
            ApplyCandidate::force_candidate(planned_update("fresh", "2.0.0")),
        ];

        let merged = merge_chosen_candidates(candidates, vec![None, None], BTreeSet::new());

        assert!(merged.selection.selected.is_empty());
        assert_eq!(
            merged.selection.pinned_after_selection,
            BTreeSet::from(["ready".to_string()])
        );
        assert_eq!(merged.pinned_items[0].name, "ready");
    }
}
