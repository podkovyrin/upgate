use std::str::FromStr;

use upnow_domain::{DomainError, VersionPolicy};

#[test]
fn version_policy_parses_approved_modes() {
    assert_eq!(
        VersionPolicy::from_str("none").expect("none policy should parse"),
        VersionPolicy::None
    );
    assert_eq!(
        VersionPolicy::from_str("stable").expect("stable policy should parse"),
        VersionPolicy::Stable
    );
    assert_eq!(
        VersionPolicy::from_str("same-track").expect("same-track policy should parse"),
        VersionPolicy::SameTrack
    );
}

#[test]
fn version_policy_displays_config_spelling() {
    assert_eq!(VersionPolicy::None.to_string(), "none");
    assert_eq!(VersionPolicy::Stable.to_string(), "stable");
    assert_eq!(VersionPolicy::SameTrack.to_string(), "same-track");
}

#[test]
fn version_policy_rejects_unknown_modes() {
    assert_eq!(
        VersionPolicy::from_str("any"),
        Err(DomainError::InvalidVersionPolicy("any".to_owned()))
    );
}
