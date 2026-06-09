use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use upgate_domain::{
    AuditFinding, AuditLookupResult, AuditPackageName, AuditQuery, AuditSubject, BlockReason,
    CandidateAuditFact, ExecutionSupport, InstalledTool, ManagerId, ManagerMetadata,
    ManagerSelectedTarget, ManagerUpdateInput, OsvEcosystem, PackageName, PlanItem, PlanItemId,
    ReleaseEntry, ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp,
    TargetAgeEvidence, TargetAgeLookupResult, ToolId, ToolName, UpdateSeed, VersionPolicy,
    VersionScheme, VersionText,
};
use upgate_planning::{derive_audit_queries, evaluate_seed, evaluate_seed_with_audit};

const NOW_SECS: u64 = 1_800_000_000;

fn manager_id() -> ManagerId {
    ManagerId::new("test-manager").expect("valid manager id")
}

fn item_id(value: &str) -> PlanItemId {
    PlanItemId::new(value).expect("valid plan item id")
}

fn version(value: &str) -> VersionText {
    VersionText::new(value).expect("valid version")
}

fn installed_tool(package: &str, installed_version: &str) -> InstalledTool {
    InstalledTool::new(
        manager_id(),
        ToolId::new(format!("test-manager:{package}")).expect("valid tool id"),
        PackageName::new(package).expect("valid package name"),
        ToolName::new(package).expect("valid tool name"),
        version(installed_version),
        ManagerMetadata::empty(),
    )
}

fn installed_tool_with_audit(package: &str, installed_version: &str) -> InstalledTool {
    installed_tool(package, installed_version).with_audit_subject(audit_subject(package))
}

fn audit_subject(package: &str) -> AuditSubject {
    AuditSubject::new(
        OsvEcosystem::Npm,
        AuditPackageName::new(package).expect("valid audit package name"),
    )
}

fn audit_query(package: &str, version: &str) -> AuditQuery {
    AuditQuery::new(audit_subject(package), self::version(version))
}

fn finding(id: &str) -> AuditFinding {
    AuditFinding {
        id: id.to_owned(),
        aliases: Vec::new(),
        summary: None,
        severity: None,
        references: Vec::new(),
    }
}

fn manager_selected_seed(
    package: &str,
    installed_version: &str,
    target_version: &str,
    target_age: TargetAgeLookupResult,
) -> UpdateSeed {
    UpdateSeed::manager_selected(
        installed_tool(package, installed_version),
        ManagerSelectedTarget::new(version(target_version), target_age),
        VersionScheme::SemVer,
        ExecutionSupport::native_or_exact(),
    )
}

#[test]
fn advisory_latest_does_not_replace_manager_selected_target() {
    let selected_target = ManagerSelectedTarget::new(
        version("1.1.0"),
        TargetAgeLookupResult::Known(TargetAgeEvidence::PublishedAt(ReleaseTimestamp::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 10_000),
        ))),
    )
    .with_advisory_release_lookup(
        version("1.2.0"),
        ReleaseLookupResult::Known(ReleaseTimeline::new(vec![ReleaseEntry::new(
            version("1.2.0"),
            ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 20_000)),
        )])),
    );
    let seed = UpdateSeed::manager_selected(
        installed_tool("alpha", "1.0.0"),
        selected_target,
        VersionScheme::SemVer,
        ExecutionSupport::native_or_exact(),
    );

    let item = evaluate_seed(
        item_id("item"),
        seed,
        VersionPolicy::None,
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS),
        Duration::from_secs(0),
    );

    let PlanItem::Update { candidate, .. } = item else {
        panic!("expected update")
    };
    assert_eq!(
        candidate.target_version().expect("known target").as_str(),
        "1.1.0"
    );
}

#[test]
fn manager_selected_target_missing_required_evidence_blocks_the_item() {
    let seed = manager_selected_seed(
        "alpha",
        "1.0.0",
        "1.1.0",
        TargetAgeLookupResult::MissingMetadata,
    );

    let item = evaluate_seed(
        item_id("item"),
        seed,
        VersionPolicy::None,
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS),
        Duration::from_secs(0),
    );

    assert!(matches!(
        item,
        PlanItem::Blocked {
            reason: BlockReason::MissingReleaseMetadata,
            ..
        }
    ));
}

