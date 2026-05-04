use upnow_domain::{
    BlockReason, DelayReason, PlanItem, ScanIssue, ScanItem, ScanReport, UpdatePlan,
};
use upnow_execution::{ExecutionReport, ExecutionStatus};

#[must_use]
pub fn render_scan_report(
    report: &ScanReport,
    old_age_threshold: Option<std::time::Duration>,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!("scan {}", report.manager_id.as_str()));
    for issue in &report.issues {
        lines.push(format!("issue {}", render_scan_issue(issue)));
    }
    for item in &report.items {
        match item {
            ScanItem::Installed(tool) => lines.push(format!(
                "installed {} {}",
                tool.package_name.as_str(),
                tool.installed_version.as_str()
            )),
            ScanItem::InstalledWithReleaseAge { tool, age } => {
                let suffix = if old_age_threshold.is_some_and(|threshold| *age >= threshold) {
                    " old"
                } else {
                    ""
                };
                lines.push(format!(
                    "installed {} {} age {}{}",
                    tool.package_name.as_str(),
                    tool.installed_version.as_str(),
                    human_age(*age),
                    suffix
                ));
            }
            ScanItem::Skipped { tool, reason } => lines.push(format!(
                "skipped {} {}",
                tool.package_name.as_str(),
                render_scan_issue(reason)
            )),
        }
    }
    finish(lines)
}

#[must_use]
pub fn render_update_plan(plan: &UpdatePlan) -> String {
    let mut lines = Vec::new();
    lines.push(format!("plan {}", plan.manager_id.as_str()));
    for item in &plan.items {
        lines.push(match item {
            PlanItem::Update { candidate, .. } => format!(
                "update {} {} -> {}",
                candidate.package_name.as_str(),
                candidate.installed_version.as_str(),
                candidate.target_version.as_str()
            ),
            PlanItem::Current { installed, .. } => format!(
                "current {} {}",
                installed.package_name.as_str(),
                installed.installed_version.as_str()
            ),
            PlanItem::Delayed {
                candidate, reason, ..
            } => format!(
                "delayed {} {} -> {} {}",
                candidate.package_name.as_str(),
                candidate.installed_version.as_str(),
                candidate.target_version.as_str(),
                render_delay_reason(reason)
            ),
            PlanItem::Blocked { seed, reason, .. } => format!(
                "blocked {} {}",
                seed.installed.package_name.as_str(),
                render_block_reason(reason)
            ),
            PlanItem::Skipped {
                installed, reason, ..
            } => format!("skipped {} {reason:?}", installed.package_name.as_str()),
            PlanItem::ResolverError { seed, message, .. } => {
                format!("error {} {}", seed.installed.package_name.as_str(), message)
            }
        });
    }
    finish(lines)
}

#[must_use]
pub fn render_execution_report(report: &ExecutionReport) -> String {
    let mut lines = Vec::new();
    lines.push(format!("apply {}", report.manager_id.as_str()));
    if report.items.is_empty() {
        lines.push("no selected updates".to_owned());
    }
    for item in &report.items {
        match &item.status {
            ExecutionStatus::Succeeded {
                command,
                skipped_mutation,
            } => {
                let suffix = if *skipped_mutation { " skipped" } else { "" };
                lines.push(format!(
                    "applied {} {} -> {}{} ({command})",
                    item.package_name.as_str(),
                    item.installed_version.as_str(),
                    item.target_version.as_str(),
                    suffix
                ));
            }
            ExecutionStatus::Failed { command, detail } => lines.push(format!(
                "failed {} {} -> {} ({command}): {detail}",
                item.package_name.as_str(),
                item.installed_version.as_str(),
                item.target_version.as_str()
            )),
        }
    }
    finish(lines)
}

fn render_scan_issue(issue: &ScanIssue) -> String {
    match issue {
        ScanIssue::ManagerUnavailable { detail }
        | ScanIssue::DiscoveryFailed { detail }
        | ScanIssue::ReleaseLookupFailed { detail } => detail.clone(),
        ScanIssue::MissingReleaseMetadata => "missing release metadata".to_owned(),
        ScanIssue::ExcludedByManagerRule(reason) => format!("{reason:?}"),
    }
}

fn render_delay_reason(reason: &DelayReason) -> &'static str {
    match reason {
        DelayReason::ReleaseTooFresh => "release too fresh",
    }
}

fn render_block_reason(reason: &BlockReason) -> String {
    match reason {
        BlockReason::MissingReleaseMetadata => "missing release metadata".to_owned(),
        BlockReason::ReleaseLookupFailed => "release lookup failed".to_owned(),
        BlockReason::VersionPolicy(reason) => format!("version policy {reason:?}"),
        BlockReason::UnsupportedExactExecution => "unsupported exact execution".to_owned(),
    }
}

fn finish(mut lines: Vec<String>) -> String {
    lines.push(String::new());
    lines.join("\n")
}

fn human_age(age: std::time::Duration) -> String {
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
