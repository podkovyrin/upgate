use upnow_domain::{
    DelayReason, ExecutionEligibility, ManagerId, PackageName, PlanItem, PlanItemId,
    SelectedTarget, ToolId, UpdateCandidate, UpdatePlan, UpdateSelectionMode,
    UpdateSelectionPolicy, VersionScheme, VersionText,
};
use upnow_planning::selection_view;
use upnow_presentation::tui::InteractiveSelectionState;

#[test]
fn include_mode_deselect_adds_exception() {
    let plan = plan(vec![update(
        "pnpm:alpha",
        "alpha",
        ExecutionEligibility::NativeOrExact,
    )]);
    let mut state = state(&plan, UpdateSelectionPolicy::default());

    state
        .deselect(&plan_item_id("pnpm:alpha"))
        .expect("known row");
    let selection = state.plan_selection(&plan).expect("valid selection");

    assert!(selection.selected_items.is_empty());
    assert_eq!(
        selection.selection_policy.mode,
        UpdateSelectionMode::Include
    );
    assert_eq!(
        selection.selection_policy.except,
        [package("alpha")].into_iter().collect()
    );
}

#[test]
fn include_mode_reselect_removes_exception() {
    let plan = plan(vec![update(
        "pnpm:alpha",
        "alpha",
        ExecutionEligibility::NativeOrExact,
    )]);
    let policy = UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Include,
        except: [package("alpha")].into_iter().collect(),
    };
    let mut state = state(&plan, policy);

    state.unpin(&plan_item_id("pnpm:alpha")).expect("known row");
    let selection = state.plan_selection(&plan).expect("valid selection");

    assert_eq!(selection.selected_items.len(), 1);
    assert_eq!(
        selection.selected_items[0].target,
        SelectedTarget::Recommended
    );
    assert!(selection.selection_policy.except.is_empty());
}

#[test]
fn skip_mode_select_adds_exception() {
    let plan = plan(vec![
        update("pnpm:alpha", "alpha", ExecutionEligibility::NativeOrExact),
        update("pnpm:beta", "beta", ExecutionEligibility::NativeOrExact),
    ]);
    let mut state = state(&plan, UpdateSelectionPolicy::skip_all());

    state.unpin(&plan_item_id("pnpm:alpha")).expect("known row");
    let selection = state.plan_selection(&plan).expect("valid selection");

    assert_eq!(selection.selected_items.len(), 1);
    assert_eq!(
        selection.selected_items[0].plan_item_id,
        plan_item_id("pnpm:alpha")
    );
    assert_eq!(selection.selection_policy.mode, UpdateSelectionMode::Skip);
    assert_eq!(
        selection.selection_policy.except,
        [package("alpha")].into_iter().collect()
    );
}

#[test]
fn skip_mode_deselect_removes_exception() {
    let plan = plan(vec![update(
        "pnpm:alpha",
        "alpha",
        ExecutionEligibility::NativeOrExact,
    )]);
    let policy = UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Skip,
        except: [package("alpha")].into_iter().collect(),
    };
    let mut state = state(&plan, policy);

    state.pin(&plan_item_id("pnpm:alpha")).expect("known row");
    let selection = state.plan_selection(&plan).expect("valid selection");

    assert!(selection.selected_items.is_empty());
    assert_eq!(selection.selection_policy.mode, UpdateSelectionMode::Skip);
    assert!(selection.selection_policy.except.is_empty());
}

#[test]
fn pin_all_produces_skip_empty_policy() {
    let plan = plan(vec![update(
        "pnpm:alpha",
        "alpha",
        ExecutionEligibility::NativeOrExact,
    )]);
    let mut state = state(&plan, UpdateSelectionPolicy::default());

    state.pin_all();
    let selection = state.plan_selection(&plan).expect("valid selection");

    assert!(selection.selected_items.is_empty());
    assert_eq!(
        selection.selection_policy,
        UpdateSelectionPolicy::skip_all()
    );
}

#[test]
fn unpin_all_produces_include_empty_policy() {
    let plan = plan(vec![update(
        "pnpm:alpha",
        "alpha",
        ExecutionEligibility::NativeOrExact,
    )]);
    let mut state = state(&plan, UpdateSelectionPolicy::skip_all());

    state.unpin_all();
    let selection = state.plan_selection(&plan).expect("valid selection");

    assert_eq!(selection.selected_items.len(), 1);
    assert_eq!(
        selection.selection_policy,
        UpdateSelectionPolicy::include_all()
    );
}

#[test]
fn forced_candidate_does_not_mutate_selection_policy() {
    let plan = plan(vec![delayed(
        "pnpm:alpha",
        "alpha",
        ExecutionEligibility::ExactOnly,
    )]);
    let mut state = state(&plan, UpdateSelectionPolicy::default());

    state
        .force_candidate(&plan_item_id("pnpm:alpha"))
        .expect("force should be available");
    let selection = state.plan_selection(&plan).expect("valid selection");

    assert_eq!(selection.selected_items.len(), 1);
    assert_eq!(
        selection.selected_items[0].target,
        SelectedTarget::ForcedCandidate
    );
    assert!(selection.selection_policy.is_default());
}

