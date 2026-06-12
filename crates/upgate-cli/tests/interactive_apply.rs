use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use upgate_cli::config::ConfigFile;
use upgate_cli::{
    ConfirmedInteractiveManagerApply, execute_confirmed_interactive_apply_with_config_path,
};
use upgate_domain::{
    ExecutionSupport, ManagerId, PackageName, PlanItem, PlanItemId, PlanSelection, SelectedItem,
    ToolId, UpdateCandidate, UpdatePlan, UpdateSelectionPolicy, VersionScheme, VersionText,
};
use upgate_execution::progress::ExecutionProgressStatus;
use upgate_infra::{CommandOutput, Env, ProcessRunner};

#[test]
fn confirmed_interactive_apply_persists_selection_before_failed_execution() {
    let path = temp_config_path("persist-before-execution-failure");
    let config = ConfigFile::default();
    let manager_config = config.resolve_manager("npm").expect("npm config resolves");
    let plan = update_plan("npm", "alpha-ready");
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::recommended(plan_item_id("npm:alpha-ready"))],
        UpdateSelectionPolicy::skip_all(),
    )
    .expect("selection should validate");
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(
        failure_status(),
        "",
        "update failed",
    ))]);
    let env = Env::fixed([]);

    let report = execute_confirmed_interactive_apply_with_config_path(
        config,
        &process,
        &env,
        vec![ConfirmedInteractiveManagerApply {
            plan,
            manager_config,
            selection,
        }],
        &path,
    )
    .expect("ordinary execution failure should be reported, not returned");

    let raw = std::fs::read_to_string(&path).expect("selection policy should be persisted");
    assert!(raw.contains("[npm.selection]"));
    assert!(raw.contains("mode = \"skip\""));
    assert!(matches!(
        report.progress.rows[0].status,
        ExecutionProgressStatus::Failed { .. }
    ));
    assert!(report.summary.had_failure);
}

fn update_plan(manager_id: &str, package_name: &str) -> UpdatePlan {
    let manager_id = ManagerId::new(manager_id).expect("valid manager id");
    let package_name_value = PackageName::new(package_name).expect("valid package name");
    let item_id = plan_item_id(&format!("{manager_id}:{package_name}"));
    UpdatePlan::new(
        manager_id,
        vec![PlanItem::Update {
            id: item_id,
            candidate: UpdateCandidate::new(
                ToolId::new(package_name).expect("valid tool id"),
                package_name_value,
                VersionText::new("1.0.0").expect("valid version"),
                VersionText::new("1.2.0").expect("valid version"),
                VersionScheme::SemVer,
                ExecutionSupport::native_or_exact(),
            ),
        }],
    )
    .expect("valid update plan")
}

fn plan_item_id(value: &str) -> PlanItemId {
    PlanItemId::new(value).expect("valid plan item id")
}

fn temp_config_path(test_name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after UNIX_EPOCH")
        .as_nanos();
    std::env::temp_dir()
        .join("upgate-cli-interactive-apply-tests")
        .join(format!("{test_name}-{nanos}"))
        .join("config.toml")
}

#[cfg(unix)]
fn failure_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(1 << 8)
}

#[cfg(windows)]
fn failure_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(1)
}