#[test]
fn advisory_lookup_failure_is_non_blocking_diagnostic_for_manager_selected_target() {
    let seed = UpdateSeed::manager_selected(
        installed_tool("alpha", "1.0.0"),
        ManagerSelectedTarget::new(
            version("1.1.0"),
            TargetAgeLookupResult::Known(TargetAgeEvidence::PublishedAt(ReleaseTimestamp::new(
                SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 10_000),
            ))),
        )
        .with_advisory_lookup_failure(ReleaseLookupError::new("mise outdated failed")),
        VersionScheme::SemVer,
        ExecutionSupport::native_or_exact(),
    );

    let item = evaluate_seed(
        item_id("item"),
        seed,
        VersionPolicy::None,
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS),
        Duration::from_secs(0),
    );

    let PlanItem::Update { candidate, .. } = item else {
        panic!("expected update")
    };
    assert_eq!(
        candidate.target_version().expect("known target").as_str(),
        "1.1.0"
    );
    assert_eq!(
        candidate
            .diagnostics
            .advisory_lookup_failure
            .as_ref()
            .expect("advisory lookup failure should be retained")
            .detail,
        "mise outdated failed"
    );
}

#[test]
fn planner_preserves_manager_produced_item_execution_support() {
    let seed = UpdateSeed::new(
        installed_tool("alpha", "1.0.0"),
        version("1.1.0"),
        VersionScheme::SemVer,
        ReleaseLookupResult::Known(ReleaseTimeline::new(vec![ReleaseEntry::new(
            version("1.1.0"),
            ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 86_400)),
        )])),
        ExecutionSupport::exact_only(),
    );

    let PlanItem::Update { candidate, .. } = evaluate_seed(
        item_id("item"),
        seed,
        VersionPolicy::Stable,
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS),
        Duration::from_secs(0),
    ) else {
        panic!("expected update")
    };

    assert_eq!(candidate.execution_support, ExecutionSupport::exact_only());
}

#[test]
fn planner_selects_publish_date_newest_target_before_version_newest_target() {
    let seed = UpdateSeed::new(
        installed_tool("alpha", "3.0.0"),
        version("4.0.0-alpha.13"),
        VersionScheme::SemVer,
        ReleaseLookupResult::Known(ReleaseTimeline::new(vec![
            ReleaseEntry::new(
                version("4.0.0-alpha.13"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 200)),
            ),
            ReleaseEntry::new(
                version("3.9.0"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 100)),
            ),
        ])),
        ExecutionSupport::exact_only(),
    );

    let PlanItem::Update { candidate, .. } = evaluate_seed(
        item_id("item"),
        seed,
        VersionPolicy::None,
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS),
        Duration::ZERO,
    ) else {
        panic!("expected update")
    };

    assert_eq!(
        candidate.target_version().expect("known target").as_str(),
        "3.9.0"
    );
    assert_eq!(
        candidate
            .diagnostics
            .latest_overall
            .expect("latest overall")
            .version
            .as_str(),
        "3.9.0"
    );
    assert_eq!(
        candidate.diagnostics.candidates[0].version.as_str(),
        "4.0.0-alpha.13"
    );
}

#[test]
fn planner_age_gate_selects_publish_date_newest_eligible_target() {
    let seed = UpdateSeed::new(
        installed_tool("alpha", "3.0.0"),
        version("4.0.0-alpha.13"),
        VersionScheme::SemVer,
        ReleaseLookupResult::Known(ReleaseTimeline::new(vec![
            ReleaseEntry::new(
                version("4.0.0-alpha.13"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 300)),
            ),
            ReleaseEntry::new(
                version("3.9.0"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 200)),
            ),
            ReleaseEntry::new(
                version("3.10.0"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 10)),
            ),
        ])),
        ExecutionSupport::exact_only(),
    );

    let PlanItem::Update { candidate, .. } = evaluate_seed(
        item_id("item"),
        seed,
        VersionPolicy::None,
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS),
        Duration::from_secs(100),
    ) else {
        panic!("expected update")
    };

    assert_eq!(
        candidate.target_version().expect("known target").as_str(),
        "3.9.0"
    );
    assert_eq!(
        candidate
            .diagnostics
            .latest_overall
            .expect("latest overall")
            .version
            .as_str(),
        "3.10.0"
    );
    assert_eq!(
        candidate
            .diagnostics
            .latest_age_eligible
            .expect("latest age eligible")
            .version
            .as_str(),
        "3.9.0"
    );
}

