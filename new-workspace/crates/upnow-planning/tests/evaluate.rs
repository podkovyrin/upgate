use std::time::{Duration, SystemTime};

use upnow_domain::{
    BlockReason, ExecutionEligibility, InstalledTool, ManagerId, ManagerMetadata,
    ManagerSelectedTarget, PackageName, PlanItem, PlanItemId, ReleaseEntry, ReleaseLookupResult,
    ReleaseTimeline, ReleaseTimestamp, TargetAgeEvidence, TargetAgeLookupResult, ToolId, ToolName,
    UpdateSeed, VersionPolicy, VersionScheme, VersionText,
};
use upnow_planning::evaluate_seed;

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
        ExecutionEligibility::NativeOrExact,
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
        ExecutionEligibility::NativeOrExact,
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
    assert_eq!(candidate.target_version.as_str(), "1.1.0");
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
fn planner_preserves_manager_produced_item_execution_eligibility() {
    let seed = UpdateSeed::new(
        installed_tool("alpha", "1.0.0"),
        version("1.1.0"),
        VersionScheme::SemVer,
        ReleaseLookupResult::Known(ReleaseTimeline::new(vec![ReleaseEntry::new(
            version("1.1.0"),
            ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 86_400)),
        )])),
        ExecutionEligibility::ExactOnly,
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

    assert_eq!(
        candidate.execution_eligibility,
        ExecutionEligibility::ExactOnly
    );
}
