//! Release metadata behavior for the `upnow` rebuild.

use std::time::{Duration, SystemTime};

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
