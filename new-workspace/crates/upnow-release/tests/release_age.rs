use std::time::SystemTime;

use upnow_domain::{ReleaseEntry, ReleaseTimeline, ReleaseTimestamp, VersionText};
use upnow_release::newest_semver_version;

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
