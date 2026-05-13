use upnow_domain::{
    ExecutionEligibility, ExecutionTargetKind, ManagerCapabilities, ManagerId, PackageName,
    PlanItem, PlanItemId, PlanSelection, SelectedItem, ToolId, UpdateCandidate, UpdatePlan,
    UpdateSelectionPolicy, VersionPolicy, VersionScheme, VersionText,
};
use upnow_execution::{
    ExecutionCommand, ExecutionCommandIntent, ExecutionCommandItem, ExecutionStatus,
    execute_commands, resolve_selection_for_execution,
};
use upnow_infra::{CommandOutput, CommandSpec, ProcessRunner};

#[test]
fn executes_manager_supplied_commands() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(success_status(), "", ""))]);
    let command = ExecutionCommand {
        items: vec![command_item("alpha-ready")],
        command: CommandSpec::new("tool", ["install", "alpha-ready@1.2.0"]).mutating(),
    };

    let report = execute_commands(
        ManagerId::new("pnpm").expect("valid manager"),
        vec![command],
        &process,
    )
    .expect("execution should report success");

    assert!(matches!(
        report.items[0].status,
        ExecutionStatus::Succeeded { .. }
    ));
}

#[test]
fn resolves_native_global_intent_for_complete_native_only_selection() {
    let plan = plan(vec![
        update_item(
            "pnpm:alpha-ready",
            "alpha-ready",
            ExecutionEligibility::NativeOnly,
        ),
        update_item(
            "pnpm:beta-ready",
            "beta-ready",
            ExecutionEligibility::NativeOnly,
        ),
    ]);
    let selection = PlanSelection::new(
        &plan,
        vec![
            SelectedItem::recommended(PlanItemId::new("pnpm:alpha-ready").expect("valid id")),
            SelectedItem::recommended(PlanItemId::new("pnpm:beta-ready").expect("valid id")),
        ],
        UpdateSelectionPolicy::default(),
    )
    .expect("valid selection");

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        ManagerCapabilities::new().with_native_global_update(true),
        VersionPolicy::Stable,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::NativeGlobal(items)] if items.len() == 2
    ));
}

#[test]
fn does_not_resolve_native_global_for_exact_only_items() {
    let plan = plan(vec![
        update_item(
            "pnpm:alpha-ready",
            "alpha-ready",
            ExecutionEligibility::ExactOnly,
        ),
        update_item(
            "pnpm:beta-ready",
            "beta-ready",
            ExecutionEligibility::ExactOnly,
        ),
    ]);
    let selection = PlanSelection::new(
        &plan,
        vec![
            SelectedItem::recommended(PlanItemId::new("pnpm:alpha-ready").expect("valid id")),
            SelectedItem::recommended(PlanItemId::new("pnpm:beta-ready").expect("valid id")),
        ],
        UpdateSelectionPolicy::default(),
    )
    .expect("valid selection");

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        ManagerCapabilities::new().with_native_global_update(true),
        VersionPolicy::None,
    )
    .expect("selection should resolve");

    assert!(
        resolved
            .intents
            .iter()
            .all(|intent| { matches!(intent, ExecutionCommandIntent::Exact(_)) })
    );
}

#[test]
fn resolves_native_global_for_exact_or_native_global_items_with_no_policy() {
    let plan = plan(vec![
        update_item(
            "pnpm:alpha-ready",
            "alpha-ready",
            ExecutionEligibility::ExactOrNativeGlobal,
        ),
        update_item(
            "pnpm:beta-ready",
            "beta-ready",
            ExecutionEligibility::ExactOrNativeGlobal,
        ),
    ]);
    let selection = PlanSelection::new(
        &plan,
        vec![
            SelectedItem::recommended(PlanItemId::new("pnpm:alpha-ready").expect("valid id")),
            SelectedItem::recommended(PlanItemId::new("pnpm:beta-ready").expect("valid id")),
        ],
        UpdateSelectionPolicy::default(),
    )
    .expect("valid selection");

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        ManagerCapabilities::new().with_native_global_update(true),
        VersionPolicy::None,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::NativeGlobal(items)] if items.len() == 2
    ));
}

#[test]
fn resolves_grouped_native_intent_for_brew_target_kinds() {
    let plan = plan(vec![
        update_item_with_target_kind(
            "brew:alpha-ready",
            "alpha-ready",
            ExecutionEligibility::NativeOnly,
            ExecutionTargetKind::BrewFormula,
        ),
        update_item_with_target_kind(
            "brew:beta-ready",
            "beta-ready",
            ExecutionEligibility::NativeOnly,
            ExecutionTargetKind::BrewCask,
        ),
    ]);
    let selection = PlanSelection::new(
        &plan,
        vec![
            SelectedItem::recommended(PlanItemId::new("brew:alpha-ready").expect("valid id")),
            SelectedItem::recommended(PlanItemId::new("brew:beta-ready").expect("valid id")),
        ],
        UpdateSelectionPolicy::default(),
    )
    .expect("valid selection");

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        ManagerCapabilities::new(),
        VersionPolicy::Stable,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::GroupedNative(items)] if items.len() == 2
    ));
}

#[test]
fn resolves_exact_intent_for_policy_filtered_update() {
    let plan = plan(vec![update_item(
        "pnpm:alpha-ready",
        "alpha-ready",
        ExecutionEligibility::NativeOrExact,
    )]);
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::recommended(
            PlanItemId::new("pnpm:alpha-ready").expect("valid id"),
        )],
        UpdateSelectionPolicy::default(),
    )
    .expect("valid selection");

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        ManagerCapabilities::new(),
        VersionPolicy::Stable,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::Exact(_)]
    ));
}

fn update_item(id: &str, package: &str, eligibility: ExecutionEligibility) -> PlanItem {
    update_item_with_target_kind(id, package, eligibility, ExecutionTargetKind::Standard)
}

fn update_item_with_target_kind(
    id: &str,
    package: &str,
    eligibility: ExecutionEligibility,
    target_kind: ExecutionTargetKind,
) -> PlanItem {
    PlanItem::Update {
        id: PlanItemId::new(id).expect("valid id"),
        candidate: UpdateCandidate::new(
            ToolId::new(package).expect("valid tool id"),
            PackageName::new(package).expect("valid package"),
            VersionText::new("1.0.0").expect("valid version"),
            VersionText::new("1.2.0").expect("valid version"),
            VersionScheme::SemVer,
            eligibility,
        )
        .with_execution_target_kind(target_kind),
    }
}

fn plan(items: Vec<PlanItem>) -> UpdatePlan {
    UpdatePlan::new(ManagerId::new("pnpm").expect("valid manager"), items).expect("valid plan")
}

fn command_item(package_name: &str) -> ExecutionCommandItem {
    ExecutionCommandItem {
        plan_item_id: PlanItemId::new(format!("pnpm:{package_name}")).expect("valid id"),
        package_name: PackageName::new(package_name).expect("valid package"),
        installed_version: VersionText::new("1.0.0").expect("valid version"),
        target_version: VersionText::new("1.2.0").expect("valid version"),
    }
}

#[cfg(unix)]
fn success_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn success_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(0)
}
