use std::time::{Duration, SystemTime};

use upgate_domain::{
    AuditLookupResult, BlockReason, CandidateEvaluationFact, DelayReason, ExecutionSupport,
    ExecutionTargetKind, InstalledTool, ManagerCapabilities, ManagerId, ManagerSelectedTarget,
    MinAgeConstraintSupport, MissingMetadataKind, PackageName, PlanDiagnostics, PlanItem,
    PlanItemId, PlanSelection, PolicyBlockReason, ReleaseEntry, ReleaseLookupResult,
    ReleaseTimeline, ReleaseTimestamp, SelectedItem, TargetAgeLookupResult, ToolId, ToolName,
    UpdateCandidate, UpdatePlan, UpdateSeed, UpdateSelectionPolicy, VersionPolicy, VersionScheme,
    VersionText,
};
use upgate_execution::{
    ExecutionCommandIntent, ResolvedExecutionTarget, resolve_selection_for_execution,
};

#[test]
fn resolves_native_global_intent_for_complete_native_only_selection() {
    let plan = plan(vec![
        update_item(
            "pnpm:alpha-ready",
            "alpha-ready",
            ExecutionSupport::native_only(),
        ),
        update_item(
            "pnpm:beta-ready",
            "beta-ready",
            ExecutionSupport::native_only(),
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
            ExecutionSupport::exact_only(),
        ),
        update_item(
            "pnpm:beta-ready",
            "beta-ready",
            ExecutionSupport::exact_only(),
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
            ExecutionSupport::exact_or_native_global(),
        ),
        update_item(
            "pnpm:beta-ready",
            "beta-ready",
            ExecutionSupport::exact_or_native_global(),
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
            ExecutionSupport::native_only(),
            ExecutionTargetKind::BrewFormula,
        ),
        update_item_with_target_kind(
            "brew:beta-ready",
            "beta-ready",
            ExecutionSupport::native_only(),
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
        ExecutionSupport::native_or_exact(),
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
        ExecutionSupport::exact_only(),
    )]);
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::exact(
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
            if item.known_target_version().expect("known target").as_str() == "2.0.0" && item.bypass_min_release_age
    ));
}

#[test]
fn forced_policy_blocked_item_resolves_exact_intent() {
    let plan = plan(vec![blocked_policy_item(
        "pnpm:alpha-blocked",
        "alpha-blocked",
        ExecutionSupport::exact_only(),
    )]);
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::force_planned_candidate(
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
            if item.known_target_version().expect("known target").as_str() == "2.0.0" && item.exact_target_required
    ));
}

#[test]
fn forced_audit_blocked_item_uses_audit_blocking_candidate_target() {
    let plan = plan(vec![blocked_audit_item(
        "pnpm:alpha-blocked",
        "alpha-blocked",
        ExecutionSupport::exact_only(),
    )]);
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::force_planned_candidate(
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
            if item.known_target_version().expect("known target").as_str() == "2.0.0" && item.exact_target_required
    ));
}

#[test]
fn forced_delayed_resolver_native_item_bypasses_age_limit() {
    let plan = plan(vec![delayed_item(
        "mise:node",
        "node",
        ExecutionSupport::resolver_native(MinAgeConstraintSupport::Optional, true, false),
    )]);
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::force_planned_candidate(
            PlanItemId::new("mise:node").expect("valid id"),
        )],
        UpdateSelectionPolicy::default(),
    )
    .expect("valid selection");

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        ManagerCapabilities::new(),
        VersionPolicy::None,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::ResolverNative(item)]
            if item.known_target_version().expect("known target").as_str() == "1.2.0"
                && item.bypass_min_release_age
    ));
}

#[test]
fn manager_resolved_with_resolver_native_support_resolves_resolver_native() {
    let plan = plan(vec![manager_resolved_update_item(
        "uv:ruff",
        "ruff",
        ExecutionSupport::resolver_native(MinAgeConstraintSupport::Optional, true, false),
    )]);
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::manager_resolved(
            PlanItemId::new("uv:ruff").expect("valid id"),
        )],
        UpdateSelectionPolicy::default(),
    )
    .expect("valid selection");

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        ManagerCapabilities::new(),
        VersionPolicy::None,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::ResolverNative(item)]
            if item.target == ResolvedExecutionTarget::ManagerResolved
    ));
}

#[test]
fn manager_resolved_with_native_selected_support_resolves_native_selected() {
    let plan = plan(vec![manager_resolved_update_item(
        "npm:eslint",
        "eslint",
        ExecutionSupport::native_only(),
    )]);
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::manager_resolved(
            PlanItemId::new("npm:eslint").expect("valid id"),
        )],
        UpdateSelectionPolicy::default(),
    )
    .expect("valid selection");

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        ManagerCapabilities::new(),
        VersionPolicy::None,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::NativeSelected(item)]
            if item.target == ResolvedExecutionTarget::ManagerResolved
    ));
}

#[test]
fn manager_resolved_missing_selected_metadata_resolves_resolver_native() {
    let plan = plan(vec![blocked_missing_selected_update_metadata_item(
        "uv:ruff",
        "ruff",
        ExecutionSupport::resolver_native(MinAgeConstraintSupport::Optional, true, false),
    )]);
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::manager_resolved(
            PlanItemId::new("uv:ruff").expect("valid id"),
        )],
        UpdateSelectionPolicy::default(),
    )
    .expect("valid selection");

    let resolved = resolve_selection_for_execution(
        &plan,
        &selection,
        ManagerCapabilities::new(),
        VersionPolicy::None,
    )
    .expect("selection should resolve");

    assert!(matches!(
        resolved.intents.as_slice(),
        [ExecutionCommandIntent::ResolverNative(item)]
            if item.target == ResolvedExecutionTarget::ManagerResolved
    ));
}