#[test]
fn audit_queries_include_all_picker_candidates_for_planner_selectable_timelines() {
    let seed = UpdateSeed::new(
        installed_tool_with_audit("alpha", "1.0.0"),
        version("3.0.0-alpha.1"),
        VersionScheme::SemVer,
        ReleaseLookupResult::Known(ReleaseTimeline::new(vec![
            ReleaseEntry::new(
                version("1.1.0"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 500)),
            ),
            ReleaseEntry::new(
                version("2.0.0"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 10)),
            ),
            ReleaseEntry::new(
                version("3.0.0-alpha.1"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 500)),
            ),
        ])),
        ExecutionSupport::exact_only(),
    );

    let queries = derive_audit_queries(&[ManagerUpdateInput::Seed(seed)]);

    assert_eq!(
        queries,
        vec![
            audit_query("alpha", "1.1.0"),
            audit_query("alpha", "2.0.0"),
            audit_query("alpha", "3.0.0-alpha.1"),
        ]
    );
}

#[test]
fn planner_attaches_audit_facts_to_policy_and_age_blocked_picker_candidates() {
    let seed = UpdateSeed::new(
        installed_tool_with_audit("alpha", "1.0.0"),
        version("3.0.0-alpha.1"),
        VersionScheme::SemVer,
        ReleaseLookupResult::Known(ReleaseTimeline::new(vec![
            ReleaseEntry::new(
                version("1.1.0"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 500)),
            ),
            ReleaseEntry::new(
                version("2.0.0"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 10)),
            ),
            ReleaseEntry::new(
                version("3.0.0-alpha.1"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 500)),
            ),
        ])),
        ExecutionSupport::exact_only(),
    );
    let audit_results = BTreeMap::from([
        (audit_query("alpha", "1.1.0"), AuditLookupResult::Clean),
        (
            audit_query("alpha", "2.0.0"),
            AuditLookupResult::LookupFailed {
                detail: "OSV unavailable".to_owned(),
            },
        ),
        (
            audit_query("alpha", "3.0.0-alpha.1"),
            AuditLookupResult::Vulnerable {
                findings: vec![finding("GHSA-alpha")],
            },
        ),
    ]);

    let PlanItem::Update { candidate, .. } = evaluate_seed_with_audit(
        item_id("item"),
        seed,
        VersionPolicy::Stable,
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS),
        Duration::from_secs(100),
        &audit_results,
    ) else {
        panic!("expected clean older update")
    };

    assert_eq!(
        candidate.target_version().expect("known target").as_str(),
        "1.1.0"
    );
    let too_fresh = candidate
        .diagnostics
        .candidates
        .iter()
        .find(|candidate| candidate.version.as_str() == "2.0.0")
        .expect("too-fresh candidate");
    assert!(matches!(
        too_fresh.audit,
        Some(CandidateAuditFact::LookupFailed { .. })
    ));
    let policy_blocked = candidate
        .diagnostics
        .candidates
        .iter()
        .find(|candidate| candidate.version.as_str() == "3.0.0-alpha.1")
        .expect("policy-blocked candidate");
    assert!(matches!(
        policy_blocked.audit,
        Some(CandidateAuditFact::Vulnerable { .. })
    ));
}

