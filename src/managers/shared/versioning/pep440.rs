use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use pep440_rs::Version as Pep440Version;

use super::policy::{
    GateBypass, OrderedCandidate, PolicyWarning, RecommendedOutcome, VersionPolicy,
    classify_pep440_release, evaluate_candidates,
};
use crate::util::time::parse_rfc3339_unix;

#[derive(Debug, Clone)]
pub struct Pep440Timestamp {
    pub version: String,
    pub published_unix: u64,
}

#[derive(Debug, Clone)]
pub struct Pep440AgeResolution {
    pub selected_version: Option<String>,
    pub latest_version: Option<String>,
    pub latest_age_secs: Option<u64>,
    pub current_blocked_by_policy: bool,
    pub version_policy: Option<String>,
    pub latest_blocked_by_policy_version: Option<String>,
    pub version_policy_warning: Option<String>,
}

pub fn resolve_pep440_with_min_age(
    current: &str,
    releases: &[Pep440Timestamp],
    now_unix_secs: u64,
    min_age: Duration,
    version_policy: VersionPolicy,
) -> Result<Pep440AgeResolution> {
    let current_ver = Pep440Version::from_str(current)
        .with_context(|| format!("failed to parse current PEP440 version: {current}"))?;
    let installed_class = classify_pep440_release(current);
    let candidates = releases
        .iter()
        .filter_map(|item| {
            Pep440Version::from_str(&item.version)
                .ok()
                .map(|parsed| OrderedCandidate {
                    version: item.version.clone(),
                    parsed,
                    release_class: classify_pep440_release(&item.version),
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

    let (selected_version, current_blocked_by_policy) = match resolution.recommendation {
        RecommendedOutcome::Update { target_version } => (Some(target_version), false),
        RecommendedOutcome::DelayedByAge => (None, false),
        RecommendedOutcome::CurrentNoNewer => (Some(current.to_string()), false),
        RecommendedOutcome::CurrentBlockedByPolicy => (Some(current.to_string()), true),
    };
    let version_policy =
        (version_policy != VersionPolicy::Disabled).then(|| version_policy.as_str().to_string());
    let latest_blocked_by_policy_version = resolution
        .evaluations
        .iter()
        .find(|eval| !eval.policy_allowed)
        .map(|eval| eval.version.clone());
    let version_policy_warning = resolution
        .evaluations
        .iter()
        .find_map(|eval| eval.policy_warning)
        .map(PolicyWarning::as_note)
        .map(str::to_string);

    Ok(Pep440AgeResolution {
        selected_version,
        latest_version: resolution.latest_overall_version,
        latest_age_secs: resolution.latest_overall_age_secs,
        current_blocked_by_policy,
        version_policy,
        latest_blocked_by_policy_version,
        version_policy_warning,
    })
}

pub fn release_age_secs_for_pep440_version(
    releases: &[Pep440Timestamp],
    version: &str,
    now_unix_secs: u64,
) -> Option<u64> {
    releases
        .iter()
        .find(|item| item.version == version)
        .map(|item| now_unix_secs.saturating_sub(item.published_unix))
}

pub fn parse_pep440_release_timestamps<T, F1, F2>(
    package: &str,
    releases_by_version: &BTreeMap<String, Vec<T>>,
    upload_time_iso_8601: F1,
    upload_time: F2,
) -> Result<Vec<Pep440Timestamp>>
where
    F1: Fn(&T) -> Option<&str>,
    F2: Fn(&T) -> Option<&str>,
{
    let mut releases = Vec::new();

    for (ver_str, files) in releases_by_version {
        let mut newest_ts = None::<u64>;
        for file in files {
            let raw = upload_time_iso_8601(file).or_else(|| upload_time(file));

            if let Some(raw) = raw {
                let ts = parse_rfc3339_unix(raw).with_context(|| {
                    format!("invalid upload timestamp for {package}@{ver_str}: {raw}")
                })?;
                newest_ts = Some(newest_ts.map_or(ts, |curr| curr.max(ts)));
            }
        }

        let Some(published_unix) = newest_ts else {
            continue;
        };

        releases.push(Pep440Timestamp {
            version: ver_str.clone(),
            published_unix,
        });
    }

    Ok(releases)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_current_when_installed_version_is_newer_than_registry_latest() {
        let now = 1_800_000_000;
        let releases = vec![Pep440Timestamp {
            version: "1.9.9".to_string(),
            published_unix: now - 3600,
        }];

        let resolved = resolve_pep440_with_min_age(
            "2.0.0",
            &releases,
            now,
            Duration::from_secs(7 * 24 * 60 * 60),
            VersionPolicy::Disabled,
        )
        .expect("resolution should succeed");

        assert_eq!(resolved.selected_version.as_deref(), Some("2.0.0"));
        assert_eq!(resolved.latest_version, None);
        assert!(!resolved.current_blocked_by_policy);
        assert_eq!(resolved.version_policy, None);
        assert_eq!(resolved.latest_blocked_by_policy_version, None);
        assert_eq!(resolved.version_policy_warning, None);
    }

    #[test]
    fn keeps_current_when_current_equals_latest_but_release_is_too_fresh() {
        let now = 1_800_000_000;
        let releases = vec![Pep440Timestamp {
            version: "1.0.0".to_string(),
            published_unix: now - 60,
        }];

        let resolved = resolve_pep440_with_min_age(
            "1.0.0",
            &releases,
            now,
            Duration::from_secs(7 * 24 * 60 * 60),
            VersionPolicy::Disabled,
        )
        .expect("resolution should succeed");

        assert_eq!(resolved.selected_version.as_deref(), Some("1.0.0"));
        assert_eq!(resolved.latest_version, None);
        assert!(!resolved.current_blocked_by_policy);
        assert_eq!(resolved.version_policy, None);
        assert_eq!(resolved.latest_blocked_by_policy_version, None);
        assert_eq!(resolved.version_policy_warning, None);
    }

    #[test]
    fn applies_version_policy_before_age_gate_for_selection() {
        let now = 1_800_000_000;
        let releases = vec![
            Pep440Timestamp {
                version: "1.3.0b1".to_string(),
                published_unix: now - 20 * 24 * 60 * 60,
            },
            Pep440Timestamp {
                version: "1.2.5".to_string(),
                published_unix: now - 2 * 24 * 60 * 60,
            },
        ];

        let resolved = resolve_pep440_with_min_age(
            "1.2.0",
            &releases,
            now,
            Duration::from_secs(7 * 24 * 60 * 60),
            VersionPolicy::Stable,
        )
        .expect("resolution should succeed");

        assert_eq!(resolved.selected_version, None);
        assert_eq!(resolved.latest_version.as_deref(), Some("1.3.0b1"));
        assert!(!resolved.current_blocked_by_policy);
        assert_eq!(resolved.version_policy.as_deref(), Some("stable"));
        assert_eq!(
            resolved.latest_blocked_by_policy_version.as_deref(),
            Some("1.3.0b1")
        );
        assert_eq!(resolved.version_policy_warning, None);
    }

    #[test]
    fn keeps_current_when_newer_versions_exist_but_all_blocked_by_policy() {
        let now = 1_800_000_000;
        let releases = vec![Pep440Timestamp {
            version: "1.3.0b1".to_string(),
            published_unix: now - 20 * 24 * 60 * 60,
        }];

        let resolved = resolve_pep440_with_min_age(
            "1.2.0",
            &releases,
            now,
            Duration::from_secs(0),
            VersionPolicy::Stable,
        )
        .expect("resolution should succeed");

        assert_eq!(resolved.selected_version.as_deref(), Some("1.2.0"));
        assert_eq!(resolved.latest_version.as_deref(), Some("1.3.0b1"));
        assert!(resolved.current_blocked_by_policy);
        assert_eq!(resolved.version_policy.as_deref(), Some("stable"));
        assert_eq!(
            resolved.latest_blocked_by_policy_version.as_deref(),
            Some("1.3.0b1")
        );
        assert_eq!(resolved.version_policy_warning, None);
    }
}
