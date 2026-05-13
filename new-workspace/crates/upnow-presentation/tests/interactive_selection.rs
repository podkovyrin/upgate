use upnow_domain::{
    DelayReason, ExecutionEligibility, ManagerId, PackageName, PlanIssue, PlanItem, PlanItemId,
    SelectedTarget, ToolId, UpdateCandidate, UpdatePlan, UpdateSelectionPolicy, VersionScheme,
    VersionText,
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
fn cancel_returns_cancel_control() {
    let mut screen = screen(&plan("pnpm", Vec::new()));

    let control = screen
        .handle_input(SelectionInput::Cancel)
        .expect("cancel should be handled");

    assert_eq!(control, SelectionControl::Cancel);
}

#[test]
fn planning_screen_starts_with_manager_tabs_and_waiting_placeholder() {
    let mut screen =
        InteractiveSelectionScreen::from_manager_ids(vec![manager_id("pnpm"), manager_id("npm")]);

    assert_eq!(screen.active_tab(), 0);
    assert_eq!(
        screen.placeholder_message().as_deref(),
        Some("Waiting to plan")
    );

    screen
        .handle_input(SelectionInput::NextTab)
        .expect("tab should move to first manager");
    assert_eq!(screen.active_tab(), 1);
    assert_eq!(
        screen.placeholder_message().as_deref(),
        Some("Waiting to plan")
    );

    screen
        .handle_input(SelectionInput::NextTab)
        .expect("tab should move to second manager");
    assert_eq!(screen.active_tab(), 2);
    assert_eq!(
        screen.placeholder_message().as_deref(),
        Some("Waiting to plan")
    );
}

#[test]
fn manager_started_changes_placeholder_to_planning() {
    let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![manager_id("pnpm")]);

    screen.apply_planning_event(InteractiveSelectionPlanningEvent::ManagerStarted {
        manager_id: manager_id("pnpm"),
    });

    assert_eq!(
        screen.placeholder_message().as_deref(),
        Some("Planning updates...")
    );
}

#[test]
fn manager_finished_with_rows_displays_selectable_rows() {
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
fn planning_indicators_do_not_affect_row_selection_or_drafts() {
    let mut screen =
        InteractiveSelectionScreen::from_manager_ids(vec![manager_id("pnpm"), manager_id("npm")]);
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
    screen.apply_planning_event(InteractiveSelectionPlanningEvent::ManagerStarted {
        manager_id: manager_id("npm"),
    });

    let selections = screen.selection_drafts();
    assert_eq!(selections.len(), 2);
    assert_eq!(selections[0].manager_id.as_str(), "pnpm");
    assert_eq!(selections[0].selected_items.len(), 1);
    assert_eq!(
        selections[0].selected_items[0].target,
        SelectedTarget::Recommended
    );
    assert_eq!(selections[1].manager_id.as_str(), "npm");
    assert!(selections[1].selected_items.is_empty());
}

#[test]
fn manager_finished_empty_stays_as_empty_placeholder() {
    let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![manager_id("pnpm")]);
    let plan = plan("pnpm", Vec::new());
    let policy = UpdateSelectionPolicy::default();

    screen.apply_planning_event(InteractiveSelectionPlanningEvent::ManagerReady {
        view: selection_view(&plan, &policy),
        issues: Vec::new(),
        selection_policy: policy,
    });
    screen.apply_planning_event(InteractiveSelectionPlanningEvent::Finished);

    assert!(screen.visible_rows().is_empty());
    assert_eq!(
        screen.placeholder_message().as_deref(),
        Some("No selectable updates")
    );
}

#[test]
fn planning_failure_finishes_with_error_placeholder() {
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
    let control = screen
        .handle_input(SelectionInput::Confirm)
        .expect("confirm after terminal planning failure should exit");
    assert_eq!(control, SelectionControl::Confirm);
}

#[test]
fn manager_error_is_visible_on_manager_tab_and_all_without_rows() {
    let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![manager_id("pnpm")]);

    screen.apply_planning_event(InteractiveSelectionPlanningEvent::ManagerError {
        manager_id: manager_id("pnpm"),
        detail: "outdated failed".to_owned(),
    });

    assert_eq!(
        screen.placeholder_message().as_deref(),
        Some("pnpm: outdated failed")
    );
    screen
        .handle_input(SelectionInput::NextTab)
        .expect("tab should move to manager");
    assert_eq!(
        screen.placeholder_message().as_deref(),
        Some("outdated failed")
    );
}

#[test]
fn confirm_before_planning_finished_does_not_exit() {
    let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![manager_id("pnpm")]);

    let control = screen
        .handle_input(SelectionInput::Confirm)
        .expect("confirm should be ignored during planning");

    assert_eq!(control, SelectionControl::Continue);
}

