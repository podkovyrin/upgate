use crate::util::time::parse_rfc3339_unix;
use anyhow::{Context, Result};
use pep440_rs::Version as Pep440Version;
use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Duration;

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
}

pub fn resolve_pep440_with_min_age(
    current: &str,
    releases: &[Pep440Timestamp],
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<Pep440AgeResolution> {
    let current_ver = Pep440Version::from_str(current)
        .with_context(|| format!("failed to parse current PEP440 version: {current}"))?;

    let mut eligible: Option<(Pep440Version, String, u64)> = None;
    let mut newest_any: Option<(Pep440Version, String, u64)> = None;

    for item in releases {
        let Ok(version) = Pep440Version::from_str(&item.version) else {
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

    let selected_version = eligible.map(|(_, version, _)| version);
    let (latest_version, latest_age_secs) =
        if let Some((_latest_ver, latest_str, latest_ts)) = newest_any {
            (
                Some(latest_str),
                Some(now_unix_secs.saturating_sub(latest_ts)),
            )
        } else {
            (None, None)
        };

    Ok(Pep440AgeResolution {
        selected_version,
        latest_version,
        latest_age_secs,
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
