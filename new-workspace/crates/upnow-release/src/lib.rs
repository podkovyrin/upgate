//! Manager-agnostic release metadata behavior for the `upnow` rebuild.

use std::str::FromStr;
use std::time::{Duration, SystemTime};

use pep440_rs::Version as Pep440Version;
use semver::Version;
use upnow_domain::{ReleaseTimeline, VersionText};

#[must_use]
pub fn release_age_for_version(
    timeline: &ReleaseTimeline,
    version: &VersionText,
    now: SystemTime,
) -> Option<Duration> {
    timeline
        .versions
        .iter()
        .find(|entry| entry.version == *version)
        .map(|entry| {
            now.duration_since(*entry.published_at.as_system_time())
                .unwrap_or(Duration::ZERO)
        })
}

#[must_use]
pub fn newest_semver_version(timeline: &ReleaseTimeline) -> Option<VersionText> {
    timeline
        .versions
        .iter()
        .filter_map(|entry| {
            Version::parse(entry.version.as_str())
                .ok()
                .map(|version| (version, entry.version.clone()))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, version)| version)
}

#[must_use]
pub fn newest_pep440_version(timeline: &ReleaseTimeline) -> Option<VersionText> {
    timeline
        .versions
        .iter()
        .filter_map(|entry| {
            Pep440Version::from_str(entry.version.as_str())
                .ok()
                .map(|version| (version, entry.version.clone()))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, version)| version)
}
