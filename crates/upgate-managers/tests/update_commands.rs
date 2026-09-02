use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::time::Duration;

use upgate_domain::{
    ExecutionSupport, ExecutionTargetKind, ManagerConfig, ManagerId, ManagerMode,
    ManagerUpdateInput, PackageName, PlanItemId, UpdateSelectionPolicy, VersionPolicy, VersionText,
};
use upgate_execution::{
    ExecutionCommandIntent, ResolvedExecutionItem, ResolvedExecutionPlan, ResolvedExecutionTarget,
};
use upgate_infra::{CommandOutput, Env, HttpClient, HttpResponse, ProcessRunner};
use upgate_managers::adapter::ManagerAdapter;
use upgate_managers::brew::BrewManager;
use upgate_managers::bun::BunManager;
use upgate_managers::npm::NpmManager;
use upgate_managers::pipx::PipxManager;

fn config(manager_id: &str) -> ManagerConfig {
    ManagerConfig {
        manager_id: ManagerId::new(manager_id).expect("valid manager id"),
        mode: ManagerMode::Apply,
        min_release_age: Duration::from_secs(7 * 24 * 60 * 60),
        version_policy: VersionPolicy::None,
        no_update: false,
        selection: UpdateSelectionPolicy::default(),
    }
}

fn exact_item(manager: &str, package: &str, target: &str) -> ResolvedExecutionItem {
    ResolvedExecutionItem {
        plan_item_id: PlanItemId::new(format!("{manager}:{package}")).expect("valid plan item id"),
        package_name: PackageName::new(package).expect("valid package name"),
        installed_version: VersionText::new("1.0.0").expect("valid installed version"),
        target: ResolvedExecutionTarget::Known(
            VersionText::new(target).expect("valid target version"),
        ),
        execution_support: ExecutionSupport::exact_only(),
        execution_target_kind: ExecutionTargetKind::Standard,
        exact_target_required: false,
        bypass_min_release_age: false,
    }
}

fn native_brew_item(package: &str, kind: ExecutionTargetKind) -> ResolvedExecutionItem {
    ResolvedExecutionItem {
        execution_support: ExecutionSupport::native_with_age_bypass_only(),
        execution_target_kind: kind,
        ..exact_item("brew", package, "2.0.0")
    }
}

#[test]
fn brew_runs_each_selected_item_in_its_own_scoped_command() {
    let manager = BrewManager::new(config("brew"));
    let plan = ResolvedExecutionPlan {
        intents: vec![
            ExecutionCommandIntent::NativeSelected(native_brew_item(
                "btop",
                ExecutionTargetKind::BrewFormula,
            )),
            ExecutionCommandIntent::NativeSelected(native_brew_item(
                "docker",
                ExecutionTargetKind::BrewCask,
            )),
        ],
    };

    let commands = manager
        .commands_for_execution_plan(&ProcessRunner::fake([]), &Env::fixed([]), &plan)
        .expect("scoped Brew commands should be supported");

    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0].command.to_string(),
        "brew upgrade --formula btop"
    );
    assert_eq!(
        commands[1].command.to_string(),
        "brew upgrade --cask docker"
    );
    assert!(commands.iter().all(|command| command.items.len() == 1));
}

#[test]
fn npm_installs_each_selected_exact_target_with_an_age_cutoff() {
    let manager = NpmManager::new(config("npm"));
    let plan = ResolvedExecutionPlan {
        intents: vec![ExecutionCommandIntent::Exact(exact_item(
            "npm", "npm", "11.6.0",
        ))],
    };

    let commands = manager
        .commands_for_execution_plan(&ProcessRunner::fake([]), &Env::fixed([]), &plan)
        .expect("exact npm command should be supported");
    let display = commands[0].command.to_string();

    assert!(display.starts_with("npm install -g npm@11.6.0 --before="));
    assert!(!display.contains("min-release-age"));
    assert_eq!(commands[0].items.len(), 1);
}

#[test]
fn pipx_uses_install_upgrade_for_the_selected_exact_target() {
    let manager = PipxManager::new(config("pipx"));
    let plan = ResolvedExecutionPlan {
        intents: vec![ExecutionCommandIntent::Exact(exact_item(
            "pipx",
            "pymobiledevice3",
            "10.11.5",
        ))],
    };

    let commands = manager
        .commands_for_execution_plan(&ProcessRunner::fake([]), &Env::fixed([]), &plan)
        .expect("exact pipx command should be supported");

    assert_eq!(
        commands[0].command.to_string(),
        "pipx install --upgrade --skip-maintenance pymobiledevice3==10.11.5"
    );
}

#[test]
fn pipx_upload_cutoff_limits_the_versions_visible_to_planning() {
    let list = r#"{
        "venvs": {
            "pymobiledevice3": {
                "metadata": {
                    "main_package": {
                        "package": "pymobiledevice3",
                        "package_version": "9.7.3",
                        "package_or_url": "pymobiledevice3",
                        "pip_args": ["--uploaded-prior-to=2026-03-24T15:31:25Z"],
                        "pinned": false,
                        "lock_file": null,
                        "suffix": ""
                    }
                }
            }
        }
    }"#;
    let releases = r#"{
        "releases": {
            "10.10.0": [{"upload_time_iso_8601": "2026-03-20T00:00:00Z"}],
            "10.11.5": [{"upload_time_iso_8601": "2026-04-01T00:00:00Z"}]
        }
    }"#;
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(
        ExitStatus::from_raw(0),
        list,
        "",
    ))]);
    let http = HttpClient::fake([(
        "https://pypi.org/pypi/pymobiledevice3/json".to_owned(),
        HttpResponse {
            status: 200,
            body: releases.to_owned(),
        },
    )]);

    let inputs = PipxManager::new(config("pipx"))
        .update_inputs(&process, &http, &Env::fixed([]), 1)
        .expect("pipx metadata should produce a planning input");

    let ManagerUpdateInput::Seed(seed) = &inputs[0] else {
        panic!("expected an update seed");
    };
    assert_eq!(
        seed.target_selection
            .target_version()
            .expect("known target")
            .as_str(),
        "10.10.0"
    );
}

#[test]
fn bun_missing_global_lockfile_is_an_empty_installation() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(
        ExitStatus::from_raw(256),
        "",
        "error: missing lockfile, nothing to list\nnote: run 'bun install' first",
    ))]);

    let inputs = BunManager::new(config("bun"))
        .scan_inputs(&process, &Env::fixed([]))
        .expect("an uninitialized Bun global directory should not be an error");

    assert!(inputs.is_empty());
}
