use std::time::SystemTime;

use upnow_domain::{
    ReleaseEntry, ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, VersionText,
};

#[test]
fn release_timestamp_preserves_parsed_time() {
    let timestamp = upnow_domain::ReleaseTimestamp::new(SystemTime::UNIX_EPOCH);

    assert_eq!(timestamp.as_system_time(), &SystemTime::UNIX_EPOCH);
}

#[test]
fn release_lookup_result_distinguishes_known_missing_and_failed_metadata() {
    let timeline = ReleaseTimeline::new(vec![ReleaseEntry::new(
        VersionText::new("1.2.0").expect("valid version"),
        upnow_domain::ReleaseTimestamp::new(SystemTime::UNIX_EPOCH),
    )]);

    assert!(matches!(
        ReleaseLookupResult::Known(timeline),
        ReleaseLookupResult::Known(_)
    ));
    assert_eq!(
        ReleaseLookupResult::MissingMetadata,
        ReleaseLookupResult::MissingMetadata
    );
    assert_eq!(
        ReleaseLookupResult::LookupFailed(ReleaseLookupError::new("registry timeout")),
        ReleaseLookupResult::LookupFailed(ReleaseLookupError::new("registry timeout"))
    );
}