#[test]
fn audit_block_records_the_specific_blocked_candidate_version() {
    let seed = UpdateSeed::new(
        installed_tool_with_audit("alpha", "1.0.0"),
        version("3.0.0"),
        VersionScheme::SemVer,
        ReleaseLookupResult::Known(ReleaseTimeline::new(vec![
            ReleaseEntry::new(
                version("2.0.0"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 500)),
            ),
            ReleaseEntry::new(
                version("3.0.0"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 10)),
            ),
        ])),
        ExecutionSupport::exact_only(),
    );
    let audit_results = BTreeMap::from([
        (
            audit_query("alpha", "2.0.0"),
            AuditLookupResult::Vulnerable {
                findings: vec![finding("GHSA-alpha")],
            },
        ),
        (audit_query("alpha", "3.0.0"), AuditLookupResult::Clean),
    ]);

    let PlanItem::Blocked { diagnostics, .. } = evaluate_seed_with_audit(
        item_id("item"),
        seed,
        VersionPolicy::None,
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS),
        Duration::from_secs(100),
        &audit_results,
    ) else {
        panic!("expected audit block")
    };

    assert_eq!(
        diagnostics
            .audit_blocking_candidate
            .expect("audit blocking candidate")
            .version
            .as_str(),
        "2.0.0"
    );
}

#[test]
fn manager_selected_policy_blocked_target_keeps_audit_fact_for_picker() {
    let seed = manager_selected_seed(
        "alpha",
        "1.0.0",
        "2.0.0-alpha.1",
        TargetAgeLookupResult::Known(TargetAgeEvidence::PublishedAt(ReleaseTimestamp::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 500),
        ))),
    );
    let seed = UpdateSeed {
        installed: seed.installed.with_audit_subject(audit_subject("alpha")),
        ..seed
    };
    let audit_results = BTreeMap::from([(
        audit_query("alpha", "2.0.0-alpha.1"),
        AuditLookupResult::Vulnerable {
            findings: vec![finding("GHSA-alpha")],
        },
    )]);

    let PlanItem::Blocked { diagnostics, .. } = evaluate_seed_with_audit(
        item_id("item"),
        seed,
        VersionPolicy::Stable,
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS),
        Duration::ZERO,
        &audit_results,
    ) else {
        panic!("expected policy block")
    };

    assert!(matches!(
        diagnostics.candidates[0].audit,
        Some(CandidateAuditFact::Vulnerable { .. })
    ));
}

#[test]
fn pep440_timeline_skips_unparseable_entries() {
    let seed = UpdateSeed::new(
        installed_tool("alpha", "1.0.0"),
        version("1.2.0"),
        VersionScheme::Pep440,
        ReleaseLookupResult::Known(ReleaseTimeline::new(vec![
            ReleaseEntry::new(
                version("not-a-version"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 300)),
            ),
            ReleaseEntry::new(
                version("1.2.0"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 200)),
            ),
        ])),
        ExecutionSupport::exact_only(),
    );

    let PlanItem::Update { candidate, .. } = evaluate_seed(
        item_id("item"),
        seed,
        VersionPolicy::None,
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS),
        Duration::ZERO,
    ) else {
        panic!("expected update despite unparseable timeline entry")
    };

    assert_eq!(
        candidate.target_version().expect("known target").as_str(),
        "1.2.0"
    );
}

#[test]
fn pep440_age_gate_selects_publish_date_newest_eligible_target() {
    let seed = UpdateSeed::new(
        installed_tool("alpha", "3.0.0"),
        version("3.10.0"),
        VersionScheme::Pep440,
        ReleaseLookupResult::Known(ReleaseTimeline::new(vec![
            ReleaseEntry::new(
                version("3.9.0"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 200)),
            ),
            ReleaseEntry::new(
                version("3.10.0"),
                ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 10)),
            ),
        ])),
        ExecutionSupport::exact_only(),
    );

    let PlanItem::Update { candidate, .. } = evaluate_seed(
        item_id("item"),
        seed,
        VersionPolicy::None,
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS),
        Duration::from_secs(100),
    ) else {
        panic!("expected update")
    };

    assert_eq!(
        candidate.target_version().expect("known target").as_str(),
        "3.9.0"
    );
    assert_eq!(
        candidate
            .diagnostics
            .latest_overall
            .expect("latest overall")
            .version
            .as_str(),
        "3.10.0"
    );
}
