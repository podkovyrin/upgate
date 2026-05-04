use std::time::{Duration, SystemTime};

use upnow_domain::{ReleaseEntry, ReleaseTimeline, ReleaseTimestamp, VersionText};
use upnow_release::{newest_semver_version, release_age_for_version};

#[test]
fn release_age_uses_matching_version_timestamp() {
    let version = VersionText::new("1.2.0").expect("valid version");
    let timeline = ReleaseTimeline::new(vec![ReleaseEntry::new(
        version.clone(),
        ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
    )]);

    let age = release_age_for_version(
        &timeline,
        &version,
        SystemTime::UNIX_EPOCH + Duration::from_secs(250),
    );

    assert_eq!(age, Some(Duration::from_secs(150)));
}

#[test]
fn release_age_returns_none_when_version_is_absent() {
    let timeline = ReleaseTimeline::new(vec![ReleaseEntry::new(
        VersionText::new("1.0.0").expect("valid version"),
        ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
    )]);

    let age = release_age_for_version(
        &timeline,
        &VersionText::new("1.2.0").expect("valid version"),
        SystemTime::UNIX_EPOCH + Duration::from_secs(250),
    );

    assert_eq!(age, None);
}

#[test]
fn release_age_clamps_future_timestamps_to_zero() {
    let version = VersionText::new("1.2.0").expect("valid version");
    let timeline = ReleaseTimeline::new(vec![ReleaseEntry::new(
        version.clone(),
        ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(300)),
    )]);

    let age = release_age_for_version(
        &timeline,
        &version,
        SystemTime::UNIX_EPOCH + Duration::from_secs(250),
    );

    assert_eq!(age, Some(Duration::ZERO));
}

#[test]
fn newest_semver_version_ignores_non_semver_entries() {
    let timeline = ReleaseTimeline::new(vec![
        ReleaseEntry::new(
            VersionText::new("1.2.0").expect("valid version"),
            ReleaseTimestamp::new(SystemTime::UNIX_EPOCH),
        ),
        ReleaseEntry::new(
            VersionText::new("not-semver").expect("valid version"),
            ReleaseTimestamp::new(SystemTime::UNIX_EPOCH),
        ),
        ReleaseEntry::new(
            VersionText::new("1.10.0").expect("valid version"),
            ReleaseTimestamp::new(SystemTime::UNIX_EPOCH),
        ),
    ]);

    assert_eq!(
        newest_semver_version(&timeline),
        Some(VersionText::new("1.10.0").expect("valid version"))
    );
}
