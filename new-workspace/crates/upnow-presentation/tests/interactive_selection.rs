use std::time::{Duration, SystemTime};

use upnow_domain::{
    BlockReason, CandidateEvaluationFact, ExecutionSupport, InstalledTool, ManagerId,
    ManagerMetadata, ManagerSelectedTarget, PackageName, PlanDiagnostics, PlanIssue, PlanItem,
    PlanItemId, PolicyBlockReason, ReleaseEntry, ReleaseLookupResult, ReleaseTimeline,
    ReleaseTimestamp, SelectedUpdate, TargetAgeLookupResult, ToolId, ToolName, UpdateCandidate,
    UpdatePlan, UpdateSeed, UpdateSelectionPolicy, VersionScheme, VersionText,
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
            ExecutionSupport::native_or_exact(),
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
        selections[0].selected_items[0].selected_update,
        SelectedUpdate::Recommended
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
            ExecutionSupport::native_or_exact(),
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
            ExecutionSupport::native_or_exact(),
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
            ExecutionSupport::native_or_exact(),
        )],
    );
    let npm_plan = plan(
        "npm",
        vec![update(
            "npm:beta",
            "beta",
            ExecutionSupport::native_or_exact(),
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
        screen.target_picker_options()[0]
            .target_version()
            .expect("known target")
            .as_str(),
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
                ExecutionSupport::exact_only(),
            ),
        }],
    ));

    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("picker should open");
    let options = screen.target_picker_options();
    assert_eq!(options.len(), 3);
    assert!(matches!(options[0], TargetOption::AlternateExact { .. }));
    assert_eq!(
        options[0].target_version().expect("known target").as_str(),
        "2.0.0"
    );
    assert!(!options[0].has_violation());
    assert_eq!(
        options[1].target_version().expect("known target").as_str(),
        "1.3.0-beta.1"
    );
    assert!(options[1].has_violation());
    assert!(matches!(options[2], TargetOption::Recommended { .. }));
    assert_eq!(
        options[2].target_version().expect("known target").as_str(),
        "1.2.0"
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
        selections[0].selected_items[0].selected_update,
        SelectedUpdate::Exact { ref target_version }
            if target_version.as_str() == "2.0.0"
    ));
}

#[test]
fn view_all_hidden_forceable_row_opens_details_and_confirms_forced_target() {
    let mut screen = screen(&plan(
        "pnpm",
        vec![blocked_policy_item(
            "pnpm:alpha",
            "alpha",
            ExecutionSupport::exact_only(),
        )],
    ));

    assert!(screen.visible_rows().is_empty());
    screen
        .handle_input(SelectionInput::ToggleViewAll)
        .expect("view all should be handled");
    assert_eq!(screen.visible_rows().len(), 1);

    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("visible forceable row should open details");
    assert!(screen.target_picker_open());
    assert!(matches!(
        screen.target_picker_options()[0],
        TargetOption::ForcedCandidate { .. }
    ));

    screen
        .handle_input(SelectionInput::PickerConfirm)
        .expect("force target should confirm");
    let selections = screen.selection_drafts();

    assert!(!screen.target_picker_open());
    assert_eq!(
        selections[0].selected_items[0].selected_update,
        SelectedUpdate::ForcePlannedCandidate
    );
}

#[test]
fn view_all_manager_resolved_row_opens_details_and_confirms_manager_resolved() {
    let mut screen = screen(&plan(
        "uv",
        vec![PlanItem::Delayed {
            id: plan_item_id("uv:ruff"),
            candidate: manager_resolved_candidate(
                "uv:ruff",
                "ruff",
                ExecutionSupport::resolver_native(
                    upnow_domain::MinAgeConstraintSupport::Optional,
                    true,
                    false,
                ),
            ),
            reason: upnow_domain::DelayReason::ReleaseTooFresh,
        }],
    ));

    assert_eq!(screen.visible_rows().len(), 1);
    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("manager-resolved row should open details");
    assert!(screen.target_picker_open());
    assert!(screen.visible_rows()[0].target_version.is_none());
    assert!(matches!(
        screen.target_picker_options()[0],
        TargetOption::ManagerResolved { .. }
    ));

    screen
        .handle_input(SelectionInput::PickerConfirm)
        .expect("manager-resolved target should confirm");
    let selections = screen.selection_drafts();

    assert_eq!(
        selections[0].selected_items[0].selected_update,
        SelectedUpdate::ManagerResolved
    );
}

#[test]
fn missing_selected_target_metadata_row_opens_details_and_confirms_manager_resolved() {
    let mut screen = screen(&plan(
        "uv",
        vec![blocked_missing_selected_update_metadata_item(
            "uv:ruff",
            "ruff",
            ExecutionSupport::resolver_native(
                upnow_domain::MinAgeConstraintSupport::Optional,
                true,
                false,
            ),
        )],
    ));

    screen
        .handle_input(SelectionInput::ToggleViewAll)
        .expect("view all should be handled");
    assert_eq!(screen.visible_rows().len(), 1);
    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("missing selected metadata row should open details");
    assert!(matches!(
        screen.target_picker_options()[0],
        TargetOption::ManagerResolved { .. }
    ));

    screen
        .handle_input(SelectionInput::PickerConfirm)
        .expect("manager-resolved target should confirm");
    let selections = screen.selection_drafts();

    assert_eq!(
        selections[0].selected_items[0].selected_update,
        SelectedUpdate::ManagerResolved
    );
}

