use std::time::{Duration, SystemTime};

use upnow_domain::{
    BlockReason, DelayReason, ExecutionEligibility, InstalledTool, ManagerId, ManagerMetadata,
    PackageName, PlanItem, PlanItemId, PolicyBlockReason, PolicyWarning, ReleaseEntry,
    ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp, ToolId, ToolName,
    UpdateSeed, VersionPolicy, VersionScheme, VersionText,
};
use upnow_planning::{evaluate_brew_seed, evaluate_seed};

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

fn known_seed(
    package: &str,
    installed_version: &str,
    discovered_target: &str,
    version_scheme: VersionScheme,
    releases: Vec<(&str, u64)>,
) -> UpdateSeed {
    UpdateSeed::new(
        installed_tool(package, installed_version),
        version(discovered_target),
        version_scheme,
        ReleaseLookupResult::Known(ReleaseTimeline::new(
            releases
                .into_iter()
                .map(|(release, age_secs)| {
                    ReleaseEntry::new(
                        version(release),
                        ReleaseTimestamp::new(
                            SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - age_secs),
                        ),
                    )
                })
                .collect(),
        )),
    )
}

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS)
}

fn evaluate(seed: UpdateSeed, policy: VersionPolicy, min_age_secs: u64) -> PlanItem {
    evaluate_seed(
        item_id("item"),
        seed,
        policy,
        now(),
        Duration::from_secs(min_age_secs),
        ExecutionEligibility::NativeOrExact,
    )
}

fn evaluate_brew(seed: UpdateSeed, policy: VersionPolicy, min_age_secs: u64) -> PlanItem {
    evaluate_brew_seed(
        item_id("item"),
        seed,
        policy,
        now(),
        Duration::from_secs(min_age_secs),
        ExecutionEligibility::NativeOrExact,
    )
}

#[test]
fn semver_candidates_are_ordered_before_selecting_update() {
    let item = evaluate(
        known_seed(
            "alpha",
            "1.0.0",
            "2.0.0",
            VersionScheme::SemVer,
            vec![("1.5.0", 10_000), ("2.0.0", 10_000), ("1.9.0", 10_000)],
        ),
        VersionPolicy::None,
        0,
    );

    let PlanItem::Update { candidate, .. } = item else {
        panic!("expected update");
    };
    assert_eq!(candidate.target_version.as_str(), "2.0.0");
}

#[test]
fn pep440_candidates_are_ordered_before_selecting_update() {
    let item = evaluate(
        known_seed(
            "alpha",
            "1.0.0",
            "1.10.0",
            VersionScheme::Pep440,
            vec![("1.9.0", 10_000), ("1.10.0", 10_000), ("1.2.0", 10_000)],
        ),
        VersionPolicy::None,
        0,
    );

    let PlanItem::Update { candidate, .. } = item else {
        panic!("expected update");
    };
    assert_eq!(candidate.target_version.as_str(), "1.10.0");
}

#[test]
fn semver_target_metadata_uses_parsed_version_equality() {
    let item = evaluate(
        known_seed(
            "alpha",
            "1.0.0",
            "v1.2.0",
            VersionScheme::SemVer,
            vec![("1.2.0", 10_000)],
        ),
        VersionPolicy::None,
        0,
    );

    let PlanItem::Update { candidate, .. } = item else {
        panic!("expected update");
    };
    assert_eq!(candidate.target_version.as_str(), "1.2.0");
}

#[test]
fn pep440_target_metadata_uses_parsed_version_equality() {
    let item = evaluate(
        known_seed(
            "alpha",
            "0.9",
            "1.0.0",
            VersionScheme::Pep440,
            vec![("1.0", 10_000)],
        ),
        VersionPolicy::None,
        0,
    );

    let PlanItem::Update { candidate, .. } = item else {
        panic!("expected update");
    };
    assert_eq!(candidate.target_version.as_str(), "1.0");
}

#[test]
fn brew_native_policy_treats_clear_prerelease_markers_as_unstable() {
    let item = evaluate_brew(
        known_seed(
            "brew-tool",
            "1.0.0",
            "1.2.0-beta.1,123",
            VersionScheme::ManagerNative,
            vec![("1.2.0-beta.1,123", 10_000)],
        ),
        VersionPolicy::Stable,
        0,
    );

    assert!(matches!(
        item,
        PlanItem::Blocked {
            reason: BlockReason::VersionPolicy(PolicyBlockReason::PreReleaseBlocked),
            ..
        }
    ));
}

#[test]
fn brew_missing_release_metadata_blocks_the_item() {
    let seed = UpdateSeed::new(
        installed_tool("brew-tool", "1.0.0"),
        version("1.2.0"),
        VersionScheme::ManagerNative,
        ReleaseLookupResult::MissingMetadata,
    );

    assert!(matches!(
        evaluate_brew(seed, VersionPolicy::Stable, 0),
        PlanItem::Blocked {
            reason: BlockReason::MissingReleaseMetadata,
            ..
        }
    ));
}

