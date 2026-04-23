use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};
use semver::Version;

use super::policy::{
    GateBypass, OrderedCandidate, VersionPolicy, VersionPolicyResolution, classify_semver_release,
    evaluate_candidates,
};
use crate::util::time::parse_rfc3339_unix;

#[derive(Debug, Clone)]
pub struct SemverTimestamp {
    pub version: String,
    pub published_unix: u64,
}

pub fn resolve_semver_with_min_age(
    current: &str,
    releases: &[SemverTimestamp],
    now_unix_secs: u64,
    min_age: Duration,
    version_policy: VersionPolicy,
) -> Result<VersionPolicyResolution> {
    // Semver support is intentionally shared at manager-common level for ecosystem managers
    // that represent release timelines as semver + publish timestamp. Manager-local data fetch/
    // parse remains self-contained.
    let current_ver = Version::parse(current)
        .with_context(|| format!("failed to parse current semver: {current}"))?;
    let installed_class = classify_semver_release(current);
    let candidates = releases
        .iter()
        .filter_map(|item| {
            Version::parse(&item.version)
                .ok()
                .map(|parsed| OrderedCandidate {
                    version: item.version.clone(),
                    parsed,
                    release_class: classify_semver_release(&item.version),
                    published_unix: item.published_unix,
                })
        })
        .collect::<Vec<_>>();

    let resolution = evaluate_candidates(
        &current_ver,
        &candidates,
        installed_class,
        version_policy,
        now_unix_secs,
        min_age,
        GateBypass::NONE,
    );

    Ok(resolution)
}

pub fn release_age_secs_for_version(
    releases: &[SemverTimestamp],
    version: &str,
    now_unix_secs: u64,
) -> Option<u64> {
    releases
        .iter()
        .find(|item| item.version == version)
        .map(|item| now_unix_secs.saturating_sub(item.published_unix))
}

pub fn parse_semver_time_releases(
    source: &str,
    package: &str,
    timestamps_by_version: &BTreeMap<String, String>,
) -> Result<Vec<SemverTimestamp>> {
    let mut releases = Vec::new();

    for (ver_str, ts_raw) in timestamps_by_version {
        if ver_str == "created" || ver_str == "modified" {
            continue;
        }

        let ts = parse_rfc3339_unix(ts_raw).with_context(|| {
            format!("invalid {source} timestamp for {package}@{ver_str}: {ts_raw}")
        })?;

        releases.push(SemverTimestamp {
            version: ver_str.clone(),
            published_unix: ts,
        });
    }

    Ok(releases)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::shared::versioning::policy::{PolicyWarning, RecommendedOutcome};

    #[test]
    fn keeps_current_when_installed_version_is_newer_than_registry_latest() {
        let now = 1_800_000_000;
        let releases = vec![SemverTimestamp {
            version: "1.9.9".to_string(),
            published_unix: now - 3600,
        }];

        let resolved = resolve_semver_with_min_age(
            "2.0.0",
            &releases,
            now,
            Duration::from_secs(7 * 24 * 60 * 60),
            VersionPolicy::Disabled,
        )
        .expect("resolution should succeed");

        assert_eq!(resolved.recommendation, RecommendedOutcome::CurrentNoNewer);
        assert_eq!(resolved.latest_policy_eligible_version, None);
        assert_eq!(resolved.configured_policy(), None);
        assert_eq!(resolved.latest_blocked_by_policy_version(), None);
        assert_eq!(resolved.version_policy_warning(), None);
    }

    #[test]
    fn keeps_current_when_current_equals_latest_but_release_is_too_fresh() {
        let now = 1_800_000_000;
        let releases = vec![SemverTimestamp {
            version: "1.0.0".to_string(),
            published_unix: now - 60,
        }];

        let resolved = resolve_semver_with_min_age(
            "1.0.0",
            &releases,
            now,
            Duration::from_secs(7 * 24 * 60 * 60),
            VersionPolicy::Disabled,
        )
        .expect("resolution should succeed");

        assert_eq!(resolved.recommendation, RecommendedOutcome::CurrentNoNewer);
        assert_eq!(resolved.latest_policy_eligible_version, None);
        assert_eq!(resolved.configured_policy(), None);
        assert_eq!(resolved.latest_blocked_by_policy_version(), None);
        assert_eq!(resolved.version_policy_warning(), None);
    }

    #[test]
    fn applies_version_policy_before_age_gate_for_selection() {
        let now = 1_800_000_000;
        let releases = vec![
            SemverTimestamp {
                version: "1.3.0-beta.1".to_string(),
                published_unix: now - 20 * 24 * 60 * 60,
            },
            SemverTimestamp {
                version: "1.2.5".to_string(),
                published_unix: now - 2 * 24 * 60 * 60,
            },
        ];

        let resolved = resolve_semver_with_min_age(
            "1.2.0",
            &releases,
            now,
            Duration::from_secs(7 * 24 * 60 * 60),
            VersionPolicy::Stable,
        )
        .expect("resolution should succeed");

        assert_eq!(resolved.recommendation, RecommendedOutcome::DelayedByAge);
        assert_eq!(
            resolved.latest_policy_eligible_version.as_deref(),
            Some("1.2.5")
        );
        assert_eq!(resolved.configured_policy(), Some(VersionPolicy::Stable));
        assert_eq!(
            resolved.latest_blocked_by_policy_version(),
            Some("1.3.0-beta.1")
        );
        assert_eq!(resolved.version_policy_warning(), None);
    }

    #[test]
    fn keeps_current_when_newer_versions_exist_but_all_blocked_by_policy() {
        let now = 1_800_000_000;
        let releases = vec![SemverTimestamp {
            version: "1.3.0-beta.1".to_string(),
            published_unix: now - 20 * 24 * 60 * 60,
        }];

        let resolved = resolve_semver_with_min_age(
            "1.2.0",
            &releases,
            now,
            Duration::from_secs(0),
            VersionPolicy::Stable,
        )
        .expect("resolution should succeed");

        assert_eq!(
            resolved.recommendation,
            RecommendedOutcome::CurrentBlockedByPolicy
        );
        assert_eq!(resolved.latest_policy_eligible_version, None);
        assert_eq!(resolved.configured_policy(), Some(VersionPolicy::Stable));
        assert_eq!(
            resolved.latest_blocked_by_policy_version(),
            Some("1.3.0-beta.1")
        );
        assert_eq!(resolved.version_policy_warning(), None);
    }

    #[test]
    fn same_track_unknown_installed_track_sets_fallback_warning() {
        let now = 1_800_000_000;
        let releases = vec![SemverTimestamp {
            version: "1.1.0-beta.1".to_string(),
            published_unix: now - 20 * 24 * 60 * 60,
        }];

        let resolved = resolve_semver_with_min_age(
            "1.0.0-foo.1",
            &releases,
            now,
            Duration::from_secs(0),
            VersionPolicy::SameTrack,
        )
        .expect("resolution should succeed");

        assert_eq!(
            resolved.version_policy_warning(),
            Some(PolicyWarning::InstalledTrackUnknownFallbackStable)
        );
    }
}
