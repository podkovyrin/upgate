use std::time::Duration;

use upnow_domain::{PolicyBlockReason, PolicyWarning, SkipReason, VersionPolicy, VersionText};

use crate::version_label;

pub(crate) fn released(age: Duration) -> String {
    format!("released: {}", human_age(age))
}

pub(crate) fn too_fresh(age: Option<Duration>, required_age: Duration) -> String {
    age.map_or_else(
        || format!("too fresh: need {}", human_age(required_age)),
        |age| {
            format!(
                "too fresh: {} < {}",
                human_age(age),
                human_age(required_age)
            )
        },
    )
}

pub(crate) fn latest_too_fresh(
    version: &VersionText,
    age: Option<Duration>,
    required_age: Option<Duration>,
    verbose: bool,
) -> String {
    let version = version_label(version.as_str());
    match (verbose, age, required_age) {
        (true, Some(age), Some(required_age)) => format!(
            "latest {version} too fresh: {} < {}",
            human_age(age),
            human_age(required_age)
        ),
        _ => format!("latest {version} too fresh"),
    }
}

pub(crate) fn no_eligible_latest_too_fresh(
    version: &VersionText,
    age: Option<Duration>,
    required_age: Option<Duration>,
    verbose: bool,
) -> String {
    format!(
        "no eligible release yet; {}",
        latest_too_fresh(version, age, required_age, verbose)
    )
}

pub(crate) fn version_policy_blocked(reason: &PolicyBlockReason) -> String {
    match reason {
        PolicyBlockReason::PreReleaseBlocked => "pre-release blocked by policy".to_owned(),
        PolicyBlockReason::TrackRegression => "track regression blocked by policy".to_owned(),
        PolicyBlockReason::UnknownStability => "unknown stability blocked by policy".to_owned(),
    }
}

pub(crate) fn latest_blocked_by_policy(version: &VersionText, policy: VersionPolicy) -> String {
    format!(
        "latest {} blocked by version policy: {policy}",
        version_label(version.as_str())
    )
}

pub(crate) fn policy_warning(warning: PolicyWarning) -> &'static str {
    match warning {
        PolicyWarning::InstalledTrackUnknownFallbackStable => {
            "same-track fell back to stable because installed track is unknown"
        }
    }
}

pub(crate) fn version_policy_warning(warning: PolicyWarning) -> String {
    format!("version policy warning: {}", policy_warning(warning))
}

pub(crate) fn skip_reason(reason: &SkipReason) -> String {
    match reason {
        SkipReason::Pinned => "pinned".to_owned(),
        SkipReason::ManagerRule(detail) => detail.clone(),
    }
}

pub(crate) fn human_age(age: Duration) -> String {
    let seconds = age.as_secs();
    let days = seconds / (24 * 60 * 60);
    if days > 0 {
        return format!("{days}d");
    }
    let hours = seconds / (60 * 60);
    if hours > 0 {
        return format!("{hours}h");
    }
    let minutes = seconds / 60;
    if minutes > 0 {
        return format!("{minutes}m");
    }
    format!("{seconds}s")
}
