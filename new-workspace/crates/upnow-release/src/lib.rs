//! Manager-agnostic release metadata behavior for the `upnow` rebuild.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

use std::str::FromStr;
use std::time::{Duration, SystemTime};

use pep440_rs::Version as Pep440Version;
use semver::Version;
use upnow_domain::{ReleaseEvidenceSource, ReleaseTimeline, VersionReleaseEvidence, VersionText};
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
pub fn release_evidence_for_version(
    timeline: &ReleaseTimeline,
    version: &VersionText,
    source: ReleaseEvidenceSource,
) -> Option<VersionReleaseEvidence> {
    timeline
        .versions
        .iter()
        .find(|entry| entry.version == *version)
        .map(|entry| {
            VersionReleaseEvidence::new(entry.version.clone(), entry.published_at.clone(), source)
        })
}
pub fn release_age_for_evidence(evidence: &VersionReleaseEvidence, now: SystemTime) -> Duration {
    now.duration_since(*evidence.published_at.as_system_time())
        .unwrap_or(Duration::ZERO)
}
pub fn newest_semver_version(timeline: &ReleaseTimeline) -> Option<VersionText> {
    timeline
        .versions
        .iter()
        .filter_map(|entry| {
            let raw = entry.version.as_str().trim().trim_start_matches(['v', 'V']);
            let parsed = Version::parse(raw).or_else(|_| {
                let parts = raw.split('.').collect::<Vec<_>>();
                if parts.is_empty()
                    || parts.len() > 3
                    || parts.iter().any(|part| part.is_empty())
                    || !parts
                        .iter()
                        .all(|part| part.chars().all(|ch| ch.is_ascii_digit()))
                {
                    return Version::parse(raw);
                }
                let mut padded = parts;
                while padded.len() < 3 {
                    padded.push("0");
                }
                Version::parse(&padded.join("."))
            });
            parsed.ok().map(|version| (version, entry.version.clone()))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, version)| version)
}
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
