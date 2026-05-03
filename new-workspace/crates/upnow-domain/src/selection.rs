use crate::{DomainError, PackageName, PlanItemId, UpdatePlan};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSelection {
    pub selected_items: Vec<SelectedItem>,
    pub pin_changes: Vec<PinChange>,
}

impl PlanSelection {
    /// Creates a typed selection for an update plan.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::UnknownPlanItemId`] when a selected item is not in the plan.
    pub fn new(
        plan: &UpdatePlan,
        selected_items: Vec<SelectedItem>,
        pin_changes: Vec<PinChange>,
    ) -> Result<Self, DomainError> {
        for pin_change in &pin_changes {
            if !plan.contains_package(&pin_change.package_name) {
                return Err(DomainError::UnknownPinTarget(
                    pin_change.package_name.as_str().to_owned(),
                ));
            }
        }

        for selected in &selected_items {
            if plan.item(&selected.plan_item_id).is_none() {
                return Err(DomainError::UnknownPlanItemId(
                    selected.plan_item_id.as_str().to_owned(),
                ));
            }
        }

        Ok(Self {
            selected_items,
            pin_changes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedItem {
    pub plan_item_id: PlanItemId,
    pub forced: bool,
}

impl SelectedItem {
    #[must_use]
    pub fn new(plan_item_id: PlanItemId, forced: bool) -> Self {
        Self {
            plan_item_id,
            forced,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinChange {
    pub package_name: PackageName,
    pub operation: PinOperation,
}

impl PinChange {
    #[must_use]
    pub fn new(package_name: PackageName, operation: PinOperation) -> Self {
        Self {
            package_name,
            operation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOperation {
    Pin,
    Unpin,
}