#[test]
fn brew_failed_release_lookup_blocks_the_item() {
    let seed = UpdateSeed::new(
        installed_tool("brew-tool", "1.0.0"),
        version("1.2.0"),
        VersionScheme::ManagerNative,
        ReleaseLookupResult::LookupFailed(ReleaseLookupError::new("tap lookup failed")),
    );

    assert!(matches!(
        evaluate_brew(seed, VersionPolicy::Stable, 0),
        PlanItem::Blocked {
            reason: BlockReason::ReleaseLookupFailed,
            ..
        }
    ));
}

#[test]
fn generic_manager_native_is_not_evaluated_as_brew() {
    let item = evaluate(
        known_seed(
            "native-tool",
            "1.0.0",
            "1.2.0",
            VersionScheme::ManagerNative,
            vec![("1.2.0", 10_000)],
        ),
        VersionPolicy::Stable,
        0,
    );

    assert!(matches!(item, PlanItem::ResolverError { .. }));
}

#[test]
fn stable_policy_blocks_prerelease_candidates() {
    let item = evaluate(
        known_seed(
            "alpha",
            "1.0.0",
            "1.1.0-beta.1",
            VersionScheme::SemVer,
            vec![("1.1.0-beta.1", 10_000)],
        ),
        VersionPolicy::Stable,
        0,
    );

    assert!(matches!(
        item,
        PlanItem::Blocked {
            reason: BlockReason::VersionPolicy(PolicyBlockReason::PreReleaseBlocked),
            ..
        }
    ));
}

#[test]
fn stable_policy_blocks_unknown_semver_prerelease_candidates() {
    let item = evaluate(
        known_seed(
            "alpha",
            "1.0.0",
            "1.1.0-foo.1",
            VersionScheme::SemVer,
            vec![("1.1.0-foo.1", 10_000)],
        ),
        VersionPolicy::Stable,
        0,
    );

    assert!(matches!(
        item,
        PlanItem::Blocked {
            reason: BlockReason::VersionPolicy(PolicyBlockReason::PreReleaseBlocked),
            ..
        }
    ));
}

#[test]
fn stable_policy_allows_semver_build_metadata_as_final() {
    let item = evaluate(
        known_seed(
            "alpha",
            "1.0.0",
            "1.1.0+build.7",
            VersionScheme::SemVer,
            vec![("1.1.0+build.7", 10_000)],
        ),
        VersionPolicy::Stable,
        0,
    );

    let PlanItem::Update { candidate, .. } = item else {
        panic!("expected update");
    };
    assert_eq!(candidate.target_version.as_str(), "1.1.0+build.7");
}

#[test]
fn same_track_allows_same_or_more_stable_candidates() {
    let item = evaluate(
        known_seed(
            "alpha",
            "1.0.0-beta.1",
            "1.0.0-rc.1",
            VersionScheme::SemVer,
            vec![("1.0.0-alpha.1", 10_000), ("1.0.0-rc.1", 10_000)],
        ),
        VersionPolicy::SameTrack,
        0,
    );

    let PlanItem::Update { candidate, .. } = item else {
        panic!("expected update");
    };
    assert_eq!(candidate.target_version.as_str(), "1.0.0-rc.1");
}

#[test]
fn same_track_blocks_track_regressions() {
    let item = evaluate(
        known_seed(
            "alpha",
            "1.0.0-rc.1",
            "1.0.1-beta.1",
            VersionScheme::SemVer,
            vec![("1.0.1-beta.1", 10_000)],
        ),
        VersionPolicy::SameTrack,
        0,
    );

    assert!(matches!(
        item,
        PlanItem::Blocked {
            reason: BlockReason::VersionPolicy(PolicyBlockReason::TrackRegression),
            ..
        }
    ));
}

#[test]
fn same_track_blocks_unknown_candidate_stability_when_installed_track_is_known() {
    let item = evaluate(
        known_seed(
            "alpha",
            "1.0.0-beta.1",
            "1.1.0-foo.1",
            VersionScheme::SemVer,
            vec![("1.1.0-foo.1", 10_000)],
        ),
        VersionPolicy::SameTrack,
        0,
    );

    assert!(matches!(
        item,
        PlanItem::Blocked {
            reason: BlockReason::VersionPolicy(PolicyBlockReason::UnknownStability),
            ..
        }
    ));
}

#[test]
fn unknown_installed_track_falls_back_to_stable_with_typed_warning() {
    let item = evaluate_brew(
        known_seed(
            "brew-tool",
            "latest",
            "1.2.0",
            VersionScheme::ManagerNative,
            vec![("1.2.0", 10_000)],
        ),
        VersionPolicy::SameTrack,
        0,
    );

    let PlanItem::Update { candidate, .. } = item else {
        panic!("expected update");
    };
    assert_eq!(
        candidate.policy_warnings,
        vec![PolicyWarning::InstalledTrackUnknownFallbackStable]
    );
}