#[test]
fn unsupported_manager_resolved_selected_update_is_rejected() {
    let plan = plan(vec![manager_resolved_update_item(
        "pnpm:alpha",
        "alpha",
        ExecutionSupport::exact_only(),
    )]);
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::manager_resolved(
            PlanItemId::new("pnpm:alpha").expect("valid id"),
        )],
        UpdateSelectionPolicy::default(),
    )
    .expect("valid selection");

    let err = resolve_selection_for_execution(
        &plan,
        &selection,
        ManagerCapabilities::new(),
        VersionPolicy::None,
    )
    .expect_err("targetless exact-only item should be rejected");

    assert_eq!(
        err.to_string(),
        "plan item `pnpm:alpha` does not support manager-resolved selected execution"
    );
}

fn update_item(id: &str, package: &str, eligibility: ExecutionSupport) -> PlanItem {
    update_item_with_target_kind(id, package, eligibility, ExecutionTargetKind::Standard)
}

fn update_item_with_diagnostics(
    id: &str,
    package: &str,
    eligibility: ExecutionSupport,
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
                age_allowed: false,
                policy_block_reason: None,
                policy_warning: None,
                audit: None,
            }],
            ..PlanDiagnostics::default()
        }),
    }
}

fn update_item_with_target_kind(
    id: &str,
    package: &str,
    eligibility: ExecutionSupport,
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

fn delayed_item(id: &str, package: &str, eligibility: ExecutionSupport) -> PlanItem {
    PlanItem::Delayed {
        id: PlanItemId::new(id).expect("valid id"),
        candidate: candidate(id, package, eligibility).with_diagnostics(PlanDiagnostics {
            required_age: Duration::from_secs(7 * 24 * 60 * 60),
            candidates: vec![CandidateEvaluationFact {
                version: VersionText::new("1.2.0").expect("valid version"),
                age: Some(Duration::from_secs(24 * 60 * 60)),
                age_allowed: false,
                policy_block_reason: None,
                policy_warning: None,
                audit: None,
            }],
            ..PlanDiagnostics::default()
        }),
        reason: DelayReason::ReleaseTooFresh,
    }
}

fn manager_resolved_update_item(
    id: &str,
    package: &str,
    eligibility: ExecutionSupport,
) -> PlanItem {
    PlanItem::Update {
        id: PlanItemId::new(id).expect("valid id"),
        candidate: candidate(id, package, eligibility),
    }
}

fn candidate(id: &str, package: &str, eligibility: ExecutionSupport) -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new(id).expect("valid tool id"),
        PackageName::new(package).expect("valid package"),
        VersionText::new("1.0.0").expect("valid version"),
        VersionText::new("1.2.0").expect("valid version"),
        VersionScheme::SemVer,
        eligibility,
    )
}

fn blocked_policy_item(id: &str, package: &str, eligibility: ExecutionSupport) -> PlanItem {
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
                age_allowed: true,
                policy_block_reason: Some(PolicyBlockReason::PreReleaseBlocked),
                policy_warning: None,
                audit: None,
            }],
            ..PlanDiagnostics::default()
        },
    }
}

fn blocked_audit_item(id: &str, package: &str, eligibility: ExecutionSupport) -> PlanItem {
    let audit_blocking_candidate = CandidateEvaluationFact {
        version: VersionText::new("2.0.0").expect("valid version"),
        age: Some(Duration::from_secs(30 * 24 * 60 * 60)),
        age_allowed: true,
        policy_block_reason: None,
        policy_warning: None,
        audit: Some(AuditLookupResult::LookupFailed {
            detail: "OSV unavailable".to_owned(),
        }),
    };
    PlanItem::Blocked {
        id: PlanItemId::new(id).expect("valid id"),
        seed: UpdateSeed::new(
            installed_tool(package),
            VersionText::new("3.0.0").expect("valid version"),
            VersionScheme::SemVer,
            release_lookup("3.0.0"),
            eligibility,
        ),
        reason: BlockReason::AuditLookupFailed,
        policy_warnings: Vec::new(),
        diagnostics: PlanDiagnostics {
            required_age: Duration::from_secs(7 * 24 * 60 * 60),
            candidates: vec![audit_blocking_candidate.clone()],
            audit_blocking_target: audit_blocking_candidate.audit.clone(),
            audit_blocking_candidate: Some(audit_blocking_candidate),
            ..PlanDiagnostics::default()
        },
    }
}

fn blocked_missing_selected_update_metadata_item(
    id: &str,
    package: &str,
    eligibility: ExecutionSupport,
) -> PlanItem {
    PlanItem::Blocked {
        id: PlanItemId::new(id).expect("valid id"),
        seed: UpdateSeed::manager_selected(
            installed_tool(package),
            ManagerSelectedTarget::new(
                VersionText::new("2.0.0").expect("valid version"),
                TargetAgeLookupResult::MissingMetadata,
            ),
            VersionScheme::Pep440,
            eligibility,
        ),
        reason: BlockReason::MissingReleaseMetadata,
        policy_warnings: Vec::new(),
        diagnostics: PlanDiagnostics::new(Duration::from_secs(7 * 24 * 60 * 60))
            .with_missing_metadata(MissingMetadataKind::SelectedUpdate),
    }
}

fn installed_tool(package: &str) -> InstalledTool {
    InstalledTool::new(
        ManagerId::new("pnpm").expect("valid manager"),
        ToolId::new(format!("pnpm:{package}")).expect("valid tool id"),
        PackageName::new(package).expect("valid package"),
        ToolName::new(package).expect("valid tool name"),
        VersionText::new("1.0.0").expect("valid version"),
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
