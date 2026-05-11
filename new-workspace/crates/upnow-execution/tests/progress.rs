use upnow_domain::{ExecutionEligibility, ManagerId, PackageName, PlanItemId, VersionText};
use upnow_execution::progress::{
    ExecutionProgressEvent, ExecutionProgressState, ExecutionProgressStatus,
};
use upnow_execution::{
    ExecutionCommandIntent, ExecutionItemResult, ExecutionReport, ExecutionStatus,
    ResolvedExecutionItem, ResolvedExecutionPlan,
};

#[test]
fn progress_tracks_grouped_execution_for_each_item() {
    let manager_id = manager_id("brew");
    let mut state = ExecutionProgressState::from_execution_plans(vec![(
        manager_id.clone(),
        ResolvedExecutionPlan {
            intents: vec![ExecutionCommandIntent::NativeGlobal(vec![
                item("brew:alpha", "alpha"),
                item("brew:beta", "beta"),
            ])],
        },
    )]);

    state.apply_event(ExecutionProgressEvent::manager_started(manager_id.clone()));
    assert!(
        state
            .rows
            .iter()
            .all(|row| { matches!(row.status, ExecutionProgressStatus::Running) })
    );

    state.apply_event(ExecutionProgressEvent::manager_finished(ExecutionReport {
        manager_id,
        items: vec![
            result(
                "brew:alpha",
                "alpha",
                ExecutionStatus::Succeeded {
                    command: "brew upgrade --formula alpha beta".to_owned(),
                    skipped_mutation: false,
                },
            ),
            result(
                "brew:beta",
                "beta",
                ExecutionStatus::Succeeded {
                    command: "brew upgrade --formula alpha beta".to_owned(),
                    skipped_mutation: false,
                },
            ),
        ],
    }));

    assert_eq!(state.rows.len(), 2);
    assert!(
        state
            .rows
            .iter()
            .all(|row| { matches!(row.status, ExecutionProgressStatus::Succeeded { .. }) })
    );
    assert!(!state.summary().had_failure);
}

#[test]
fn progress_attaches_manager_failure_to_selected_rows() {
    let manager_id = manager_id("pnpm");
    let mut state = ExecutionProgressState::from_execution_plans(vec![(
        manager_id.clone(),
        ResolvedExecutionPlan {
            intents: vec![ExecutionCommandIntent::Exact(item("pnpm:alpha", "alpha"))],
        },
    )]);

    state.apply_event(ExecutionProgressEvent::manager_started(manager_id.clone()));
    state.apply_event(ExecutionProgressEvent::manager_failed(
        manager_id,
        "command construction failed",
    ));

    assert!(matches!(
        state.rows[0].status,
        ExecutionProgressStatus::Failed { ref detail }
            if detail == "command construction failed"
    ));
    assert_eq!(state.manager_failures.len(), 1);
    assert!(state.summary().had_failure);
}

#[test]
fn stop_after_current_marks_pending_rows_skipped_on_finish() {
    let mut state = ExecutionProgressState::from_execution_plans(vec![
        (
            manager_id("pnpm"),
            ResolvedExecutionPlan {
                intents: vec![ExecutionCommandIntent::Exact(item("pnpm:alpha", "alpha"))],
            },
        ),
        (
            manager_id("npm"),
            ResolvedExecutionPlan {
                intents: vec![ExecutionCommandIntent::Exact(item("npm:beta", "beta"))],
            },
        ),
    ]);

    state.apply_event(ExecutionProgressEvent::manager_started(manager_id("pnpm")));
    state.apply_event(ExecutionProgressEvent::StopAfterCurrentRequested);
    state.apply_event(ExecutionProgressEvent::manager_finished(ExecutionReport {
        manager_id: manager_id("pnpm"),
        items: vec![result(
            "pnpm:alpha",
            "alpha",
            ExecutionStatus::Succeeded {
                command: "pnpm add -g alpha@1.2.0".to_owned(),
                skipped_mutation: false,
            },
        )],
    }));
    state.apply_event(ExecutionProgressEvent::Finished);

    assert!(matches!(
        state.rows[0].status,
        ExecutionProgressStatus::Succeeded { .. }
    ));
    assert!(matches!(
        state.rows[1].status,
        ExecutionProgressStatus::Skipped { ref detail }
            if detail == "stopped after current manager"
    ));
    assert!(state.summary().stopped_after_current);
}

fn item(id: &str, package_name: &str) -> ResolvedExecutionItem {
    ResolvedExecutionItem {
        plan_item_id: PlanItemId::new(id).expect("valid plan item id"),
        package_name: PackageName::new(package_name).expect("valid package name"),
        installed_version: VersionText::new("1.0.0").expect("valid version"),
        target_version: VersionText::new("1.2.0").expect("valid version"),
        execution_eligibility: ExecutionEligibility::ExactOnly,
        execution_target_kind: upnow_domain::ExecutionTargetKind::Standard,
        exact_target_required: true,
        bypass_min_release_age: false,
    }
}

fn result(id: &str, package_name: &str, status: ExecutionStatus) -> ExecutionItemResult {
    ExecutionItemResult {
        plan_item_id: PlanItemId::new(id).expect("valid plan item id"),
        package_name: PackageName::new(package_name).expect("valid package name"),
        installed_version: VersionText::new("1.0.0").expect("valid version"),
        target_version: VersionText::new("1.2.0").expect("valid version"),
        status,
    }
}

fn manager_id(value: &str) -> ManagerId {
    ManagerId::new(value).expect("valid manager id")
}
