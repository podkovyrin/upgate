use upnow_domain::{
    DelayReason, ExecutionEligibility, ManagerId, PackageName, PlanIssue, PlanItem, PlanItemId,
    SelectedTarget, ToolId, UpdateCandidate, UpdatePlan, UpdateSelectionPolicy, VersionScheme,
    VersionText,
};
use upnow_planning::selection_view;
use upnow_presentation::tui::{
    InteractiveSelectionPlan, InteractiveSelectionScreen, SelectionControl, SelectionInput,
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
fn cancel_returns_cancel_control() {
    let mut screen = screen(&plan("pnpm", Vec::new()));

    let control = screen
        .handle_input(SelectionInput::Cancel)
        .expect("cancel should be handled");

    assert_eq!(control, SelectionControl::Cancel);
}

#[test]
fn toggle_current_deselects_update_without_mutating_plan() {
    let plan = plan(
        "pnpm",
        vec![update(
            "pnpm:alpha",
            "alpha",
            ExecutionEligibility::NativeOrExact,
        )],
    );
    let mut screen = screen(&plan);

    screen
        .handle_input(SelectionInput::ToggleCurrent)
        .expect("toggle should update selection");
    let selections = screen.selection_drafts();

    assert!(selections[0].selected_items.is_empty());
    assert_eq!(plan.items.len(), 1);
}

#[test]
fn tabs_and_visible_bulk_selection_apply_to_current_view() {
    let mut screen = InteractiveSelectionScreen::new(vec![
        selection_plan(&plan(
            "pnpm",
            vec![update(
                "pnpm:alpha",
                "alpha",
                ExecutionEligibility::NativeOrExact,
            )],
        )),
        selection_plan(&plan(
            "npm",
            vec![update(
                "npm:beta",
                "beta",
                ExecutionEligibility::NativeOrExact,
            )],
        )),
    ]);

    screen
        .handle_input(SelectionInput::NextTab)
        .expect("tab should move to first manager");
    screen
        .handle_input(SelectionInput::SelectNoneVisible)
        .expect("visible rows should deselect");
    let selections = screen.selection_drafts();

    assert_eq!(screen.active_tab(), 1);
    assert!(selections[0].selected_items.is_empty());
    assert_eq!(selections[1].selected_items.len(), 1);
}

#[test]
fn no_selectable_updates_are_hidden_until_view_all() {
    let mut screen = screen(&plan("pnpm", vec![current("pnpm:alpha", "alpha")]));

    assert!(screen.visible_rows().is_empty());
    assert_eq!(
        screen.placeholder_message().as_deref(),
        Some("No selectable updates")
    );

    screen
        .handle_input(SelectionInput::ToggleViewAll)
        .expect("view all should toggle");

    assert!(screen.show_all());
    assert_eq!(screen.visible_rows().len(), 1);
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
    assert_eq!(
        screen.target_picker_options(),
        ["recommended", "exact 1.2.0"]
    );
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
fn delayed_forced_candidate_is_visible_and_selectable_by_toggle() {
    let mut screen = screen(&plan(
        "pnpm",
        vec![delayed(
            "pnpm:alpha",
            "alpha",
            ExecutionEligibility::ExactOnly,
        )],
    ));

    assert_eq!(screen.visible_rows().len(), 1);

    screen
        .handle_input(SelectionInput::ToggleCurrent)
        .expect("forced delayed row should toggle");
    let selections = screen.selection_drafts();

    assert_eq!(
        selections[0].selected_items[0].target,
        SelectedTarget::ForcedCandidate
    );
}

#[test]
fn delayed_picker_shows_only_real_actions_and_forces_on_first_option() {
    let mut screen = screen(&plan(
        "pnpm",
        vec![delayed(
            "pnpm:alpha",
            "alpha",
            ExecutionEligibility::ExactOnly,
        )],
    ));

    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("picker should open");
    assert_eq!(
        screen.target_picker_options(),
        ["force candidate", "exact 1.2.0"]
    );

    screen
        .handle_input(SelectionInput::PickerConfirm)
        .expect("first delayed picker option should force");
    let selections = screen.selection_drafts();

    assert_eq!(
        selections[0].selected_items[0].target,
        SelectedTarget::ForcedCandidate
    );
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

fn update(id: &str, name: &str, execution_eligibility: ExecutionEligibility) -> PlanItem {
    PlanItem::Update {
        id: plan_item_id(id),
        candidate: candidate(id, name, execution_eligibility),
    }
}

fn delayed(id: &str, name: &str, execution_eligibility: ExecutionEligibility) -> PlanItem {
    PlanItem::Delayed {
        id: plan_item_id(id),
        candidate: candidate(id, name, execution_eligibility),
        reason: DelayReason::ReleaseTooFresh,
    }
}

fn current(id: &str, name: &str) -> PlanItem {
    PlanItem::Current {
        id: plan_item_id(id),
        installed: upnow_domain::InstalledTool::new(
            ManagerId::new("pnpm").expect("valid manager"),
            ToolId::new(id).expect("valid tool"),
            package(name),
            upnow_domain::ToolName::new(name).expect("valid tool name"),
            VersionText::new("1.0.0").expect("valid installed version"),
            upnow_domain::ManagerMetadata::empty(),
        ),
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
