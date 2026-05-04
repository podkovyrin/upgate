use upnow_domain::{
    ExecutionEligibility, ManagerId, PackageName, PlanItem, PlanItemId, PlanSelection,
    SelectedItem, ToolId, UpdateCandidate, UpdatePlan, VersionPolicy, VersionScheme, VersionText,
};
use upnow_managers::adapter::{CommandBuildSettings, ManagerAdapterError, ManagerAdapterErrorKind};
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

    assert_eq!(pnpm.id(), "pnpm");
    assert_eq!(npm.id(), "npm");
    assert_eq!(yarn.id(), "yarn");
    assert_eq!(bun.id(), "bun");
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
                .expect("migrated npm-family managers support all current policies");
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

    assert!(pnpm.capabilities().exact_target);
    assert!(!pnpm.capabilities().native_update);
    assert!(npm.capabilities().exact_target);
    assert!(npm.capabilities().native_update);
    assert!(yarn.capabilities().exact_target);
    assert!(!yarn.capabilities().native_update);
    assert!(bun.capabilities().exact_target);
    assert!(!bun.capabilities().native_update);
    assert!(bun.capabilities().native_global_update);
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

    let commands = manager
        .commands_for_selection(
            &upnow_infra::ProcessRunner::fake([]),
            &plan,
            &selection,
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
fn adapter_errors_preserve_command_construction_category() {
    let manager =
        manager_by_id(&ManagerId::new("npm").expect("valid id")).expect("npm should be registered");
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

    let err = manager
        .commands_for_selection(
            &upnow_infra::ProcessRunner::fake([]),
            &plan,
            &selection,
            CommandBuildSettings {
                version_policy: VersionPolicy::Stable,
                min_release_age: std::time::Duration::from_secs(86_400),
            },
        )
        .expect_err("current item should not be executable");

    match err {
        ManagerAdapterError::Manager { kind, .. } => {
            assert_eq!(kind, ManagerAdapterErrorKind::CommandConstruction);
        }
        other => panic!("unexpected error: {other}"),
    }
}
