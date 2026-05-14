use std::time::{Duration, SystemTime};

use upnow_domain::{
    BlockReason, CandidateEvaluationFact, ExecutionEligibility, ExecutionTargetKind, InstalledTool,
    ManagerCapabilities, ManagerId, ManagerMetadata, PackageName, PlanDiagnostics, PlanItem,
    PlanItemId, PlanSelection, PolicyBlockReason, ReleaseEntry, ReleaseLookupResult,
    ReleaseTimeline, ReleaseTimestamp, SelectedItem, ToolId, ToolName, UpdateCandidate, UpdatePlan,
    UpdateSeed, UpdateSelectionPolicy, VersionPolicy, VersionScheme, VersionText,
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

#[test]
fn alternate_exact_too_fresh_target_bypasses_min_release_age() {
    let plan = plan(vec![update_item_with_diagnostics(
        "pnpm:alpha-ready",
        "alpha-ready",
        ExecutionEligibility::ExactOnly,
    )]);
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::alternate_exact(
            PlanItemId::new("pnpm:alpha-ready").expect("valid id"),
            VersionText::new("2.0.0").expect("valid version"),
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
        [ExecutionCommandIntent::Exact(item)]
            if item.target_version.as_str() == "2.0.0" && item.bypass_min_release_age
    ));
}

#[test]
fn forced_policy_blocked_item_resolves_exact_intent() {
    let plan = plan(vec![blocked_policy_item(
        "pnpm:alpha-blocked",
        "alpha-blocked",
        ExecutionEligibility::ExactOnly,
    )]);
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::forced_candidate(
            PlanItemId::new("pnpm:alpha-blocked").expect("valid id"),
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
        [ExecutionCommandIntent::Exact(item)]
            if item.target_version.as_str() == "2.0.0" && item.exact_target_required
    ));
}

fn update_item(id: &str, package: &str, eligibility: ExecutionEligibility) -> PlanItem {
    update_item_with_target_kind(id, package, eligibility, ExecutionTargetKind::Standard)
}

fn update_item_with_diagnostics(
    id: &str,
    package: &str,
    eligibility: ExecutionEligibility,
) -> PlanItem {
    let PlanItem::Update { id, candidate } =
        update_item_with_target_kind(id, package, eligibility, ExecutionTargetKind::Standard)
    else {
        unreachable!("helper always builds update item");
    };
    PlanItem::Update {
        id,
        candidate: candidate.with_diagnostics(PlanDiagnostics {
            required_age: Duration::from_secs(7 * 24 * 60 * 60),
            candidates: vec![CandidateEvaluationFact {
                version: VersionText::new("2.0.0").expect("valid version"),
                age: Some(Duration::from_secs(24 * 60 * 60)),
                policy_allowed: true,
                age_allowed: false,
                policy_block_reason: None,
                policy_warning: None,
            }],
            ..PlanDiagnostics::default()
        }),
    }
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

fn blocked_policy_item(id: &str, package: &str, eligibility: ExecutionEligibility) -> PlanItem {
    PlanItem::Blocked {
        id: PlanItemId::new(id).expect("valid id"),
        seed: UpdateSeed::new(
            installed_tool(package),
            VersionText::new("2.0.0").expect("valid version"),
            VersionScheme::SemVer,
            release_lookup("2.0.0"),
            eligibility,
        ),
        reason: BlockReason::VersionPolicy(PolicyBlockReason::PreReleaseBlocked),
        policy_warnings: Vec::new(),
        diagnostics: PlanDiagnostics {
            required_age: Duration::from_secs(7 * 24 * 60 * 60),
            candidates: vec![CandidateEvaluationFact {
                version: VersionText::new("2.0.0").expect("valid version"),
                age: Some(Duration::from_secs(30 * 24 * 60 * 60)),
                policy_allowed: false,
                age_allowed: true,
                policy_block_reason: Some(PolicyBlockReason::PreReleaseBlocked),
                policy_warning: None,
            }],
            ..PlanDiagnostics::default()
        },
    }
}

fn installed_tool(package: &str) -> InstalledTool {
    InstalledTool::new(
        ManagerId::new("pnpm").expect("valid manager"),
        ToolId::new(format!("pnpm:{package}")).expect("valid tool id"),
        PackageName::new(package).expect("valid package"),
        ToolName::new(package).expect("valid tool name"),
        VersionText::new("1.0.0").expect("valid version"),
        ManagerMetadata::empty(),
    )
}

fn release_lookup(version: &str) -> ReleaseLookupResult {
    ReleaseLookupResult::Known(ReleaseTimeline::new(vec![ReleaseEntry::new(
        VersionText::new(version).expect("valid release version"),
        ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
    )]))
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
