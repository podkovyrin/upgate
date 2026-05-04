use upnow_domain::{
    DelayReason, ExecutionEligibility, ManagerId, PackageName, PlanItem, PlanItemId, PlanSelection,
    SelectedItem, ToolId, UpdateCandidate, UpdatePlan, VersionPolicy, VersionScheme, VersionText,
};
use upnow_execution::{
    ExecutionCapabilities, ExecutionCommand, ExecutionCommandIntent, ExecutionCommandItem,
    ExecutionSelectionError, ExecutionStatus, execute_commands, resolve_selection_for_execution,
};
use upnow_infra::{CommandOutput, CommandSpec, ProcessRunner};

#[test]
fn executes_manager_supplied_commands() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(success_status(), "", ""))]);
    let command = ExecutionCommand {
        items: vec![command_item()],
        command: CommandSpec::new("tool", ["install", "alpha-ready@1.2.0"]).mutating(),
    };

    let report = execute_commands(
        ManagerId::new("pnpm").expect("valid manager"),
        vec![command],
        &process,
    )
    .expect("execution should report success");

    assert_eq!(report.items.len(), 1);
    assert!(matches!(
        report.items[0].status,
        ExecutionStatus::Succeeded { .. }
    ));
    let calls = match &process {
        ProcessRunner::Fake(fake) => fake.calls(),
        ProcessRunner::Real { .. } => panic!("expected fake process"),
    };
    assert_eq!(calls[0].display(), "tool install alpha-ready@1.2.0");
}

#[test]
fn command_failures_are_item_scoped() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(
        failure_status(),
        "",
        "install failed",
    ))]);

    let report = execute_commands(
        ManagerId::new("pnpm").expect("valid manager"),
        vec![command()],
        &process,
    )
    .expect("ordinary command failure should stay item-scoped");

    assert!(matches!(
        report.items[0].status,
        ExecutionStatus::Failed { .. }
    ));
}

#[cfg(unix)]
#[test]
fn interrupted_commands_return_execution_error() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(signal_status(), "", ""))]);

    let err = execute_commands(
        ManagerId::new("pnpm").expect("valid manager"),
        vec![command()],
        &process,
    )
    .expect_err("signal should interrupt execution");

    assert!(err.is_interruption());
}

#[test]
fn resolves_native_selected_intent_for_no_policy_native_updates() {
    let plan = plan(vec![PlanItem::Update {
        id: plan_item_id(),
        candidate: candidate(ExecutionEligibility::NativeOrExact),
    }]);
    let selection = selection(&plan, false);

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        capabilities(true, true, false),
        VersionPolicy::None,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::NativeSelected(_)]
    ));
}

#[test]
fn resolves_exact_intent_for_policy_filtered_updates() {
    let plan = plan(vec![PlanItem::Update {
        id: plan_item_id(),
        candidate: candidate(ExecutionEligibility::NativeOrExact),
    }]);
    let selection = selection(&plan, false);

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        capabilities(true, true, false),
        VersionPolicy::Stable,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::Exact(_)]
    ));
}

#[test]
fn forced_delayed_selection_resolves_to_exact_intent() {
    let plan = plan(vec![PlanItem::Delayed {
        id: plan_item_id(),
        candidate: candidate(ExecutionEligibility::NativeOrExact),
        reason: DelayReason::ReleaseTooFresh,
    }]);
    let selection = selection(&plan, true);

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        capabilities(true, true, false),
        VersionPolicy::None,
    )
    .expect("forced delayed selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::Exact(item)] if item.forced
    ));
}

#[test]
fn complete_unforced_update_selection_resolves_to_native_global() {
    let plan = plan(vec![
        PlanItem::Update {
            id: plan_item_id(),
            candidate: candidate(ExecutionEligibility::ExactOnly),
        },
        PlanItem::Update {
            id: PlanItemId::new("pnpm:beta-ready").expect("valid id"),
            candidate: UpdateCandidate::new(
                ToolId::new("beta-ready").expect("valid tool id"),
                PackageName::new("beta-ready").expect("valid package"),
                VersionText::new("1.0.0").expect("valid version"),
                VersionText::new("1.2.0").expect("valid version"),
                VersionScheme::SemVer,
                ExecutionEligibility::ExactOnly,
            ),
        },
    ]);
    let selection = PlanSelection::new(
        &plan,
        vec![
            SelectedItem::new(plan_item_id(), false),
            SelectedItem::new(PlanItemId::new("pnpm:beta-ready").expect("valid id"), false),
        ],
        Vec::new(),
    )
    .expect("valid selection");

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        capabilities(true, false, true),
        VersionPolicy::Stable,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::NativeGlobal(items)] if items.len() == 2
    ));
}

#[test]
fn current_selection_is_not_executable() {
    let plan = plan(vec![PlanItem::Current {
        id: plan_item_id(),
        installed: upnow_domain::InstalledTool::new(
            ManagerId::new("pnpm").expect("valid manager"),
            ToolId::new("alpha-ready").expect("valid tool id"),
            PackageName::new("alpha-ready").expect("valid package"),
            upnow_domain::ToolName::new("alpha-ready").expect("valid tool name"),
            VersionText::new("1.0.0").expect("valid version"),
            upnow_domain::ManagerMetadata::empty(),
        ),
    }]);
    let selection = selection(&plan, false);

    let err = resolve_selection_for_execution(
        &plan,
        &selection,
        capabilities(true, true, false),
        VersionPolicy::Stable,
    )
    .expect_err("current item should not resolve");

    assert_eq!(
        err,
        ExecutionSelectionError::ItemNotExecutable("pnpm:alpha-ready".to_owned())
    );
}

fn command() -> ExecutionCommand {
    ExecutionCommand {
        items: vec![command_item()],
        command: CommandSpec::new("tool", ["install", "alpha-ready@1.2.0"]).mutating(),
    }
}

fn command_item() -> ExecutionCommandItem {
    ExecutionCommandItem {
        plan_item_id: PlanItemId::new("pnpm:alpha-ready").expect("valid id"),
        package_name: PackageName::new("alpha-ready").expect("valid package"),
        installed_version: VersionText::new("1.0.0").expect("valid version"),
        target_version: VersionText::new("1.2.0").expect("valid version"),
    }
}

fn plan(items: Vec<PlanItem>) -> UpdatePlan {
    UpdatePlan::new(ManagerId::new("pnpm").expect("valid manager"), items).expect("valid plan")
}

fn selection(plan: &UpdatePlan, forced: bool) -> PlanSelection {
    PlanSelection::new(
        plan,
        vec![SelectedItem::new(plan_item_id(), forced)],
        Vec::new(),
    )
    .expect("valid selection")
}

fn capabilities(
    exact_target: bool,
    native_update: bool,
    native_global_update: bool,
) -> ExecutionCapabilities {
    ExecutionCapabilities {
        exact_target,
        native_update,
        native_global_update,
    }
}

fn candidate(execution_eligibility: ExecutionEligibility) -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new("alpha-ready").expect("valid tool id"),
        PackageName::new("alpha-ready").expect("valid package"),
        VersionText::new("1.0.0").expect("valid version"),
        VersionText::new("1.2.0").expect("valid version"),
        VersionScheme::SemVer,
        execution_eligibility,
    )
}

fn plan_item_id() -> PlanItemId {
    PlanItemId::new("pnpm:alpha-ready").expect("valid id")
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
