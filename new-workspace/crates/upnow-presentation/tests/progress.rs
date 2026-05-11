use upnow_domain::{ManagerId, PackageName, PlanItemId, VersionText};
use upnow_execution::progress::{
    ExecutionProgressRow, ExecutionProgressState, ExecutionProgressStatus,
};
use upnow_presentation::tui::render_progress_state;

#[test]
fn progress_renderer_shows_pending_done_failed_and_summary() {
    let state = ExecutionProgressState {
        rows: vec![
            row(
                "pnpm",
                "pnpm:alpha",
                "alpha",
                ExecutionProgressStatus::Pending,
            ),
            row(
                "npm",
                "npm:beta",
                "beta",
                ExecutionProgressStatus::Succeeded {
                    command: "npm update -g beta".to_owned(),
                    skipped_mutation: true,
                },
            ),
            row(
                "brew",
                "brew:gamma",
                "gamma",
                ExecutionProgressStatus::Failed {
                    detail: "brew upgrade gamma: failed".to_owned(),
                },
            ),
        ],
        manager_failures: Vec::new(),
        finished: true,
        stop_after_current: false,
    };

    let output = render_progress_state(&state);

    assert!(output.contains("interactive apply progress"));
    assert!(output.contains("pnpm alpha 1.0.0 -> 1.2.0 pending"));
    assert!(output.contains("npm beta 1.0.0 -> 1.2.0 done skipped (npm update -g beta)"));
    assert!(output.contains("brew gamma 1.0.0 -> 1.2.0 failed: brew upgrade gamma: failed"));
    assert!(output.contains("summary failed"));
}

fn row(
    manager_id: &str,
    plan_item_id: &str,
    package_name: &str,
    status: ExecutionProgressStatus,
) -> ExecutionProgressRow {
    ExecutionProgressRow {
        manager_id: ManagerId::new(manager_id).expect("valid manager id"),
        plan_item_id: PlanItemId::new(plan_item_id).expect("valid plan item id"),
        package_name: PackageName::new(package_name).expect("valid package name"),
        installed_version: VersionText::new("1.0.0").expect("valid version"),
        target_version: VersionText::new("1.2.0").expect("valid version"),
        status,
    }
}
