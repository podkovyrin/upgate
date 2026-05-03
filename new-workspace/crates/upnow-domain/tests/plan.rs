use upnow_domain::{
    BlockReason, DelayReason, DomainError, ExecutionEligibility, InstalledTool, ManagerId,
    ManagerMetadata, PackageName, PlanItem, PlanItemId, PolicyBlockReason, ReleaseLookupResult,
    SkipReason, ToolId, ToolName, UpdateCandidate, UpdatePlan, UpdateSeed, VersionScheme,
    VersionText,
};

fn manager_id() -> ManagerId {
    ManagerId::new("pnpm").expect("valid manager id")
}

fn installed_tool(name: &str, version: &str) -> InstalledTool {
    InstalledTool::new(
        manager_id(),
        ToolId::new(format!("pnpm:{name}")).expect("valid tool id"),
        PackageName::new(name).expect("valid package name"),
        ToolName::new(name).expect("valid tool name"),
        VersionText::new(version).expect("valid version"),
        ManagerMetadata::empty(),
    )
}

fn candidate(name: &str) -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new(format!("pnpm:{name}")).expect("valid tool id"),
        PackageName::new(name).expect("valid package name"),
        VersionText::new("1.0.0").expect("valid installed version"),
        VersionText::new("1.2.0").expect("valid target version"),
        VersionScheme::SemVer,
        ExecutionEligibility::NativeOrExact,
    )
}

fn seed(name: &str) -> UpdateSeed {
    UpdateSeed::new(
        installed_tool(name, "1.0.0"),
        VersionText::new("1.2.0").expect("valid target version"),
        VersionScheme::SemVer,
        ReleaseLookupResult::MissingMetadata,
    )
}

fn item_id(value: &str) -> PlanItemId {
    PlanItemId::new(value).expect("valid plan item id")
}

#[test]
fn plan_item_id_rejects_empty_values() {
    assert_eq!(PlanItemId::new(" "), Err(DomainError::EmptyPlanItemId));
}

#[test]
fn plan_item_variants_represent_phase_two_states() {
    let update = PlanItem::Update {
        id: item_id("update"),
        candidate: candidate("alpha-ready"),
    };
    let current = PlanItem::Current {
        id: item_id("current"),
        installed: installed_tool("fresh-tool", "2.0.0"),
    };
    let delayed = PlanItem::Delayed {
        id: item_id("delayed"),
        candidate: candidate("gamma-delayed"),
        reason: DelayReason::ReleaseTooFresh,
    };
    let blocked = PlanItem::Blocked {
        id: item_id("blocked"),
        seed: seed("missing-age"),
        reason: BlockReason::VersionPolicy(PolicyBlockReason::PreReleaseBlocked),
        policy_warnings: Vec::new(),
    };
    let skipped = PlanItem::Skipped {
        id: item_id("skipped"),
        installed: installed_tool("pinned-pkg", "3.0.0"),
        reason: SkipReason::Pinned,
    };
    let resolver_error = PlanItem::ResolverError {
        id: item_id("resolver-error"),
        seed: seed("omega-error"),
        message: "registry timeout".to_owned(),
    };

    assert_eq!(update.id().as_str(), "update");
    assert_eq!(current.id().as_str(), "current");
    assert_eq!(delayed.id().as_str(), "delayed");
    assert_eq!(blocked.id().as_str(), "blocked");
    assert_eq!(skipped.id().as_str(), "skipped");
    assert_eq!(resolver_error.id().as_str(), "resolver-error");
}

#[test]
fn update_plan_rejects_duplicate_item_ids() {
    let duplicate_id = item_id("same-id");
    let result = UpdatePlan::new(
        manager_id(),
        vec![
            PlanItem::Update {
                id: duplicate_id.clone(),
                candidate: candidate("alpha-ready"),
            },
            PlanItem::Current {
                id: duplicate_id,
                installed: installed_tool("fresh-tool", "2.0.0"),
            },
        ],
    );

    assert_eq!(
        result,
        Err(DomainError::DuplicatePlanItemId("same-id".to_owned()))
    );
}

#[test]
fn update_candidate_represents_target_and_execution_eligibility() {
    let candidate = UpdateCandidate::new(
        ToolId::new("pnpm:not-executable").expect("valid tool id"),
        PackageName::new("not-executable").expect("valid package name"),
        VersionText::new("1.0.0").expect("valid installed version"),
        VersionText::new("1.2.0").expect("valid target version"),
        VersionScheme::SemVer,
        ExecutionEligibility::NotExecutable,
    );

    assert_eq!(candidate.package_name.as_str(), "not-executable");
    assert_eq!(
        candidate.execution_eligibility,
        ExecutionEligibility::NotExecutable
    );
}
