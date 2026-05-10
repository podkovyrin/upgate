use upnow_domain::{
    ExecutionEligibility, ManagerId, PackageName, PlanItem, PlanItemId, UpdatePlan,
    UpdateSelectionPolicy, VersionText,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionView {
    pub manager_id: ManagerId,
    pub rows: Vec<SelectionRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionRow {
    pub plan_item_id: PlanItemId,
    pub package_name: PackageName,
    pub installed_version: VersionText,
    pub target_version: Option<VersionText>,
    pub status: SelectionRowStatus,
    pub initially_selected: bool,
    pub policy_exception: bool,
    pub forced_candidate_available: bool,
    pub alternate_exact_targets: Vec<VersionText>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionRowStatus {
    Update,
    Current,
    Delayed,
    Blocked,
    Skipped,
    ResolverError,
}

#[must_use]
pub fn selection_view(
    plan: &UpdatePlan,
    selection_policy: &UpdateSelectionPolicy,
) -> SelectionView {
    let rows = plan
        .items
        .iter()
        .map(|item| selection_row(item, selection_policy))
        .collect();

    SelectionView {
        manager_id: plan.manager_id.clone(),
        rows,
    }
}

fn selection_row(item: &PlanItem, selection_policy: &UpdateSelectionPolicy) -> SelectionRow {
    match item {
        PlanItem::Update { id, candidate } => {
            let selected = selection_policy.includes(&candidate.package_name);
            let alternate_exact_targets =
                exact_targets(candidate.execution_eligibility, &candidate.target_version);
            SelectionRow {
                plan_item_id: id.clone(),
                package_name: candidate.package_name.clone(),
                installed_version: candidate.installed_version.clone(),
                target_version: Some(candidate.target_version.clone()),
                status: SelectionRowStatus::Update,
                initially_selected: selected,
                policy_exception: selection_policy.except.contains(&candidate.package_name),
                forced_candidate_available: false,
                alternate_exact_targets,
            }
        }
        PlanItem::Current { id, installed } => SelectionRow {
            plan_item_id: id.clone(),
            package_name: installed.package_name.clone(),
            installed_version: installed.installed_version.clone(),
            target_version: None,
            status: SelectionRowStatus::Current,
            initially_selected: false,
            policy_exception: selection_policy.except.contains(&installed.package_name),
            forced_candidate_available: false,
            alternate_exact_targets: Vec::new(),
        },
        PlanItem::Delayed { id, candidate, .. } => SelectionRow {
            plan_item_id: id.clone(),
            package_name: candidate.package_name.clone(),
            installed_version: candidate.installed_version.clone(),
            target_version: Some(candidate.target_version.clone()),
            status: SelectionRowStatus::Delayed,
            initially_selected: false,
            policy_exception: selection_policy.except.contains(&candidate.package_name),
            forced_candidate_available: candidate.execution_eligibility.supports_exact_target(),
            alternate_exact_targets: Vec::new(),
        },
        PlanItem::Blocked { id, seed, .. } => SelectionRow {
            plan_item_id: id.clone(),
            package_name: seed.installed.package_name.clone(),
            installed_version: seed.installed.installed_version.clone(),
            target_version: Some(seed.target_selection.target_version().clone()),
            status: SelectionRowStatus::Blocked,
            initially_selected: false,
            policy_exception: selection_policy
                .except
                .contains(&seed.installed.package_name),
            forced_candidate_available: false,
            alternate_exact_targets: Vec::new(),
        },
        PlanItem::Skipped { id, installed, .. } => SelectionRow {
            plan_item_id: id.clone(),
            package_name: installed.package_name.clone(),
            installed_version: installed.installed_version.clone(),
            target_version: None,
            status: SelectionRowStatus::Skipped,
            initially_selected: false,
            policy_exception: selection_policy.except.contains(&installed.package_name),
            forced_candidate_available: false,
            alternate_exact_targets: Vec::new(),
        },
        PlanItem::ResolverError { id, installed, .. } => SelectionRow {
            plan_item_id: id.clone(),
            package_name: installed.package_name.clone(),
            installed_version: installed.installed_version.clone(),
            target_version: None,
            status: SelectionRowStatus::ResolverError,
            initially_selected: false,
            policy_exception: selection_policy.except.contains(&installed.package_name),
            forced_candidate_available: false,
            alternate_exact_targets: Vec::new(),
        },
    }
}

fn exact_targets(
    execution_eligibility: ExecutionEligibility,
    target_version: &VersionText,
) -> Vec<VersionText> {
    if execution_eligibility.supports_exact_target() {
        vec![target_version.clone()]
    } else {
        Vec::new()
    }
}
