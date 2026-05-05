use upnow_domain::{
    ExecutionEligibility, ManagerId, PackageName, PlanItem, PlanItemId, PlanSelection,
    SelectedItem, ToolId, UpdateCandidate, UpdatePlan, VersionPolicy, VersionScheme, VersionText,
};
use upnow_execution::{
    ExecutionCapabilities, ExecutionSelectionError, resolve_selection_for_execution,
};
use upnow_managers::adapter::{CommandBuildSettings, ManagerAdapterError};
use upnow_managers::registry::{available_managers, manager_by_id};

#[test]
fn registry_selects_managers_by_id() {
    let pnpm = manager_by_id(&ManagerId::new("pnpm").expect("valid id"))
        .expect("pnpm should be registered");
    let npm =
        manager_by_id(&ManagerId::new("npm").expect("valid id")).expect("npm should be registered");
    let yarn = manager_by_id(&ManagerId::new("yarn").expect("valid id"))
        .expect("yarn should be registered");
    let bun =
        manager_by_id(&ManagerId::new("bun").expect("valid id")).expect("bun should be registered");
    let cargo = manager_by_id(&ManagerId::new("cargo").expect("valid id"))
        .expect("cargo should be registered");
    let pipx = manager_by_id(&ManagerId::new("pipx").expect("valid id"))
        .expect("pipx should be registered");
    let go =
        manager_by_id(&ManagerId::new("go").expect("valid id")).expect("go should be registered");
    let gem =
        manager_by_id(&ManagerId::new("gem").expect("valid id")).expect("gem should be registered");
    let dotnet = manager_by_id(&ManagerId::new("dotnet").expect("valid id"))
        .expect("dotnet should be registered");

    assert_eq!(pnpm.id(), "pnpm");
    assert_eq!(npm.id(), "npm");
    assert_eq!(yarn.id(), "yarn");
    assert_eq!(bun.id(), "bun");
    assert_eq!(cargo.id(), "cargo");
    assert_eq!(pipx.id(), "pipx");
    assert_eq!(go.id(), "go");
    assert_eq!(gem.id(), "gem");
    assert_eq!(dotnet.id(), "dotnet");
}

#[test]
fn registry_reports_unknown_manager() {
    let err = match manager_by_id(&ManagerId::new("unknown").expect("valid id")) {
        Ok(manager) => panic!("unexpected manager: {}", manager.id()),
        Err(err) => err,
    };

    assert_eq!(
        err,
        ManagerAdapterError::UnknownManager("unknown".to_owned())
    );
}

#[test]
fn registered_managers_validate_supported_policies() {
    for manager in available_managers() {
        for policy in [
            VersionPolicy::None,
            VersionPolicy::Stable,
            VersionPolicy::SameTrack,
        ] {
            manager
                .validate_version_policy(policy)
                .unwrap_or_else(|err| {
                    assert!(
                        manager.id() == "gem" && policy == VersionPolicy::SameTrack,
                        "unexpected policy validation failure: {err}"
                    );
                });
        }
    }
}

#[test]
fn capabilities_are_typed_per_manager() {
    let pnpm = manager_by_id(&ManagerId::new("pnpm").expect("valid id"))
        .expect("pnpm should be registered");
    let npm =
        manager_by_id(&ManagerId::new("npm").expect("valid id")).expect("npm should be registered");
    let yarn = manager_by_id(&ManagerId::new("yarn").expect("valid id"))
        .expect("yarn should be registered");
    let bun =
        manager_by_id(&ManagerId::new("bun").expect("valid id")).expect("bun should be registered");
    let cargo = manager_by_id(&ManagerId::new("cargo").expect("valid id"))
        .expect("cargo should be registered");
    let pipx = manager_by_id(&ManagerId::new("pipx").expect("valid id"))
        .expect("pipx should be registered");
    let go =
        manager_by_id(&ManagerId::new("go").expect("valid id")).expect("go should be registered");
    let gem =
        manager_by_id(&ManagerId::new("gem").expect("valid id")).expect("gem should be registered");
    let dotnet = manager_by_id(&ManagerId::new("dotnet").expect("valid id"))
        .expect("dotnet should be registered");

    assert!(pnpm.capabilities().exact_target);
    assert!(!pnpm.capabilities().native_update);
    assert!(npm.capabilities().exact_target);
    assert!(npm.capabilities().native_update);
    assert!(yarn.capabilities().exact_target);
    assert!(!yarn.capabilities().native_update);
    assert!(bun.capabilities().exact_target);
    assert!(!bun.capabilities().native_update);
    assert!(bun.capabilities().native_global_update);
    assert!(cargo.capabilities().exact_target);
    assert!(!cargo.capabilities().native_update);
    assert!(pipx.capabilities().exact_target);
    assert!(!pipx.capabilities().native_update);
    assert!(go.capabilities().exact_target);
    assert!(!go.capabilities().native_update);
    assert!(gem.capabilities().exact_target);
    assert!(!gem.capabilities().native_update);
    assert!(dotnet.capabilities().exact_target);
    assert!(!dotnet.capabilities().native_update);
}

