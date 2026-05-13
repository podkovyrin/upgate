use upnow_domain::{
    ExecutionEligibility, ManagerId, PackageName, PlanIssue, PlanItem, PlanItemId, SelectedTarget,
    ToolId, UpdateCandidate, UpdatePlan, UpdateSelectionPolicy, VersionScheme, VersionText,
};
use upnow_planning::{TargetOption, selection_view};
use upnow_presentation::tui::{
    InteractiveSelectionPlan, InteractiveSelectionPlanningEvent, InteractiveSelectionScreen,
    SelectionControl, SelectionInput,
};

#[test]
fn confirm_returns_typed_selection() {
    let mut screen = screen(&plan(
        "pnpm",
        vec![update(
            "pnpm:alpha",
            "alpha",
            ExecutionEligibility::NativeOrExact,
        )],
    ));

    let control = screen
        .handle_input(SelectionInput::Confirm)
        .expect("confirm should be handled");
    let selections = screen.selection_drafts();

    assert_eq!(control, SelectionControl::Confirm);
    assert_eq!(selections[0].manager_id.as_str(), "pnpm");
    assert_eq!(selections[0].selected_items.len(), 1);
    assert_eq!(
        selections[0].selected_items[0].target,
        SelectedTarget::Recommended
    );
}

#[test]
fn planning_requires_finish_before_confirm_exits() {
    let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![manager_id("pnpm")]);

    let before = screen
        .handle_input(SelectionInput::Confirm)
        .expect("confirm should be ignored during planning");
    assert_eq!(before, SelectionControl::Continue);

    screen.apply_planning_event(InteractiveSelectionPlanningEvent::Finished);
    let after = screen
        .handle_input(SelectionInput::Confirm)
        .expect("confirm should exit after planning finished");
    assert_eq!(after, SelectionControl::Confirm);
}

#[test]
fn manager_ready_with_rows_clears_placeholder_and_shows_rows() {
    let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![manager_id("pnpm")]);
    let plan = plan(
        "pnpm",
        vec![update(
            "pnpm:alpha",
            "alpha",
            ExecutionEligibility::NativeOrExact,
        )],
    );
    let policy = UpdateSelectionPolicy::default();

    screen.apply_planning_event(InteractiveSelectionPlanningEvent::ManagerReady {
        view: selection_view(&plan, &policy),
        issues: Vec::new(),
        selection_policy: policy,
    });

    assert_eq!(screen.placeholder_message(), None);
    assert_eq!(screen.visible_rows().len(), 1);
}

#[test]
fn planning_failure_shows_detail_and_confirm_is_fatal() {
    let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![manager_id("pnpm")]);

    screen.apply_planning_event(InteractiveSelectionPlanningEvent::ManagerStarted {
        manager_id: manager_id("pnpm"),
    });
    screen.apply_planning_event(InteractiveSelectionPlanningEvent::PlanningFailed {
        detail: "planning worker stopped".to_owned(),
    });

    assert_eq!(
        screen.placeholder_message().as_deref(),
        Some("planning worker stopped")
    );
    let err = screen
        .handle_input(SelectionInput::Confirm)
        .expect_err("confirm after planning failure should fail");
    assert_eq!(err.to_string(), "planning worker stopped");
}

#[test]
fn manager_planning_error_confirm_is_fatal() {
    let mut screen =
        InteractiveSelectionScreen::from_manager_ids(vec![manager_id("pnpm"), manager_id("npm")]);

    screen.apply_planning_event(InteractiveSelectionPlanningEvent::ManagerError {
        manager_id: manager_id("pnpm"),
        detail: "outdated failed".to_owned(),
    });
    screen.apply_planning_event(InteractiveSelectionPlanningEvent::Finished);

    let err = screen
        .handle_input(SelectionInput::Confirm)
        .expect_err("confirm with a manager planning error should fail");
    assert_eq!(err.to_string(), "pnpm: outdated failed");
}

#[test]
fn picker_can_choose_alternate_exact_target() {
    let mut screen = screen(&plan(
        "pnpm",
        vec![update(
            "pnpm:alpha",
            "alpha",
            ExecutionEligibility::ExactOnly,
        )],
    ));

    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("picker should open");
    let options = screen.target_picker_options();
    assert_eq!(options.len(), 2);
    assert!(matches!(options[0], TargetOption::Recommended { .. }));
    assert!(matches!(options[1], TargetOption::AlternateExact { .. }));
    assert_eq!(options[1].target_version().as_str(), "1.2.0");
    screen
        .handle_input(SelectionInput::PickerDown)
        .expect("picker should move");
    screen
        .handle_input(SelectionInput::PickerConfirm)
        .expect("picker should confirm exact target");
    let selections = screen.selection_drafts();

    assert!(!screen.target_picker_open());
    assert!(matches!(
        selections[0].selected_items[0].target,
        SelectedTarget::AlternateExact { ref target_version }
            if target_version.as_str() == "1.2.0"
    ));
}

#[test]
fn picker_cancel_closes_picker_but_global_cancel_cancels_selection() {
    let mut screen = screen(&plan(
        "pnpm",
        vec![update(
            "pnpm:alpha",
            "alpha",
            ExecutionEligibility::NativeOrExact,
        )],
    ));

    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("picker should open");
    let control = screen
        .handle_input(SelectionInput::PickerCancel)
        .expect("picker cancel should be handled");

    assert_eq!(control, SelectionControl::Continue);
    assert!(!screen.target_picker_open());

    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("picker should reopen");
    let control = screen
        .handle_input(SelectionInput::Cancel)
        .expect("global cancel should be handled");

    assert_eq!(control, SelectionControl::Cancel);
    assert!(screen.target_picker_open());
}

#[test]
fn planning_issue_is_exposed_when_there_are_no_rows() {
    let plan = UpdatePlan::with_issues(
        ManagerId::new("pnpm").expect("valid manager"),
        Vec::new(),
        vec![PlanIssue::DiscoveryFailed {
            detail: "outdated failed".to_owned(),
        }],
    )
    .expect("valid plan");
    let screen = screen(&plan);

    assert_eq!(
        screen.placeholder_message().as_deref(),
        Some("pnpm: outdated failed")
    );
}

fn screen(plan: &UpdatePlan) -> InteractiveSelectionScreen {
    InteractiveSelectionScreen::new(vec![selection_plan(plan)])
}

fn selection_plan(plan: &UpdatePlan) -> InteractiveSelectionPlan {
    let policy = UpdateSelectionPolicy::default();
    let view = selection_view(plan, &policy);
    let issues = plan.issues.clone();
    InteractiveSelectionPlan::new(view, issues, policy)
}

fn plan(manager: &str, items: Vec<PlanItem>) -> UpdatePlan {
    UpdatePlan::new(ManagerId::new(manager).expect("valid manager"), items).expect("valid plan")
}

fn manager_id(manager: &str) -> ManagerId {
    ManagerId::new(manager).expect("valid manager")
}

fn update(id: &str, name: &str, execution_eligibility: ExecutionEligibility) -> PlanItem {
    PlanItem::Update {
        id: plan_item_id(id),
        candidate: candidate(id, name, execution_eligibility),
    }
}

fn candidate(id: &str, name: &str, execution_eligibility: ExecutionEligibility) -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new(id).expect("valid tool"),
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
