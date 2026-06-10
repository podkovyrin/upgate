use std::time::Duration;

use upgate_domain::{AuditFinding, AuditLookupResult, PolicyWarning, SkipReason, VersionText};

use crate::version_label;

pub fn released(age: Duration) -> String {
    format!("{} ago", human_age(age))
}

pub fn too_fresh(age: Option<Duration>, required_age: Duration) -> String {
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

pub fn version_too_fresh(version: &VersionText) -> String {
    format!("{} too fresh", version_label(version.as_str()))
}

pub fn no_eligible_latest_too_fresh(version: &VersionText) -> String {
    format!("no eligible release yet; {}", version_too_fresh(version))
}

pub fn version_blocked_by_policy(version: &VersionText) -> String {
    format!("{} blocked by policy", version_label(version.as_str()))
}

pub const fn policy_warning(warning: PolicyWarning) -> &'static str {
    match warning {
        PolicyWarning::InstalledTrackUnknownFallbackStable => {
            "same-track fell back to stable because installed track is unknown"
        }
    }
}

pub fn version_policy_warning(warning: PolicyWarning) -> String {
    format!("version policy warning: {}", policy_warning(warning))
}

pub fn audit_candidate(audit: &AuditLookupResult) -> Option<String> {
    match audit {
        AuditLookupResult::Clean => None,
        AuditLookupResult::Vulnerable { findings } => Some(vulnerability_note(findings)),
        AuditLookupResult::LookupFailed { .. } => Some("audit unavailable".to_owned()),
    }
}

pub fn vulnerability_note(findings: &[AuditFinding]) -> String {
    let ids = findings
        .iter()
        .flat_map(|finding| {
            std::iter::once(finding.id.as_str()).chain(finding.aliases.iter().map(String::as_str))
        })
        .take(3)
        .collect::<Vec<_>>();
    if ids.is_empty() {
        "vulnerable".to_owned()
    } else {
        format!("vulnerable: {}", ids.join(", "))
    }
}

pub fn skip_reason(reason: &SkipReason) -> String {
    match reason {
        SkipReason::Pinned => "pinned".to_owned(),
        SkipReason::ManagerRule(detail) => detail.clone(),
    }
}

pub fn human_age(age: Duration) -> String {
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