#[test]
fn view_all_error_rows_do_not_open_details() {
    let mut screen = screen(&plan(
        "pnpm",
        vec![PlanItem::ResolverError {
            id: plan_item_id("pnpm:alpha"),
            installed: installed_tool("pnpm", "alpha"),
            message: "resolver failed".to_owned(),
        }],
    ));

    screen
        .handle_input(SelectionInput::ToggleViewAll)
        .expect("view all should be handled");
    assert_eq!(screen.visible_rows().len(), 1);
    screen
        .handle_input(SelectionInput::OpenTargetPicker)
        .expect("error row input should be ignored");

    assert!(!screen.target_picker_open());
}

#[test]
fn picker_cancel_closes_picker_but_global_cancel_cancels_selection() {
    let mut screen = screen(&plan(
        "pnpm",
        vec![update(
            "pnpm:alpha",
            "alpha",
            ExecutionSupport::native_or_exact(),
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

fn update(id: &str, name: &str, execution_support: ExecutionSupport) -> PlanItem {
    PlanItem::Update {
        id: plan_item_id(id),
        candidate: candidate(id, name, execution_support),
    }
}

fn blocked_policy_item(id: &str, name: &str, execution_support: ExecutionSupport) -> PlanItem {
    PlanItem::Blocked {
        id: plan_item_id(id),
        seed: UpdateSeed::new(
            installed_tool("pnpm", name),
            VersionText::new("2.0.0").expect("valid target version"),
            VersionScheme::SemVer,
            release_lookup("2.0.0"),
            execution_support,
        ),
        reason: BlockReason::VersionPolicy(PolicyBlockReason::PreReleaseBlocked),
        policy_warnings: Vec::new(),
        diagnostics: PlanDiagnostics {
            required_age: Duration::from_secs(7 * 24 * 60 * 60),
            candidates: vec![CandidateEvaluationFact {
                version: VersionText::new("2.0.0").expect("valid version"),
                age: Some(Duration::from_secs(30 * 24 * 60 * 60)),
                policy_allowed: false,
                age_allowed: true,
                policy_block_reason: Some(PolicyBlockReason::PreReleaseBlocked),
                policy_warning: None,
            }],
            ..PlanDiagnostics::default()
        },
    }
}

fn blocked_missing_selected_update_metadata_item(
    id: &str,
    name: &str,
    execution_support: ExecutionSupport,
) -> PlanItem {
    PlanItem::Blocked {
        id: plan_item_id(id),
        seed: UpdateSeed::manager_selected(
            installed_tool("uv", name),
            ManagerSelectedTarget::new(
                VersionText::new("2.0.0").expect("valid target version"),
                TargetAgeLookupResult::MissingMetadata,
            ),
            VersionScheme::Pep440,
            execution_support,
        ),
        reason: BlockReason::MissingReleaseMetadata,
        policy_warnings: Vec::new(),
        diagnostics: PlanDiagnostics::new(Duration::from_secs(7 * 24 * 60 * 60))
            .with_missing_metadata(upnow_domain::MissingMetadataKind::SelectedUpdate),
    }
}

fn candidate(id: &str, name: &str, execution_support: ExecutionSupport) -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new(id).expect("valid tool"),
        package(name),
        VersionText::new("1.0.0").expect("valid installed version"),
        VersionText::new("1.2.0").expect("valid target version"),
        VersionScheme::SemVer,
        execution_support,
    )
}

fn manager_resolved_candidate(
    id: &str,
    name: &str,
    execution_support: ExecutionSupport,
) -> UpdateCandidate {
    UpdateCandidate::manager_resolved(
        ToolId::new(id).expect("valid tool"),
        package(name),
        VersionText::new("1.0.0").expect("valid installed version"),
        VersionScheme::SemVer,
        execution_support,
    )
}

fn candidate_with_diagnostics(
    id: &str,
    name: &str,
    execution_support: ExecutionSupport,
) -> UpdateCandidate {
    candidate(id, name, execution_support).with_diagnostics(PlanDiagnostics {
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

fn installed_tool(manager: &str, name: &str) -> InstalledTool {
    InstalledTool::new(
        manager_id(manager),
        ToolId::new(format!("{manager}:{name}")).expect("valid tool"),
        package(name),
        ToolName::new(name).expect("valid tool name"),
        VersionText::new("1.0.0").expect("valid installed version"),
        ManagerMetadata::empty(),
    )
}

fn release_lookup(version: &str) -> ReleaseLookupResult {
    ReleaseLookupResult::Known(ReleaseTimeline::new(vec![ReleaseEntry::new(
        VersionText::new(version).expect("valid release version"),
        ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
    )]))
}

fn plan_item_id(id: &str) -> PlanItemId {
    PlanItemId::new(id).expect("valid plan item id")
}
