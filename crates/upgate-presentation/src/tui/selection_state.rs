use std::collections::BTreeMap;
use std::fmt::{self, Display};

use upgate_domain::{PlanItemId, SelectedItem, SelectedUpdate, UpdateSelectionPolicy, VersionText};

use crate::{SelectionRow, SelectionRowStatus, SelectionView, TargetOption};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveSelectionState {
    rows: Vec<SelectionRow>,
    selection_policy: UpdateSelectionPolicy,
    selected_targets: BTreeMap<PlanItemId, SelectedUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionStateError {
    UnknownPlanItem(String),
    TargetUnavailable(String),
    InvalidSelection(String),
    PlanningFailed(String),
}

impl Display for SelectionStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPlanItem(id) => write!(formatter, "unknown selection row `{id}`"),
            Self::TargetUnavailable(id) => {
                write!(formatter, "selection target is unavailable for `{id}`")
            }
            Self::InvalidSelection(message) | Self::PlanningFailed(message) => {
                write!(formatter, "{message}")
            }
        }
    }
}

impl std::error::Error for SelectionStateError {}

impl InteractiveSelectionState {
    pub fn new(view: SelectionView, selection_policy: UpdateSelectionPolicy) -> Self {
        let selected_targets = view
            .rows
            .iter()
            .filter(|row| row.initially_selected)
            .map(|row| (row.plan_item_id.clone(), SelectedUpdate::Recommended))
            .collect();

        Self {
            rows: view.rows,
            selection_policy,
            selected_targets,
        }
    }
    pub fn rows(&self) -> &[SelectionRow] {
        &self.rows
    }
    pub fn selected_target(&self, plan_item_id: &PlanItemId) -> Option<&SelectedUpdate> {
        self.selected_targets.get(plan_item_id)
    }
    pub fn selected_items(&self) -> Vec<SelectedItem> {
        self.selected_targets
            .iter()
            .map(|(plan_item_id, target)| SelectedItem::new(plan_item_id.clone(), target.clone()))
            .collect()
    }
    pub const fn selection_policy(&self) -> &UpdateSelectionPolicy {
        &self.selection_policy
    }

    /// Selects an update row's recommended target.
    ///
    /// # Errors
    ///
    /// Returns [`SelectionStateError::UnknownPlanItem`] when the id is not in the view, or
    /// [`SelectionStateError::TargetUnavailable`] when the row is not a selectable update.
    pub fn select_recommended(
        &mut self,
        plan_item_id: &PlanItemId,
    ) -> Result<(), SelectionStateError> {
        let row = self.row(plan_item_id)?;
        if row.status != SelectionRowStatus::Update {
            return Err(SelectionStateError::TargetUnavailable(
                plan_item_id.to_string(),
            ));
        }
        let package_name = row.package_name.clone();
        self.selected_targets
            .insert(plan_item_id.clone(), SelectedUpdate::Recommended);
        self.selection_policy.set_included(package_name, true);
        Ok(())
    }

    /// Removes any current selection for a row.
    ///
    /// # Errors
    ///
    /// Returns [`SelectionStateError::UnknownPlanItem`] when the id is not in the view.
    pub fn deselect(&mut self, plan_item_id: &PlanItemId) -> Result<(), SelectionStateError> {
        let row = self.row(plan_item_id)?;
        let package_name = row.package_name.clone();
        let is_update = row.status == SelectionRowStatus::Update;
        self.selected_targets.remove(plan_item_id);
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
    pub fn force_candidate(
        &mut self,
        plan_item_id: &PlanItemId,
    ) -> Result<(), SelectionStateError> {
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
        self.selected_targets
            .insert(plan_item_id.clone(), SelectedUpdate::ForcePlannedCandidate);
        Ok(())
    }

    /// Selects an exact target version already exposed by the typed selection view.
    ///
    /// # Errors
    ///
    /// Returns [`SelectionStateError::UnknownPlanItem`] when the id is not in the view, or
    /// [`SelectionStateError::TargetUnavailable`] when the target version is not available for the
    /// row.
    pub fn choose_alternate_exact(
        &mut self,
        plan_item_id: &PlanItemId,
        target_version: VersionText,
    ) -> Result<(), SelectionStateError> {
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
        self.selected_targets.insert(
            plan_item_id.clone(),
            SelectedUpdate::Exact { target_version },
        );
        Ok(())
    }

    /// Selects the manager-resolved target for an item.
    ///
    /// # Errors
    ///
    /// Returns an error when the item is unknown or has no manager-resolved
    /// target option.
    pub fn choose_manager_resolved(
        &mut self,
        plan_item_id: &PlanItemId,
    ) -> Result<(), SelectionStateError> {
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
        self.selected_targets
            .insert(plan_item_id.clone(), SelectedUpdate::ManagerResolved);
        Ok(())
    }

    fn row(&self, plan_item_id: &PlanItemId) -> Result<&SelectionRow, SelectionStateError> {
        self.rows
            .iter()
            .find(|row| row.plan_item_id == *plan_item_id)
            .ok_or_else(|| SelectionStateError::UnknownPlanItem(plan_item_id.to_string()))
    }
}
