use std::collections::BTreeMap;
use std::fmt::{self, Display};

use upgate_domain::{PlanItemId, SelectedItem, SelectedUpdate, UpdateSelectionPolicy, VersionText};

use crate::{SelectionRow, SelectionRowStatus, SelectionView, TargetOption};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InteractiveSelectionState {
    rows: Vec<SelectionRow>,
    selection_policy: UpdateSelectionPolicy,
    choices: BTreeMap<PlanItemId, RowChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RowChoice {
    Update(SelectedUpdate),
    Remove {
        previous_update: Option<SelectedUpdate>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SelectionStateError {
    UnknownPlanItem(String),
    TargetUnavailable(String),
    PlanningFailed(String),
}

impl Display for SelectionStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPlanItem(id) => write!(formatter, "unknown selection row `{id}`"),
            Self::TargetUnavailable(id) => {
                write!(formatter, "selection target is unavailable for `{id}`")
            }
            Self::PlanningFailed(message) => {
                write!(formatter, "{message}")
            }
        }
    }
}

impl std::error::Error for SelectionStateError {}

impl InteractiveSelectionState {
    pub(super) fn new(view: SelectionView, selection_policy: UpdateSelectionPolicy) -> Self {
        let choices = view
            .rows
            .iter()
            .filter(|row| row.initially_selected)
            .map(|row| {
                (
                    row.plan_item_id.clone(),
                    RowChoice::Update(SelectedUpdate::Recommended),
                )
            })
            .collect();

        Self {
            rows: view.rows,
            selection_policy,
            choices,
        }
    }
    pub(super) fn is_removed(&self, id: &PlanItemId) -> bool {
        matches!(self.choices.get(id), Some(RowChoice::Remove { .. }))
    }
    pub(super) fn toggle_removal(&mut self, id: &PlanItemId) -> Result<(), SelectionStateError> {
        if let upgate_domain::RemovalSupport::Unsupported(reason) = &self.row(id)?.removal {
            return Err(SelectionStateError::TargetUnavailable(reason.clone()));
        }
        match self.choices.remove(id) {
            Some(RowChoice::Remove { previous_update }) => {
                if let Some(target) = previous_update {
                    self.choices.insert(id.clone(), RowChoice::Update(target));
                }
            }
            previous => {
                let previous_update = match previous {
                    Some(RowChoice::Update(target)) => Some(target),
                    _ => None,
                };
                self.choices
                    .insert(id.clone(), RowChoice::Remove { previous_update });
            }
        }
        Ok(())
    }
    pub(super) fn clear_removal(&mut self, id: &PlanItemId) {
        if self.is_removed(id) {
            self.choices.remove(id);
        }
    }
    pub(super) fn rows(&self) -> &[SelectionRow] {
        &self.rows
    }
    pub(super) fn selected_target(&self, plan_item_id: &PlanItemId) -> Option<&SelectedUpdate> {
        match self.choices.get(plan_item_id) {
            Some(RowChoice::Update(target)) => Some(target),
            _ => None,
        }
    }
    pub(super) fn selected_items(&self) -> Vec<SelectedItem> {
        self.choices
            .iter()
            .map(|(id, choice)| match choice {
                RowChoice::Update(target) => SelectedItem::new(id.clone(), target.clone()),
                RowChoice::Remove { .. } => SelectedItem::remove(id.clone()),
            })
            .collect()
    }
    pub(super) fn selected_count(&self) -> usize {
        self.choices.len()
    }
    pub(super) const fn selection_policy(&self) -> &UpdateSelectionPolicy {
        &self.selection_policy
    }

    /// Selects an update row's recommended target.
    ///
    /// # Errors
    ///
    /// Returns [`SelectionStateError::UnknownPlanItem`] when the id is not in the view, or
    /// [`SelectionStateError::TargetUnavailable`] when the row is not a selectable update.
    pub(super) fn select_recommended(
        &mut self,
        plan_item_id: &PlanItemId,
    ) -> Result<(), SelectionStateError> {
        if self.is_removed(plan_item_id) {
            return Ok(());
        }
        let row = self.row(plan_item_id)?;
        if row.status != SelectionRowStatus::Update {
            return Err(SelectionStateError::TargetUnavailable(
                plan_item_id.to_string(),
            ));
        }
        let package_name = row.package_name.clone();
        self.choices.insert(
            plan_item_id.clone(),
            RowChoice::Update(SelectedUpdate::Recommended),
        );
        self.selection_policy.set_included(package_name, true);
        Ok(())
    }