#[test]
fn forced_candidate_is_unavailable_without_exact_execution() {
    let plan = plan(vec![delayed(
        "pnpm:alpha",
        "alpha",
        ExecutionEligibility::NativeOnly,
    )]);
    let mut state = state(&plan, UpdateSelectionPolicy::default());

    let err = state
        .force_candidate(&plan_item_id("pnpm:alpha"))
        .expect_err("native-only delayed item cannot be forced");

    assert_eq!(
        err.to_string(),
        "selection target is unavailable for `pnpm:alpha`"
    );
}

#[test]
fn alternate_exact_target_marks_selection_as_exact_required() {
    let plan = plan(vec![update(
        "pnpm:alpha",
        "alpha",
        ExecutionEligibility::ExactOnly,
    )]);
    let mut state = state(&plan, UpdateSelectionPolicy::default());

    state
        .choose_alternate_exact(
            &plan_item_id("pnpm:alpha"),
            VersionText::new("1.2.0").expect("valid version"),
        )
        .expect("typed target from plan is available");
    let selection = state.plan_selection(&plan).expect("valid selection");

    assert!(matches!(
        selection.selected_items[0].target,
        SelectedTarget::AlternateExact { ref target_version }
            if target_version.as_str() == "1.2.0"
    ));
}

#[test]
fn unknown_plan_item_returns_error() {
    let plan = plan(vec![update(
        "pnpm:alpha",
        "alpha",
        ExecutionEligibility::NativeOrExact,
    )]);
    let mut state = state(&plan, UpdateSelectionPolicy::default());

    let err = state
        .select_recommended(&plan_item_id("pnpm:missing"))
        .expect_err("unknown row should fail");

    assert_eq!(err.to_string(), "unknown selection row `pnpm:missing`");
}

#[test]
fn selecting_non_update_row_returns_target_unavailable() {
    let plan = plan(vec![current("pnpm:alpha", "alpha")]);
    let mut state = state(&plan, UpdateSelectionPolicy::default());

    let err = state
        .select_recommended(&plan_item_id("pnpm:alpha"))
        .expect_err("current row cannot be selected as an update");

    assert_eq!(
        err.to_string(),
        "selection target is unavailable for `pnpm:alpha`"
    );
}

#[test]
fn pinning_non_update_row_returns_target_unavailable() {
    let plan = plan(vec![current("pnpm:alpha", "alpha")]);
    let mut state = state(&plan, UpdateSelectionPolicy::default());

    let err = state
        .pin(&plan_item_id("pnpm:alpha"))
        .expect_err("current row cannot be pinned as an update");

    assert_eq!(
        err.to_string(),
        "selection target is unavailable for `pnpm:alpha`"
    );
}

#[test]
fn unavailable_alternate_exact_target_returns_target_unavailable() {
    let plan = plan(vec![update(
        "pnpm:alpha",
        "alpha",
        ExecutionEligibility::NativeOnly,
    )]);
    let mut state = state(&plan, UpdateSelectionPolicy::default());

    let err = state
        .choose_alternate_exact(
            &plan_item_id("pnpm:alpha"),
            VersionText::new("1.2.0").expect("valid version"),
        )
        .expect_err("native-only update exposes no exact target choices");

    assert_eq!(
        err.to_string(),
        "selection target is unavailable for `pnpm:alpha`"
    );
}

fn plan(items: Vec<PlanItem>) -> UpdatePlan {
    UpdatePlan::new(ManagerId::new("pnpm").expect("valid manager"), items).expect("valid plan")
}

fn state(plan: &UpdatePlan, selection_policy: UpdateSelectionPolicy) -> InteractiveSelectionState {
    let view = selection_view(plan, &selection_policy);
    InteractiveSelectionState::new(view, selection_policy)
}

fn update(id: &str, name: &str, execution_eligibility: ExecutionEligibility) -> PlanItem {
    PlanItem::Update {
        id: plan_item_id(id),
        candidate: candidate(name, execution_eligibility),
    }
}

fn delayed(id: &str, name: &str, execution_eligibility: ExecutionEligibility) -> PlanItem {
    PlanItem::Delayed {
        id: plan_item_id(id),
        candidate: candidate(name, execution_eligibility),
        reason: DelayReason::ReleaseTooFresh,
    }
}

fn current(id: &str, name: &str) -> PlanItem {
    PlanItem::Current {
        id: plan_item_id(id),
        installed: upnow_domain::InstalledTool::new(
            ManagerId::new("pnpm").expect("valid manager"),
            ToolId::new(format!("pnpm:{name}")).expect("valid tool"),
            package(name),
            upnow_domain::ToolName::new(name).expect("valid tool name"),
            VersionText::new("1.0.0").expect("valid installed version"),
            upnow_domain::ManagerMetadata::empty(),
        ),
    }
}

fn candidate(name: &str, execution_eligibility: ExecutionEligibility) -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new(format!("pnpm:{name}")).expect("valid tool"),
        package(name),
        VersionText::new("1.0.0").expect("valid installed version"),
        VersionText::new("1.2.0").expect("valid target version"),
        VersionScheme::SemVer,
        execution_eligibility,
    )
}

fn package(name: &str) -> PackageName {
    PackageName::new(name).expect("valid package")
}

fn plan_item_id(id: &str) -> PlanItemId {
    PlanItemId::new(id).expect("valid plan item id")
}