#[test]
fn cancel_during_planning_cancels() {
    let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![manager_id("pnpm")]);

    let control = screen
        .handle_input(SelectionInput::Cancel)
        .expect("cancel should be available during planning");

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
fn row_cursor_wraps_at_edges() {
    let mut screen = screen(&plan(
        "pnpm",
        vec![
            update("pnpm:alpha", "alpha", ExecutionEligibility::NativeOrExact),
            update("pnpm:beta", "beta", ExecutionEligibility::NativeOrExact),
        ],
    ));

    assert_eq!(screen.cursor(), 0);
    screen
        .handle_input(SelectionInput::Up)
        .expect("up should wrap to the last row");
    assert_eq!(screen.cursor(), 1);
    screen
        .handle_input(SelectionInput::Down)
        .expect("down should wrap to the first row");
    assert_eq!(screen.cursor(), 0);
}

#[test]
fn picker_movement_wraps_and_confirms_wrapped_target() {
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
    screen
        .handle_input(SelectionInput::PickerUp)
        .expect("picker up should wrap to alternate exact");
    screen
        .handle_input(SelectionInput::PickerConfirm)
        .expect("wrapped target should confirm");
    let selections = screen.selection_drafts();

    assert!(matches!(
        selections[0].selected_items[0].target,
        SelectedTarget::AlternateExact { ref target_version }
            if target_version.as_str() == "1.2.0"
    ));
}

#[test]
fn recommended_shortcut_selects_recommended_target() {
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
    screen
        .handle_input(SelectionInput::PickerDown)
        .expect("picker should move to alternate exact");
    screen
        .handle_input(SelectionInput::RecommendedTarget)
        .expect("recommended shortcut should select recommended target");
    let selections = screen.selection_drafts();

    assert_eq!(
        selections[0].selected_items[0].target,
        SelectedTarget::Recommended
    );
    assert!(!screen.target_picker_open());
}

#[test]
fn picker_can_move_between_visible_rows() {
    let mut screen = screen(&plan(
        "pnpm",
        vec![
            update("pnpm:alpha", "alpha", ExecutionEligibility::ExactOnly),
            delayed("pnpm:beta", "beta", ExecutionEligibility::ExactOnly),
        ],
    ));

    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("picker should open on first row");
    screen
        .handle_input(SelectionInput::PickerNextRow)
        .expect("picker should move to delayed row");
    screen
        .handle_input(SelectionInput::PickerConfirm)
        .expect("delayed row option should confirm");
    let selections = screen.selection_drafts();
    let beta = selections[0]
        .selected_items
        .iter()
        .find(|item| item.plan_item_id == plan_item_id("pnpm:beta"))
        .expect("beta should be selected");

    assert_eq!(screen.cursor(), 1);
    assert_eq!(beta.target, SelectedTarget::ForcedCandidate);
}

#[test]
fn picker_row_navigation_skips_visible_rows_without_target_options() {
    let mut screen = screen(&plan(
        "pnpm",
        vec![
            update("pnpm:alpha", "alpha", ExecutionEligibility::ExactOnly),
            current("pnpm:current", "current"),
            delayed("pnpm:beta", "beta", ExecutionEligibility::ExactOnly),
        ],
    ));

    screen
        .handle_input(SelectionInput::ToggleViewAll)
        .expect("view all should expose current row");
    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("picker should open on first row");
    screen
        .handle_input(SelectionInput::PickerNextRow)
        .expect("picker should skip current row");

    assert_eq!(screen.cursor(), 2);
}

#[test]
fn picker_ignores_global_confirm_without_closing() {
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
    let confirm = screen
        .handle_input(SelectionInput::Confirm)
        .expect("confirm should be ignored inside picker");

    assert_eq!(confirm, SelectionControl::Continue);
    assert!(screen.target_picker_open());
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
fn bulk_all_does_not_select_force_candidate_rows() {
    let mut screen = screen(&plan(
        "pnpm",
        vec![delayed(
            "pnpm:alpha",
            "alpha",
            ExecutionEligibility::ExactOnly,
        )],
    ));

    screen
        .handle_input(SelectionInput::SelectVisible)
        .expect("bulk select should ignore force-only rows");
    let selections = screen.selection_drafts();

    assert!(selections[0].selected_items.is_empty());
}

#[test]
fn bulk_none_deselects_updates_but_leaves_forced_candidates_unselected() {
    let mut screen = screen(&plan(
        "pnpm",
        vec![
            update("pnpm:ready", "ready", ExecutionEligibility::NativeOnly),
            delayed("pnpm:fresh", "fresh", ExecutionEligibility::ExactOnly),
        ],
    ));

    screen
        .handle_input(SelectionInput::SelectNoneVisible)
        .expect("bulk none should deselect recommended updates");
    let selections = screen.selection_drafts();

    assert!(selections[0].selected_items.is_empty());
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
    let options = screen.target_picker_options();
    assert_eq!(options.len(), 1);
    assert!(matches!(options[0], TargetOption::ForcedCandidate { .. }));

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

fn manager_id(manager: &str) -> ManagerId {
    ManagerId::new(manager).expect("valid manager")
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
