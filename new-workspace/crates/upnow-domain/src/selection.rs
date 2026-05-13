use std::collections::BTreeSet;

use crate::{DomainError, PackageName, PlanItemId, UpdatePlan, VersionText};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSelection {
    pub selected_items: Vec<SelectedItem>,
    pub selection_policy: UpdateSelectionPolicy,
}

impl PlanSelection {
    /// Creates a typed selection for an update plan.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::UnknownPlanItemId`] when a selected item is not in the plan, or
    /// [`DomainError::UnknownSelectionPackage`] when a selection-policy exception package is not
    /// represented in the plan.
    pub fn new(
        plan: &UpdatePlan,
        selected_items: Vec<SelectedItem>,
        selection_policy: UpdateSelectionPolicy,
    ) -> Result<Self, DomainError> {
        let mut seen = Vec::new();
        for selected in &selected_items {
            if plan.item(&selected.plan_item_id).is_none() {
                return Err(DomainError::UnknownPlanItemId(
                    selected.plan_item_id.as_str().to_owned(),
                ));
            }
            if seen.contains(&selected.plan_item_id) {
                return Err(DomainError::DuplicateSelectedPlanItemId(
                    selected.plan_item_id.as_str().to_owned(),
                ));
            }
            seen.push(selected.plan_item_id.clone());
        }
        for package_name in &selection_policy.except {
            if !plan.contains_package(package_name) {
                return Err(DomainError::UnknownSelectionPackage(
                    package_name.as_str().to_owned(),
                ));
            }
        }

        Ok(Self {
            selected_items,
            selection_policy,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedItem {
    pub plan_item_id: PlanItemId,
    pub target: SelectedTarget,
}

impl SelectedItem {
    pub const fn new(plan_item_id: PlanItemId, target: SelectedTarget) -> Self {
        Self {
            plan_item_id,
            target,
        }
    }
    pub const fn recommended(plan_item_id: PlanItemId) -> Self {
        Self::new(plan_item_id, SelectedTarget::Recommended)
    }
    pub const fn forced_candidate(plan_item_id: PlanItemId) -> Self {
        Self::new(plan_item_id, SelectedTarget::ForcedCandidate)
    }
    pub const fn alternate_exact(plan_item_id: PlanItemId, target_version: VersionText) -> Self {
        Self::new(
            plan_item_id,
            SelectedTarget::AlternateExact { target_version },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectedTarget {
    Recommended,
    ForcedCandidate,
    AlternateExact { target_version: VersionText },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSelectionPolicy {
    pub mode: UpdateSelectionMode,
    pub except: BTreeSet<PackageName>,
}

impl UpdateSelectionPolicy {
    pub const fn include_all() -> Self {
        Self {
            mode: UpdateSelectionMode::Include,
            except: BTreeSet::new(),
        }
    }
    pub const fn skip_all() -> Self {
        Self {
            mode: UpdateSelectionMode::Skip,
            except: BTreeSet::new(),
        }
    }
    pub fn is_default(&self) -> bool {
        self.mode == UpdateSelectionMode::Include && self.except.is_empty()
    }
    pub fn includes(&self, package: &PackageName) -> bool {
        let is_exception = self.except.contains(package);

        match self.mode {
            UpdateSelectionMode::Include => !is_exception,
            UpdateSelectionMode::Skip => is_exception,
        }
    }

    pub fn set_included(&mut self, package: PackageName, included: bool) {
        let is_opposite_of_mode = match self.mode {
            UpdateSelectionMode::Include => !included,
            UpdateSelectionMode::Skip => included,
        };

        if is_opposite_of_mode {
            self.except.insert(package);
        } else {
            self.except.remove(&package);
        }
    }
}

impl Default for UpdateSelectionPolicy {
    fn default() -> Self {
        Self::include_all()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateSelectionMode {
    Include,
    Skip,
}
