use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};
use semver::Version;

use crate::util::time::parse_rfc3339_unix;

#[derive(Debug, Clone)]
pub struct SemverTimestamp {
    pub version: String,
    pub published_unix: u64,
}

#[derive(Debug, Clone)]
pub struct SemverAgeResolution {
    pub selected_version: Option<String>,
    pub latest_version: Option<String>,
    pub latest_age_secs: Option<u64>,
}

pub fn resolve_semver_with_min_age(
    current: &str,
    releases: &[SemverTimestamp],
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<SemverAgeResolution> {
    // Semver support is intentionally shared at manager-common level for ecosystem managers
    // that represent release timelines as semver + publish timestamp. Manager-local data fetch/
    // parse remains self-contained.
    let current_ver = Version::parse(current)
        .with_context(|| format!("failed to parse current semver: {current}"))?;

    let mut eligible: Option<(Version, String, u64)> = None;
    let mut newest_any: Option<(Version, String, u64)> = None;

    for item in releases {
        let Ok(version) = Version::parse(&item.version) else {
            continue;
        };

        if newest_any
            .as_ref()
            .is_none_or(|(curr, _, _)| version > *curr)
        {
            newest_any = Some((version.clone(), item.version.clone(), item.published_unix));
        }

        if version >= current_ver {
            let age_secs = now_unix_secs.saturating_sub(item.published_unix);
            if age_secs >= min_age.as_secs()
                && eligible.as_ref().is_none_or(|(curr, _, _)| version > *curr)
            {
                eligible = Some((version, item.version.clone(), item.published_unix));
            }
        }
    }

    let selected_version = eligible.map(|(_, version, _)| version).or_else(|| {
        newest_any.as_ref().and_then(|(latest_ver, _, _)| {
            (current_ver >= *latest_ver).then(|| current.to_string())
        })
    });

    let (latest_version, latest_age_secs) =
        if let Some((_latest_ver, latest_str, latest_ts)) = newest_any {
            (
                Some(latest_str),
                Some(now_unix_secs.saturating_sub(latest_ts)),
            )
        } else {
            (None, None)
        };

    Ok(SemverAgeResolution {
        selected_version,
        latest_version,
        latest_age_secs,
    })
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
        )
        .expect("resolution should succeed");

        assert_eq!(resolved.selected_version.as_deref(), Some("2.0.0"));
        assert_eq!(resolved.latest_version.as_deref(), Some("1.9.9"));
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
        )
        .expect("resolution should succeed");

        assert_eq!(resolved.selected_version.as_deref(), Some("1.0.0"));
        assert_eq!(resolved.latest_version.as_deref(), Some("1.0.0"));
    }
}