    /// Removes any current selection for a row.
    ///
    /// # Errors
    ///
    /// Returns [`SelectionStateError::UnknownPlanItem`] when the id is not in the view.
    pub(super) fn deselect(
        &mut self,
        plan_item_id: &PlanItemId,
    ) -> Result<(), SelectionStateError> {
        if self.is_removed(plan_item_id) {
            return Ok(());
        }
        let row = self.row(plan_item_id)?;
        let package_name = row.package_name.clone();
        let is_update = row.status == SelectionRowStatus::Update;
        self.choices.remove(plan_item_id);
        if is_update {
            self.selection_policy.set_included(package_name, false);
        }
        Ok(())
    }

    /// Selects a delayed candidate through the typed force path.
    ///
    /// # Errors
    ///
    /// Returns [`SelectionStateError::UnknownPlanItem`] when the id is not in the view, or
    /// [`SelectionStateError::TargetUnavailable`] when the row does not support forced exact
    /// execution.
    pub(super) fn force_candidate(
        &mut self,
        plan_item_id: &PlanItemId,
    ) -> Result<(), SelectionStateError> {
        if self.is_removed(plan_item_id) {
            return Ok(());
        }
        let row = self.row(plan_item_id)?;
        if !row
            .target_options
            .iter()
            .any(|option| matches!(option, TargetOption::ForcedCandidate { .. }))
        {
            return Err(SelectionStateError::TargetUnavailable(
                plan_item_id.to_string(),
            ));
        }
        self.choices.insert(
            plan_item_id.clone(),
            RowChoice::Update(SelectedUpdate::ForcePlannedCandidate),
        );
        Ok(())
    }

    /// Selects an exact target version already exposed by the typed selection view.
    ///
    /// # Errors
    ///
    /// Returns [`SelectionStateError::UnknownPlanItem`] when the id is not in the view, or
    /// [`SelectionStateError::TargetUnavailable`] when the target version is not available for the
    /// row.
    pub(super) fn choose_alternate_exact(
        &mut self,
        plan_item_id: &PlanItemId,
        target_version: VersionText,
    ) -> Result<(), SelectionStateError> {
        if self.is_removed(plan_item_id) {
            return Ok(());
        }
        let row = self.row(plan_item_id)?;
        if !row.target_options.iter().any(|option| {
            matches!(
                option,
                TargetOption::AlternateExact {
                    target_version: option_target,
                    ..
                } if option_target == &target_version
            )
        }) {
            return Err(SelectionStateError::TargetUnavailable(
                plan_item_id.to_string(),
            ));
        }
        self.choices.insert(
            plan_item_id.clone(),
            RowChoice::Update(SelectedUpdate::Exact { target_version }),
        );
        Ok(())
    }

    /// Selects the manager-resolved target for an item.
    ///
    /// # Errors
    ///
    /// Returns an error when the item is unknown or has no manager-resolved
    /// target option.
    pub(super) fn choose_manager_resolved(
        &mut self,
        plan_item_id: &PlanItemId,
    ) -> Result<(), SelectionStateError> {
        if self.is_removed(plan_item_id) {
            return Ok(());
        }
        let row = self.row(plan_item_id)?;
        if !row
            .target_options
            .iter()
            .any(|option| matches!(option, TargetOption::ManagerResolved { .. }))
        {
            return Err(SelectionStateError::TargetUnavailable(
                plan_item_id.to_string(),
            ));
        }
        self.choices.insert(
            plan_item_id.clone(),
            RowChoice::Update(SelectedUpdate::ManagerResolved),
        );
        Ok(())
    }

    fn row(&self, plan_item_id: &PlanItemId) -> Result<&SelectionRow, SelectionStateError> {
        self.rows
            .iter()
            .find(|row| row.plan_item_id == *plan_item_id)
            .ok_or_else(|| SelectionStateError::UnknownPlanItem(plan_item_id.to_string()))
    }
}
