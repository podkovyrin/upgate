use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::time::Duration;
use upgate_domain::RemovalTarget;
use upgate_execution::{ExecutionAction, ResolvedRemovalItem};
use upgate_managers::{
    cargo::CargoManager, dotnet::DotnetManager, gem::GemManager, go::GoManager, mise::MiseManager,
    pnpm::PnpmManager, uv::UvManager,
};

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

#[test]
fn removals_use_native_scoped_commands_without_update_or_dependency_overrides() {
    let cases: Vec<(Box<dyn ManagerAdapter>, &str, RemovalTarget, &str)> = vec![
        (
            Box::new(BrewManager::new(config("brew"))),
            "sample",
            RemovalTarget::BrewFormula,
            "brew uninstall --formula -- sample",
        ),
        (
            Box::new(BrewManager::new(config("brew"))),
            "sample",
            RemovalTarget::BrewCask,
            "brew uninstall --cask -- sample",
        ),
        (
            Box::new(NpmManager::new(config("npm"))),
            "sample",
            RemovalTarget::Package,
            "npm uninstall -g -- sample",
        ),
        (
            Box::new(PnpmManager::new(config("pnpm"))),
            "sample",
            RemovalTarget::Package,
            "pnpm remove -g -- sample",
        ),
        (
            Box::new(BunManager::new(config("bun"))),
            "sample",
            RemovalTarget::Package,
            "bun remove -g -- sample",
        ),
        (
            Box::new(CargoManager::new(config("cargo"))),
            "sample",
            RemovalTarget::Package,
            "cargo uninstall -- sample",
        ),
        (
            Box::new(PipxManager::new(config("pipx"))),
            "sample",
            RemovalTarget::Package,
            "pipx uninstall -- sample",
        ),
        (
            Box::new(UvManager::new(config("uv"))),
            "sample",
            RemovalTarget::Package,
            "uv tool uninstall -- sample",
        ),
        (
            Box::new(DotnetManager::new(config("dotnet"))),
            "sample",
            RemovalTarget::Package,
            "dotnet tool uninstall --global -- sample",
        ),
        (
            Box::new(GemManager::new(ManagerConfig {
                version_policy: VersionPolicy::Stable,
                ..config("gem")
            })),
            "sample",
            RemovalTarget::Version,
            "gem uninstall --version 1.2.3 --executables -- sample",
        ),
        (
            Box::new(MiseManager::new(config("mise"))),
            "node",
            RemovalTarget::Version,
            "mise uninstall -- node@1.2.3",
        ),
        (
            Box::new(GoManager::new(config("go"))),
            "sample",
            RemovalTarget::Binary("/tools/bin/sample".into()),
            "rm -- /tools/bin/sample",
        ),
    ];
    for (manager, package, target, expected) in cases {
        let item = ResolvedRemovalItem {
            plan_item_id: PlanItemId::new("item").unwrap(),
            package_name: PackageName::new(package).unwrap(),
            installed_version: VersionText::new("1.2.3").unwrap(),
            target,
        };
        let commands = manager
            .commands_for_execution_plan(
                &ProcessRunner::fake([]),
                &Env::fixed([]),
                &ResolvedExecutionPlan {
                    intents: vec![ExecutionCommandIntent::Remove(item)],
                },
            )
            .unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command.to_string(), expected);
        assert_eq!(commands[0].items[0].action, ExecutionAction::Remove);
    }
}

