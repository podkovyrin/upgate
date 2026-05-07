use crate::{DomainError, PackageName, PlanItemId, UpdatePlan, VersionText};

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
            let PinTarget::Package(package_name) = &pin_change.target else {
                continue;
            };
            if !plan.contains_package(package_name) {
                return Err(DomainError::UnknownPinTarget(
                    package_name.as_str().to_owned(),
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
    pub target: SelectedTarget,
}

impl SelectedItem {
    #[must_use]
    pub fn new(plan_item_id: PlanItemId, target: SelectedTarget) -> Self {
        Self {
            plan_item_id,
            target,
        }
    }

    #[must_use]
    pub fn recommended(plan_item_id: PlanItemId) -> Self {
        Self::new(plan_item_id, SelectedTarget::Recommended)
    }

    #[must_use]
    pub fn forced_candidate(plan_item_id: PlanItemId) -> Self {
        Self::new(plan_item_id, SelectedTarget::ForcedCandidate)
    }

    #[must_use]
    pub fn alternate_exact(plan_item_id: PlanItemId, target_version: VersionText) -> Self {
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PinTarget {
    Package(PackageName),
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinChange {
    pub target: PinTarget,
    pub operation: PinOperation,
}

impl PinChange {
    #[must_use]
    pub fn new(target: PinTarget, operation: PinOperation) -> Self {
        Self { target, operation }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOperation {
    Pin,
    Unpin,
}