#[test]
fn pnpm_builds_commands_through_adapter_boundary() {
    let manager = manager_by_id(&ManagerId::new("pnpm").expect("valid id"))
        .expect("pnpm should be registered");
    let plan = UpdatePlan::new(
        ManagerId::new("pnpm").expect("valid manager"),
        vec![PlanItem::Update {
            id: PlanItemId::new("pnpm:alpha-ready").expect("valid id"),
            candidate: UpdateCandidate::new(
                ToolId::new("alpha-ready").expect("valid tool id"),
                PackageName::new("alpha-ready").expect("valid package"),
                VersionText::new("1.0.0").expect("valid version"),
                VersionText::new("1.2.0").expect("valid version"),
                VersionScheme::SemVer,
                ExecutionEligibility::ExactOnly,
            ),
        }],
    )
    .expect("valid plan");
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::new(
            PlanItemId::new("pnpm:alpha-ready").expect("valid id"),
            false,
        )],
        Vec::new(),
    )
    .expect("valid selection");
    let execution_plan = resolve_selection_for_execution(
        &plan,
        &selection,
        ExecutionCapabilities {
            exact_target: manager.capabilities().exact_target,
            native_update: manager.capabilities().native_update,
            native_global_update: manager.capabilities().native_global_update,
        },
        VersionPolicy::Stable,
    )
    .expect("selection should resolve");

    let commands = manager
        .commands_for_execution_plan(
            &upnow_infra::ProcessRunner::fake([]),
            &upnow_infra::Env::fixed([]),
            &execution_plan,
            CommandBuildSettings {
                version_policy: VersionPolicy::Stable,
                min_release_age: std::time::Duration::from_secs(86_400),
            },
        )
        .expect("pnpm adapter should build commands");

    assert_eq!(
        commands[0].command.display(),
        "pnpm add -g alpha-ready@1.2.0"
    );
}

#[test]
fn execution_resolver_rejects_non_executable_selected_items() {
    let plan = UpdatePlan::new(
        ManagerId::new("npm").expect("valid manager"),
        vec![PlanItem::Current {
            id: PlanItemId::new("npm:alpha-ready").expect("valid id"),
            installed: upnow_domain::InstalledTool::new(
                ManagerId::new("npm").expect("valid manager"),
                ToolId::new("alpha-ready").expect("valid tool id"),
                PackageName::new("alpha-ready").expect("valid package"),
                upnow_domain::ToolName::new("alpha-ready").expect("valid tool name"),
                VersionText::new("1.0.0").expect("valid version"),
                upnow_domain::ManagerMetadata::empty(),
            ),
        }],
    )
    .expect("valid plan");
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::new(
            PlanItemId::new("npm:alpha-ready").expect("valid id"),
            false,
        )],
        Vec::new(),
    )
    .expect("valid selection");

    let err = resolve_selection_for_execution(
        &plan,
        &selection,
        ExecutionCapabilities {
            exact_target: true,
            native_update: true,
            native_global_update: false,
        },
        VersionPolicy::Stable,
    )
    .expect_err("current item should not be executable");

    assert_eq!(
        err,
        ExecutionSelectionError::ItemNotExecutable("npm:alpha-ready".to_owned())
    );
}
