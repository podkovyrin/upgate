use std::time::Duration;

use upnow_domain::{
    AuditFinding, CandidateAuditFact, PolicyWarning, ScanAuditFact, SkipReason, VersionPolicy,
    VersionText,
};

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

pub fn version_too_fresh(
    version: &VersionText,
    age: Option<Duration>,
    required_age: Option<Duration>,
    verbose: bool,
) -> String {
    let version = version_label(version.as_str());
    match (verbose, age, required_age) {
        (true, Some(age), Some(required_age)) => format!(
            "{version} too fresh: {} < {}",
            human_age(age),
            human_age(required_age)
        ),
        _ => format!("{version} too fresh"),
    }
}

pub fn no_eligible_latest_too_fresh(
    version: &VersionText,
    age: Option<Duration>,
    required_age: Option<Duration>,
    verbose: bool,
) -> String {
    format!(
        "no eligible release yet; {}",
        version_too_fresh(version, age, required_age, verbose)
    )
}

pub fn version_blocked_by_policy(version: &VersionText) -> String {
    format!("{} blocked by policy", version_label(version.as_str()))
}

pub fn latest_blocked_by_policy(version: &VersionText, _policy: VersionPolicy) -> String {
    version_blocked_by_policy(version)
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

pub fn audit_candidate(audit: &CandidateAuditFact) -> Option<String> {
    match audit {
        CandidateAuditFact::Clean => None,
        CandidateAuditFact::Vulnerable { findings } => Some(vulnerability_note(findings)),
        CandidateAuditFact::LookupFailed { .. } => Some("audit unavailable".to_owned()),
    }
}

pub fn scan_audit(audit: &ScanAuditFact) -> Option<String> {
    match audit {
        ScanAuditFact::Clean => None,
        ScanAuditFact::Vulnerable { findings } => Some(vulnerability_note(findings)),
        ScanAuditFact::LookupFailed { .. } => Some("audit unavailable".to_owned()),
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
