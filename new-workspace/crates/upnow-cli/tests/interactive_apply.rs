use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use upnow_cli::config::UpnowConfig;
use upnow_cli::{
    ConfirmedInteractiveManagerApply, execute_confirmed_interactive_apply_with_config_path,
};
use upnow_domain::{
    ExecutionEligibility, ManagerId, PackageName, PlanItem, PlanItemId, PlanSelection,
    SelectedItem, ToolId, UpdateCandidate, UpdatePlan, UpdateSelectionPolicy, VersionScheme,
    VersionText,
};
use upnow_execution::progress::ExecutionProgressStatus;
use upnow_infra::{CommandOutput, Env, ProcessRunner};

#[test]
fn confirmed_interactive_apply_persists_selection_before_failed_execution() {
    let path = temp_config_path("persist-before-execution-failure");
    let config = UpnowConfig::default();
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
    assert_eq!(
        fake_calls(&process),
        ["npm -g update alpha-ready --min-release-age 7"]
    );
}

#[cfg(unix)]
#[test]
fn confirmed_interactive_apply_persists_all_selections_before_interrupted_execution() {
    let path = temp_config_path("persist-all-before-interruption");
    let config = UpnowConfig::default();
    let npm_config = config.resolve_manager("npm").expect("npm config resolves");
    let pnpm_config = config
        .resolve_manager("pnpm")
        .expect("pnpm config resolves");
    let npm_plan = update_plan("npm", "alpha-ready");
    let pnpm_plan = exact_update_plan("pnpm", "beta-ready");
    let npm_selection = selection(
        &npm_plan,
        "npm:alpha-ready",
        UpdateSelectionPolicy::skip_all(),
    );
    let pnpm_selection = selection(
        &pnpm_plan,
        "pnpm:beta-ready",
        UpdateSelectionPolicy::skip_all(),
    );
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(signal_status(), "", ""))]);
    let env = Env::fixed([]);

    let err = execute_confirmed_interactive_apply_with_config_path(
        config,
        &process,
        &env,
        vec![
            ConfirmedInteractiveManagerApply {
                plan: npm_plan,
                manager_config: npm_config,
                selection: npm_selection,
            },
            ConfirmedInteractiveManagerApply {
                plan: pnpm_plan,
                manager_config: pnpm_config,
                selection: pnpm_selection,
            },
        ],
        &path,
    )
    .expect_err("signal should interrupt interactive apply");

    assert!(err.is_interruption());
    let raw = std::fs::read_to_string(&path).expect("all selections should be persisted");
    assert!(raw.contains("[npm.selection]"));
    assert!(raw.contains("[pnpm.selection]"));
    assert_eq!(
        fake_calls(&process),
        ["npm -g update alpha-ready --min-release-age 7"]
    );
}

#[test]
fn confirmed_interactive_apply_reports_manager_command_failure_and_continues() {
    let path = temp_config_path("nonfatal-manager-command-failure");
    let config = UpnowConfig::default();
    let npm_config = config.resolve_manager("npm").expect("npm config resolves");
    let pnpm_config = config
        .resolve_manager("pnpm")
        .expect("pnpm config resolves");
    let npm_plan = update_plan("npm", "alpha-ready");
    let pnpm_plan = exact_update_plan("pnpm", "beta-ready");
    let npm_selection = selection(
        &npm_plan,
        "npm:alpha-ready",
        UpdateSelectionPolicy::default(),
    );
    let pnpm_selection = selection(
        &pnpm_plan,
        "pnpm:beta-ready",
        UpdateSelectionPolicy::default(),
    );
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            failure_status(),
            "",
            "update failed",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let env = Env::fixed([]);

    let report = execute_confirmed_interactive_apply_with_config_path(
        config,
        &process,
        &env,
        vec![
            ConfirmedInteractiveManagerApply {
                plan: npm_plan,
                manager_config: npm_config,
                selection: npm_selection,
            },
            ConfirmedInteractiveManagerApply {
                plan: pnpm_plan,
                manager_config: pnpm_config,
                selection: pnpm_selection,
            },
        ],
        &path,
    )
    .expect("ordinary manager command failure should not fail upnow");

    assert!(matches!(
        report.progress.rows[0].status,
        ExecutionProgressStatus::Failed { .. }
    ));
    assert!(matches!(
        report.progress.rows[1].status,
        ExecutionProgressStatus::Succeeded { .. }
    ));
    assert!(report.summary.had_failure);
    assert_eq!(
        fake_calls(&process),
        [
            "npm -g update alpha-ready --min-release-age 7",
            "pnpm add -g beta-ready@1.2.0"
        ]
    );
}

