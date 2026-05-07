use upnow_domain::{
    BlockReason, DelayReason, ExecutionEligibility, ManagerCapabilities, ManagerId,
    ManagerSelectedTarget, PackageName, PlanItem, PlanItemId, PlanSelection, SelectedItem,
    TargetAgeLookupResult, ToolId, UpdateCandidate, UpdatePlan, UpdateSeed, VersionPolicy,
    VersionScheme, VersionText,
};
use upnow_execution::{
    ExecutionCommand, ExecutionCommandIntent, ExecutionCommandItem, ExecutionSelectionError,
    ExecutionStatus, execute_commands, resolve_selection_for_execution,
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

#[test]
fn grouped_native_command_reports_each_selected_item() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(success_status(), "", ""))]);
    let command = ExecutionCommand {
        items: vec![command_item(), command_item_for("beta-ready")],
        command: CommandSpec::new("tool", ["upgrade", "alpha-ready", "beta-ready"]).mutating(),
    };

    let report = execute_commands(
        ManagerId::new("brew").expect("valid manager"),
        vec![command],
        &process,
    )
    .expect("grouped command should report every item");

    assert_eq!(report.items.len(), 2);
    assert_eq!(report.items[0].package_name.as_str(), "alpha-ready");
    assert_eq!(report.items[1].package_name.as_str(), "beta-ready");
    assert!(
        report
            .items
            .iter()
            .all(|item| matches!(item.status, ExecutionStatus::Succeeded { .. }))
    );
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
    let selection = recommended_selection(&plan);

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        capabilities(false),
        VersionPolicy::None,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::NativeSelected(_)]
    ));
}

#[test]
fn resolves_resolver_native_intent_for_resolver_selected_updates() {
    let plan = plan(vec![PlanItem::Update {
        id: plan_item_id(),
        candidate: candidate(ExecutionEligibility::ResolverNativeOnly),
    }]);
    let selection = recommended_selection(&plan);

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        capabilities(false),
        VersionPolicy::None,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::ResolverNative(item)] if !item.bypass_min_release_age
    ));
}

#[test]
fn resolves_resolver_native_global_intent_for_complete_default_selection() {
    let plan = plan(vec![
        PlanItem::Update {
            id: plan_item_id(),
            candidate: candidate(ExecutionEligibility::ResolverNativeOnly),
        },
        PlanItem::Update {
            id: PlanItemId::new("pnpm:beta-ready").expect("valid id"),
            candidate: candidate_for("beta-ready", ExecutionEligibility::ResolverNativeOnly),
        },
    ]);
    let selection = PlanSelection::new(
        &plan,
        vec![
            SelectedItem::recommended(plan_item_id()),
            SelectedItem::recommended(PlanItemId::new("pnpm:beta-ready").expect("valid id")),
        ],
        Vec::new(),
    )
    .expect("valid selection");

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        resolver_global_capabilities(),
        VersionPolicy::None,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::ResolverNativeGlobal(items)] if items.len() == 2
    ));
}

#[test]
fn resolver_native_global_is_not_used_when_plan_contains_blocked_items() {
    let plan = plan(vec![
        PlanItem::Update {
            id: plan_item_id(),
            candidate: candidate(ExecutionEligibility::ResolverNativeOnly),
        },
        PlanItem::Blocked {
            id: PlanItemId::new("pnpm:missing-age").expect("valid id"),
            seed: seed_for("missing-age"),
            reason: BlockReason::MissingReleaseMetadata,
            policy_warnings: Vec::new(),
        },
    ]);
    let selection = recommended_selection(&plan);

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        resolver_global_capabilities(),
        VersionPolicy::None,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::ResolverNative(item)]
            if item.package_name.as_str() == "alpha-ready"
    ));
}

