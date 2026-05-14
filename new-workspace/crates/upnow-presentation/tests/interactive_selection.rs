use std::time::Duration;

use upnow_domain::{
    CandidateEvaluationFact, ExecutionEligibility, ManagerId, PackageName, PlanDiagnostics,
    PlanIssue, PlanItem, PlanItemId, PolicyBlockReason, SelectedTarget, ToolId, UpdateCandidate,
    UpdatePlan, UpdateSelectionPolicy, VersionScheme, VersionText,
};
use upnow_presentation::tui::{
    InteractiveSelectionPlan, InteractiveSelectionPlanningEvent, InteractiveSelectionScreen,
    SelectionControl, SelectionInput,
};
use upnow_presentation::{TargetOption, selection_view};

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
fn enter_opens_details_after_planning_finished() {
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
    screen.apply_planning_event(InteractiveSelectionPlanningEvent::Finished);
    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("details should open after planning is complete");

    assert!(screen.target_picker_open());
    assert!(!screen.target_picker_options().is_empty());
}

#[test]
fn enter_does_not_open_details_for_loading_placeholder() {
    let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![manager_id("pnpm")]);

    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("placeholder input should be ignored");

    assert!(!screen.target_picker_open());
    assert_eq!(
        screen.placeholder_message().as_deref(),
        Some("Waiting to plan")
    );
}

#[test]
fn planning_events_preserve_open_details_for_existing_row() {
    let mut screen =
        InteractiveSelectionScreen::from_manager_ids(vec![manager_id("pnpm"), manager_id("npm")]);
    let pnpm_plan = plan(
        "pnpm",
        vec![update(
            "pnpm:alpha",
            "alpha",
            ExecutionEligibility::NativeOrExact,
        )],
    );
    let npm_plan = plan(
        "npm",
        vec![update(
            "npm:beta",
            "beta",
            ExecutionEligibility::NativeOrExact,
        )],
    );
    let pnpm_policy = UpdateSelectionPolicy::default();
    let npm_policy = UpdateSelectionPolicy::default();

    screen.apply_planning_event(InteractiveSelectionPlanningEvent::ManagerReady {
        view: selection_view(&pnpm_plan, &pnpm_policy),
        issues: Vec::new(),
        selection_policy: pnpm_policy,
    });
    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("details should open for the ready row");
    assert!(screen.target_picker_open());

    screen.apply_planning_event(InteractiveSelectionPlanningEvent::ManagerReady {
        view: selection_view(&npm_plan, &npm_policy),
        issues: Vec::new(),
        selection_policy: npm_policy,
    });
    assert!(screen.target_picker_open());

    screen.apply_planning_event(InteractiveSelectionPlanningEvent::Finished);
    assert!(screen.target_picker_open());
    assert_eq!(
        screen.target_picker_options()[0].target_version().as_str(),
        "1.2.0"
    );
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
        vec![PlanItem::Update {
            id: plan_item_id("pnpm:alpha"),
            candidate: candidate_with_diagnostics(
                "pnpm:alpha",
                "alpha",
                ExecutionEligibility::ExactOnly,
            ),
        }],
    ));

    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("picker should open");
    let options = screen.target_picker_options();
    assert_eq!(options.len(), 4);
    assert!(matches!(options[0], TargetOption::Recommended { .. }));
    assert!(matches!(options[1], TargetOption::AlternateExact { .. }));
    assert_eq!(options[1].target_version().as_str(), "2.0.0");
    assert!(options[1].has_violation());
    assert_eq!(options[2].target_version().as_str(), "1.3.0-beta.1");
    assert!(options[2].has_violation());
    assert_eq!(options[3].target_version().as_str(), "1.2.0");
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
            if target_version.as_str() == "2.0.0"
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

fn candidate_with_diagnostics(
    id: &str,
    name: &str,
    execution_eligibility: ExecutionEligibility,
) -> UpdateCandidate {
    candidate(id, name, execution_eligibility).with_diagnostics(PlanDiagnostics {
        required_age: Duration::from_secs(7 * 24 * 60 * 60),
        candidates: vec![
            CandidateEvaluationFact {
                version: VersionText::new("2.0.0").expect("valid version"),
                age: Some(Duration::from_secs(24 * 60 * 60)),
                policy_allowed: true,
                age_allowed: false,
                policy_block_reason: None,
                policy_warning: None,
            },
            CandidateEvaluationFact {
                version: VersionText::new("1.3.0-beta.1").expect("valid version"),
                age: Some(Duration::from_secs(30 * 24 * 60 * 60)),
                policy_allowed: false,
                age_allowed: true,
                policy_block_reason: Some(PolicyBlockReason::PreReleaseBlocked),
                policy_warning: None,
            },
            CandidateEvaluationFact {
                version: VersionText::new("1.2.0").expect("valid version"),
                age: Some(Duration::from_secs(30 * 24 * 60 * 60)),
                policy_allowed: true,
                age_allowed: true,
                policy_block_reason: None,
                policy_warning: None,
            },
        ],
        ..PlanDiagnostics::default()
    })
}

fn package(name: &str) -> PackageName {
    PackageName::new(name).expect("valid package")
}

fn plan_item_id(id: &str) -> PlanItemId {
    PlanItemId::new(id).expect("valid plan item id")
}