#[test]
fn confirmed_interactive_apply_reports_successful_execution() {
    let path = temp_config_path("successful-execution");
    let config = UpnowConfig::default();
    let manager_config = config.resolve_manager("npm").expect("npm config resolves");
    let plan = update_plan("npm", "alpha-ready");
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::recommended(plan_item_id("npm:alpha-ready"))],
        UpdateSelectionPolicy::default(),
    )
    .expect("selection should validate");
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(success_status(), "", ""))]);
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
    .expect("successful execution should complete");

    assert!(matches!(
        report.progress.rows[0].status,
        ExecutionProgressStatus::Succeeded { .. }
    ));
    assert!(!report.summary.had_failure);
}

#[cfg(unix)]
#[test]
fn confirmed_interactive_apply_maps_signal_interruption() {
    let path = temp_config_path("signal-interruption");
    let config = UpnowConfig::default();
    let manager_config = config.resolve_manager("npm").expect("npm config resolves");
    let plan = update_plan("npm", "alpha-ready");
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::recommended(plan_item_id("npm:alpha-ready"))],
        UpdateSelectionPolicy::default(),
    )
    .expect("selection should validate");
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(signal_status(), "", ""))]);
    let env = Env::fixed([]);

    let err = execute_confirmed_interactive_apply_with_config_path(
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
    .expect_err("signal should interrupt interactive apply");

    assert!(err.is_interruption());
}

fn update_plan(manager_id: &str, package_name: &str) -> UpdatePlan {
    update_plan_with_eligibility(
        manager_id,
        package_name,
        ExecutionEligibility::NativeOrExact,
    )
}

fn exact_update_plan(manager_id: &str, package_name: &str) -> UpdatePlan {
    update_plan_with_eligibility(manager_id, package_name, ExecutionEligibility::ExactOnly)
}

fn update_plan_with_eligibility(
    manager_id: &str,
    package_name: &str,
    execution_eligibility: ExecutionEligibility,
) -> UpdatePlan {
    let manager_id = ManagerId::new(manager_id).expect("valid manager id");
    let package_name_value = PackageName::new(package_name).expect("valid package name");
    let item_id = plan_item_id(&format!("{}:{}", manager_id.as_str(), package_name));
    UpdatePlan::new(
        manager_id.clone(),
        vec![PlanItem::Update {
            id: item_id,
            candidate: UpdateCandidate::new(
                ToolId::new(package_name).expect("valid tool id"),
                package_name_value.clone(),
                VersionText::new("1.0.0").expect("valid version"),
                VersionText::new("1.2.0").expect("valid version"),
                VersionScheme::SemVer,
                execution_eligibility,
            ),
        }],
    )
    .expect("valid update plan")
}

fn selection(
    plan: &UpdatePlan,
    item_id: &str,
    selection_policy: UpdateSelectionPolicy,
) -> PlanSelection {
    PlanSelection::new(
        plan,
        vec![SelectedItem::recommended(plan_item_id(item_id))],
        selection_policy,
    )
    .expect("selection should validate")
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
        .join("upnow-cli-interactive-apply-tests")
        .join(format!("{test_name}-{nanos}"))
        .join("config.toml")
}

fn fake_calls(process: &ProcessRunner) -> Vec<String> {
    match process {
        ProcessRunner::Fake(fake) => fake
            .calls()
            .iter()
            .map(upnow_infra::CommandSpec::display)
            .collect(),
        ProcessRunner::Real { .. } => panic!("expected fake process"),
    }
}

#[cfg(unix)]
fn success_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}

#[cfg(unix)]
fn failure_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(1 << 8)
}

#[cfg(unix)]
fn signal_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(2)
}

#[cfg(windows)]
fn success_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn failure_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(1)
}