#[test]
fn current_packages_remain_available_for_removal_in_native_outdated_managers() {
    use upgate_managers::{gem::GemManager, mise::MiseManager, pnpm::PnpmManager};
    let cases: Vec<(Box<dyn ManagerAdapter>, Vec<&str>)> = vec![
        (
            Box::new(NpmManager::new(config("npm"))),
            vec![r#"{"dependencies":{"sample":{"version":"1.2.3"}}}"#, "{}"],
        ),
        (
            Box::new(PnpmManager::new(config("pnpm"))),
            vec![r#"[{"dependencies":{"sample":{"version":"1.2.3"}}}]"#, "{}"],
        ),
        (
            Box::new(GemManager::new(ManagerConfig {
                version_policy: VersionPolicy::Stable,
                ..config("gem")
            })),
            vec!["sample (1.2.3)\ndefault-gem (default: 1.0.0)\n", ""],
        ),
        (
            Box::new(MiseManager::new(config("mise"))),
            vec![
                "",
                r#"{"sample":[{"version":"1.2.3","installed":true,"active":true}]}"#,
            ],
        ),
        (
            Box::new(BrewManager::new(ManagerConfig {
                no_update: true,
                ..config("brew")
            })),
            vec![
                r#"{"formulae":[],"casks":[]}"#,
                r#"{"formulae":[{"full_name":"sample","installed":[{"version":"1.2.3","installed_on_request":true}]}],"casks":[]}"#,
            ],
        ),
    ];
    for (manager, output) in cases {
        let process = ProcessRunner::fake(output.into_iter().map(|body| {
            Ok(CommandOutput::from_parts(
                ExitStatus::from_raw(0),
                body.as_bytes().to_vec(),
                vec![],
            ))
        }));
        let inputs = manager
            .update_inputs(&process, &HttpClient::fake([]), &Env::fixed([]), 1)
            .unwrap();
        assert_eq!(inputs.len(), 1);
        let ManagerUpdateInput::Current { installed } = &inputs[0] else {
            panic!("current inventory item expected");
        };
        assert_eq!(installed.package_name.as_str(), "sample");
        assert!(matches!(
            installed.removal,
            upgate_domain::RemovalSupport::Supported(_)
        ));
    }
}

#[test]
fn mise_rejects_upgrading_a_tool_with_a_version_marked_for_removal() {
    use upgate_domain::RemovalTarget;
    use upgate_execution::ResolvedRemovalItem;
    use upgate_managers::mise::MiseManager;
    let manager = MiseManager::new(config("mise"));
    let removal = ExecutionCommandIntent::Remove(ResolvedRemovalItem {
        plan_item_id: PlanItemId::new("mise:node@20.0.0").unwrap(),
        package_name: PackageName::new("node").unwrap(),
        installed_version: VersionText::new("20.0.0").unwrap(),
        target: RemovalTarget::Version,
    });
    let update = exact_item("mise", "node", "24.0.0");
    for intent in [
        ExecutionCommandIntent::ResolverNative(update.clone()),
        ExecutionCommandIntent::ResolverNativeGlobal(vec![update]),
    ] {
        let error = manager
            .commands_for_execution_plan(
                &ProcessRunner::fake([]),
                &Env::fixed([]),
                &ResolvedExecutionPlan {
                    intents: vec![intent, removal.clone()],
                },
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Cannot update and remove different versions of node")
        );
    }
}

#[test]
fn ambiguous_mise_upgrade_preserves_installed_versions_for_removal() {
    use upgate_managers::mise::MiseManager;
    let installed =
        r#"{"node":[{"version":"20.0.0","installed":true},{"version":"22.0.0","installed":true}]}"#;
    let process = ProcessRunner::fake(
        ["Would install node@24.0.0\n", installed, installed]
            .into_iter()
            .map(|body| {
                Ok(CommandOutput::from_parts(
                    ExitStatus::from_raw(0),
                    body.as_bytes(),
                    vec![],
                ))
            }),
    );
    let inputs = MiseManager::new(config("mise"))
        .update_inputs(&process, &HttpClient::fake([]), &Env::fixed([]), 1)
        .unwrap();
    assert_eq!(inputs.len(), 2);
    for input in inputs {
        let ManagerUpdateInput::ResolverError { installed, message } = input else {
            panic!("ambiguous update must be unavailable");
        };
        assert!(message.contains("did not identify which installed version"));
        assert!(matches!(
            installed.removal,
            upgate_domain::RemovalSupport::Supported(upgate_domain::RemovalTarget::Version)
        ));
    }
}

#[test]
fn pipx_suffixed_and_renamed_environments_cannot_remove_the_main_package() {
    let list = r#"{"venvs":{
        "black":{"metadata":{"main_package":{"package":"black","package_version":"25.1.0","suffix":"","pinned":true}}},
        "black-beta":{"metadata":{"main_package":{"package":"black","package_version":"26.0.0","suffix":"-beta"}}},
        "renamed-black":{"metadata":{"main_package":{"package":"black","package_version":"24.0.0","suffix":""}}}
    }}"#;
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(
        ExitStatus::from_raw(0),
        list,
        "",
    ))]);
    let inputs = PipxManager::new(config("pipx"))
        .update_inputs(&process, &HttpClient::fake([]), &Env::fixed([]), 1)
        .unwrap();
    assert_eq!(inputs.len(), 3);
    for input in inputs {
        let ManagerUpdateInput::Skipped { installed, .. } = input else {
            panic!("pinned and unsupported environments must not update");
        };
        assert_eq!(installed.package_name.as_str(), "black");
        match installed.tool_id.as_str() {
            "black" => assert_eq!(
                installed.removal,
                upgate_domain::RemovalSupport::Supported(RemovalTarget::Package)
            ),
            "black-beta" | "renamed-black" => {
                let upgate_domain::RemovalSupport::Unsupported(reason) = installed.removal else {
                    panic!("cannot uninstall by package name for a different environment");
                };
                assert!(reason.contains(installed.tool_id.as_str()));
            }
            unexpected => panic!("unexpected environment: {unexpected}"),
        }
    }
}