#[test]
fn forced_resolver_native_delayed_selection_is_rejected_without_bypass_command() {
    let plan = plan(vec![PlanItem::Delayed {
        id: plan_item_id(),
        candidate: candidate(ExecutionEligibility::ResolverNativeOnly),
        reason: DelayReason::ReleaseTooFresh,
    }]);
    let selection = forced_selection(&plan);

    let err = resolve_selection_for_execution(
        &plan,
        &selection,
        capabilities(false),
        VersionPolicy::None,
    )
    .expect_err("forced resolver-native selection should require an explicit bypass command");

    assert_eq!(
        err,
        ExecutionSelectionError::ExactTargetUnsupported("pnpm:alpha-ready".to_owned())
    );
}

#[test]
fn resolves_exact_intent_for_policy_filtered_updates() {
    let plan = plan(vec![PlanItem::Update {
        id: plan_item_id(),
        candidate: candidate(ExecutionEligibility::NativeOrExact),
    }]);
    let selection = recommended_selection(&plan);

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        capabilities(false),
        VersionPolicy::Stable,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::Exact(_)]
    ));
}

#[test]
fn native_only_policy_filtered_update_stays_native_selected() {
    let plan = plan(vec![
        PlanItem::Update {
            id: plan_item_id(),
            candidate: candidate(ExecutionEligibility::NativeOnly),
        },
        PlanItem::Update {
            id: PlanItemId::new("pnpm:beta-ready").expect("valid id"),
            candidate: candidate_for("beta-ready", ExecutionEligibility::NativeOnly),
        },
        PlanItem::Blocked {
            id: PlanItemId::new("pnpm:missing-age").expect("valid id"),
            seed: seed_for("missing-age"),
            reason: BlockReason::MissingReleaseMetadata,
            policy_warnings: Vec::new(),
        },
    ]);
    let selection = recommended_selection(&plan);

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        capabilities(true),
        VersionPolicy::Stable,
    )
    .expect("native-only policy-gated selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::NativeSelected(item)]
            if item.package_name.as_str() == "alpha-ready"
    ));
}

#[test]
fn forced_delayed_selection_resolves_to_exact_intent() {
    let plan = plan(vec![PlanItem::Delayed {
        id: plan_item_id(),
        candidate: candidate(ExecutionEligibility::NativeOrExact),
        reason: DelayReason::ReleaseTooFresh,
    }]);
    let selection = forced_selection(&plan);

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        capabilities(false),
        VersionPolicy::None,
    )
    .expect("forced delayed selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::Exact(item)] if item.bypass_min_release_age
    ));
}

#[test]
fn alternate_exact_selection_resolves_to_exact_target_version() {
    let plan = plan(vec![PlanItem::Update {
        id: plan_item_id(),
        candidate: candidate(ExecutionEligibility::NativeOrExact),
    }]);
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::alternate_exact(
            plan_item_id(),
            VersionText::new("1.1.0").expect("valid alternate version"),
        )],
        Vec::new(),
    )
    .expect("valid selection");

    let resolved =
        resolve_selection_for_execution(&plan, &selection, capabilities(true), VersionPolicy::None)
            .expect("alternate exact selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::Exact(item)]
            if item.target_version.as_str() == "1.1.0"
                && item.exact_target_required
                && !item.bypass_min_release_age
    ));
}

#[test]
fn alternate_exact_selection_rejects_non_exact_capable_item() {
    let plan = plan(vec![PlanItem::Update {
        id: plan_item_id(),
        candidate: candidate(ExecutionEligibility::NativeOnly),
    }]);
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::alternate_exact(
            plan_item_id(),
            VersionText::new("1.1.0").expect("valid alternate version"),
        )],
        Vec::new(),
    )
    .expect("valid selection");

    let err =
        resolve_selection_for_execution(&plan, &selection, capabilities(true), VersionPolicy::None)
            .expect_err("alternate exact selection should require exact support");

    assert_eq!(
        err,
        ExecutionSelectionError::ExactTargetUnsupported("pnpm:alpha-ready".to_owned())
    );
}