#[test]
fn unknown_semver_installed_track_falls_back_to_stable_with_typed_warning() {
    let item = evaluate(
        known_seed(
            "alpha",
            "1.0.0-foo.1",
            "1.1.0",
            VersionScheme::SemVer,
            vec![("1.1.0", 10_000)],
        ),
        VersionPolicy::SameTrack,
        0,
    );

    let PlanItem::Update { candidate, .. } = item else {
        panic!("expected update");
    };
    assert_eq!(
        candidate.policy_warnings,
        vec![PolicyWarning::InstalledTrackUnknownFallbackStable]
    );
}

#[test]
fn blocked_same_track_fallback_keeps_typed_warning() {
    let item = evaluate_brew(
        known_seed(
            "brew-tool",
            "latest",
            "1.2.0-beta.1",
            VersionScheme::ManagerNative,
            vec![("1.2.0-beta.1", 10_000)],
        ),
        VersionPolicy::SameTrack,
        0,
    );

    let PlanItem::Blocked {
        reason,
        policy_warnings,
        ..
    } = item
    else {
        panic!("expected blocked item");
    };
    assert_eq!(
        reason,
        BlockReason::VersionPolicy(PolicyBlockReason::PreReleaseBlocked)
    );
    assert_eq!(
        policy_warnings,
        vec![PolicyWarning::InstalledTrackUnknownFallbackStable]
    );
}

#[test]
fn no_policy_mode_applies_no_stability_filter() {
    let item = evaluate(
        known_seed(
            "alpha",
            "1.0.0",
            "1.1.0-beta.1",
            VersionScheme::SemVer,
            vec![("1.1.0-beta.1", 10_000)],
        ),
        VersionPolicy::None,
        0,
    );

    let PlanItem::Update { candidate, .. } = item else {
        panic!("expected update");
    };
    assert_eq!(candidate.target_version.as_str(), "1.1.0-beta.1");
}

#[test]
fn too_fresh_policy_eligible_versions_are_delayed() {
    let item = evaluate(
        known_seed(
            "alpha",
            "1.0.0",
            "1.1.0",
            VersionScheme::SemVer,
            vec![("1.1.0", 60)],
        ),
        VersionPolicy::Stable,
        3_600,
    );

    assert!(matches!(
        item,
        PlanItem::Delayed {
            reason: DelayReason::ReleaseTooFresh,
            ..
        }
    ));
}

#[test]
fn missing_release_metadata_blocks_the_item() {
    let seed = UpdateSeed::new(
        installed_tool("alpha", "1.0.0"),
        version("1.1.0"),
        VersionScheme::SemVer,
        ReleaseLookupResult::MissingMetadata,
    );

    assert!(matches!(
        evaluate(seed, VersionPolicy::Stable, 0),
        PlanItem::Blocked {
            reason: BlockReason::MissingReleaseMetadata,
            ..
        }
    ));
}

#[test]
fn failed_release_lookup_blocks_the_item() {
    let seed = UpdateSeed::new(
        installed_tool("alpha", "1.0.0"),
        version("1.1.0"),
        VersionScheme::SemVer,
        ReleaseLookupResult::LookupFailed(ReleaseLookupError::new("registry timeout")),
    );

    assert!(matches!(
        evaluate(seed, VersionPolicy::Stable, 0),
        PlanItem::Blocked {
            reason: BlockReason::ReleaseLookupFailed,
            ..
        }
    ));
}

#[test]
fn current_is_returned_when_no_newer_candidate_exists() {
    let item = evaluate(
        known_seed(
            "alpha",
            "2.0.0",
            "1.9.0",
            VersionScheme::SemVer,
            vec![("1.9.0", 10_000), ("2.0.0", 10_000)],
        ),
        VersionPolicy::None,
        0,
    );

    assert!(matches!(item, PlanItem::Current { .. }));
}

#[test]
fn parse_failures_become_resolver_errors() {
    let item = evaluate(
        known_seed(
            "alpha",
            "not-semver",
            "1.1.0",
            VersionScheme::SemVer,
            vec![("1.1.0", 10_000)],
        ),
        VersionPolicy::None,
        0,
    );

    assert!(matches!(item, PlanItem::ResolverError { .. }));
}

#[test]
fn parse_failures_in_release_timeline_become_resolver_errors() {
    let item = evaluate(
        known_seed(
            "alpha",
            "1.0.0",
            "1.1.0",
            VersionScheme::SemVer,
            vec![("1.1.0", 10_000), ("not-semver", 10_000)],
        ),
        VersionPolicy::None,
        0,
    );

    assert!(matches!(item, PlanItem::ResolverError { .. }));
}

#[test]
fn missing_discovered_target_metadata_blocks_the_item() {
    let item = evaluate(
        known_seed(
            "alpha",
            "1.0.0",
            "2.0.0",
            VersionScheme::SemVer,
            vec![("1.5.0", 10_000)],
        ),
        VersionPolicy::None,
        0,
    );

    assert!(matches!(
        item,
        PlanItem::Blocked {
            reason: BlockReason::MissingReleaseMetadata,
            ..
        }
    ));
}
