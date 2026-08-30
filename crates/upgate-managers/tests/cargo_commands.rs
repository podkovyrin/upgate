use std::time::Duration;

use upgate_domain::{
    ExecutionSupport, ExecutionTargetKind, ManagerConfig, ManagerId, ManagerMode, PackageName,
    PlanItemId, UpdateSelectionPolicy, VersionPolicy, VersionText,
};
use upgate_execution::{
    ExecutionCommandIntent, ResolvedExecutionItem, ResolvedExecutionPlan, ResolvedExecutionTarget,
};
use upgate_infra::{Env, ProcessRunner};
use upgate_managers::adapter::ManagerAdapter;
use upgate_managers::cargo::CargoManager;

#[test]
fn installs_exact_targets_with_the_packaged_lockfile() {
    let manager = CargoManager::new(ManagerConfig {
        manager_id: ManagerId::new("cargo").expect("valid manager id"),
        mode: ManagerMode::Apply,
        min_release_age: Duration::from_secs(7 * 24 * 60 * 60),
        version_policy: VersionPolicy::None,
        no_update: false,
        selection: UpdateSelectionPolicy::default(),
    });
    let plan = ResolvedExecutionPlan {
        intents: vec![ExecutionCommandIntent::Exact(ResolvedExecutionItem {
            plan_item_id: PlanItemId::new("cargo:cargo-deny").expect("valid plan item id"),
            package_name: PackageName::new("cargo-deny").expect("valid package name"),
            installed_version: VersionText::new("0.19.0").expect("valid installed version"),
            target: ResolvedExecutionTarget::Known(
                VersionText::new("0.20.2").expect("valid target version"),
            ),
            execution_support: ExecutionSupport::exact_only(),
            execution_target_kind: ExecutionTargetKind::Standard,
            exact_target_required: false,
            bypass_min_release_age: false,
        })],
    };

    let commands = manager
        .commands_for_execution_plan(&ProcessRunner::fake([]), &Env::fixed([]), &plan)
        .expect("exact Cargo command should be supported");

    assert_eq!(
        commands[0].command.to_string(),
        "cargo install --force --locked cargo-deny@0.20.2"
    );
}