#[test]
fn policy_filtered_complete_exact_capable_selection_resolves_to_exact_intents() {
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
            SelectedItem::recommended(plan_item_id()),
            SelectedItem::recommended(PlanItemId::new("pnpm:beta-ready").expect("valid id")),
        ],
        Vec::new(),
    )
    .expect("valid selection");

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        capabilities(true),
        VersionPolicy::Stable,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::Exact(first), ExecutionCommandIntent::Exact(second)]
            if first.package_name.as_str() == "alpha-ready"
                && second.package_name.as_str() == "beta-ready"
    ));
}

#[test]
fn policy_filtered_complete_native_only_selection_resolves_to_native_global() {
    let plan = plan(vec![
        PlanItem::Update {
            id: plan_item_id(),
            candidate: candidate(ExecutionEligibility::NativeOnly),
        },
        PlanItem::Update {
            id: PlanItemId::new("pnpm:beta-ready").expect("valid id"),
            candidate: candidate_for("beta-ready", ExecutionEligibility::NativeOnly),
        },
    ]);
    let selection = PlanSelection::new(
        &plan,
        vec![
            SelectedItem::recommended(plan_item_id()),
            SelectedItem::recommended(PlanItemId::new("pnpm:beta-ready").expect("valid id")),
        ],
        Vec::new(),
    )
    .expect("valid selection");

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        capabilities(true),
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
    let selection = recommended_selection(&plan);

    let err = resolve_selection_for_execution(
        &plan,
        &selection,
        capabilities(false),
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
    command_item_for("alpha-ready")
}

fn command_item_for(package_name: &str) -> ExecutionCommandItem {
    ExecutionCommandItem {
        plan_item_id: PlanItemId::new(format!("pnpm:{package_name}")).expect("valid id"),
        package_name: PackageName::new(package_name).expect("valid package"),
        installed_version: VersionText::new("1.0.0").expect("valid version"),
        target_version: VersionText::new("1.2.0").expect("valid version"),
    }
}

fn plan(items: Vec<PlanItem>) -> UpdatePlan {
    UpdatePlan::new(ManagerId::new("pnpm").expect("valid manager"), items).expect("valid plan")
}

fn recommended_selection(plan: &UpdatePlan) -> PlanSelection {
    PlanSelection::new(
        plan,
        vec![SelectedItem::recommended(plan_item_id())],
        Vec::new(),
    )
    .expect("valid selection")
}

fn forced_selection(plan: &UpdatePlan) -> PlanSelection {
    PlanSelection::new(
        plan,
        vec![SelectedItem::forced_candidate(plan_item_id())],
        Vec::new(),
    )
    .expect("valid selection")
}

fn capabilities(native_global_update: bool) -> ManagerCapabilities {
    ManagerCapabilities::new().with_native_global_update(native_global_update)
}

fn resolver_global_capabilities() -> ManagerCapabilities {
    ManagerCapabilities::new().with_resolver_native_global_update(true)
}

fn candidate(execution_eligibility: ExecutionEligibility) -> UpdateCandidate {
    candidate_for("alpha-ready", execution_eligibility)
}

fn candidate_for(package: &str, execution_eligibility: ExecutionEligibility) -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new(package).expect("valid tool id"),
        PackageName::new(package).expect("valid package"),
        VersionText::new("1.0.0").expect("valid version"),
        VersionText::new("1.2.0").expect("valid version"),
        VersionScheme::SemVer,
        execution_eligibility,
    )
}

fn seed_for(package: &str) -> UpdateSeed {
    UpdateSeed::manager_selected(
        upnow_domain::InstalledTool::new(
            ManagerId::new("pnpm").expect("valid manager"),
            ToolId::new(package).expect("valid tool id"),
            PackageName::new(package).expect("valid package"),
            upnow_domain::ToolName::new(package).expect("valid tool name"),
            VersionText::new("1.0.0").expect("valid version"),
            upnow_domain::ManagerMetadata::empty(),
        ),
        ManagerSelectedTarget::new(
            VersionText::new("1.2.0").expect("valid version"),
            TargetAgeLookupResult::MissingMetadata,
        ),
        VersionScheme::SemVer,
        ExecutionEligibility::ExactOnly,
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
