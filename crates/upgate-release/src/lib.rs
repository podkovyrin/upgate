//! Manager-agnostic release metadata behavior for the `upgate` rebuild.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

use std::str::FromStr;

use pep440_rs::Version as Pep440Version;
use semver::Version;
use upgate_domain::{ReleaseTimeline, VersionReleaseEvidence, VersionText};
pub fn release_evidence_for_version(
    timeline: &ReleaseTimeline,
    version: &VersionText,
) -> Option<VersionReleaseEvidence> {
    timeline
        .versions
        .iter()
        .find(|entry| entry.version == *version)
        .map(|entry| VersionReleaseEvidence::new(entry.version.clone(), entry.published_at.clone()))
}
pub fn newest_semver_version(timeline: &ReleaseTimeline) -> Option<VersionText> {
    timeline
        .versions
        .iter()
        .filter_map(|entry| {
            let raw = entry.version.as_str().trim().trim_start_matches(['v', 'V']);
            let parsed = Version::parse(raw).or_else(|err| {
                let parts = raw.split('.').collect::<Vec<_>>();
                if parts.is_empty()
                    || parts.len() > 3
                    || parts.iter().any(|part| part.is_empty())
                    || !parts
                        .iter()
                        .all(|part| part.chars().all(|ch| ch.is_ascii_digit()))
                {
                    return Err(err);
                }
                let mut padded = parts;
                while padded.len() < 3 {
                    padded.push("0");
                }
                Version::parse(&padded.join("."))
            });
            parsed.ok().map(|version| (entry, version))
        })
        .max_by(|(left_entry, left_version), (right_entry, right_version)| {
            left_entry
                .published_at
                .as_system_time()
                .cmp(right_entry.published_at.as_system_time())
                .then_with(|| left_version.cmp(right_version))
        })
        .map(|(entry, _)| entry.version.clone())
}
pub fn newest_pep440_version(timeline: &ReleaseTimeline) -> Option<VersionText> {
    timeline
        .versions
        .iter()
        .filter_map(|entry| {
            Pep440Version::from_str(entry.version.as_str())
                .ok()
                .map(|version| (entry, version))
        })
        .max_by(|(left_entry, left_version), (right_entry, right_version)| {
            left_entry
                .published_at
                .as_system_time()
                .cmp(right_entry.published_at.as_system_time())
                .then_with(|| left_version.cmp(right_version))
        })
        .map(|(entry, _)| entry.version.clone())
}
