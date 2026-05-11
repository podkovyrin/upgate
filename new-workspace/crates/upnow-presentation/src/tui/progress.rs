use upnow_execution::progress::{
    ExecutionProgressState, ExecutionProgressStatus, ExecutionProgressSummary,
};

#[must_use]
pub fn render_progress_state(state: &ExecutionProgressState) -> String {
    let mut lines = Vec::new();
    lines.push("interactive apply progress".to_owned());

    for failure in &state.manager_failures {
        lines.push(format!(
            "manager failed {}: {}",
            failure.manager_id.as_str(),
            failure.detail
        ));
    }

    if state.rows.is_empty() {
        lines.push("no selected updates".to_owned());
    }

    for row in &state.rows {
        let status = match &row.status {
            ExecutionProgressStatus::Pending => "pending".to_owned(),
            ExecutionProgressStatus::Running => "running".to_owned(),
            ExecutionProgressStatus::Succeeded {
                command,
                skipped_mutation,
            } => {
                let suffix = if *skipped_mutation { " skipped" } else { "" };
                format!("done{suffix} ({command})")
            }
            ExecutionProgressStatus::Failed { detail } => format!("failed: {detail}"),
            ExecutionProgressStatus::Skipped { detail } => format!("skipped: {detail}"),
        };
        lines.push(format!(
            "{} {} {} -> {} {}",
            row.manager_id.as_str(),
            row.package_name.as_str(),
            row.installed_version.as_str(),
            row.target_version.as_str(),
            status
        ));
    }

    let summary = state.summary();
    lines.push(render_progress_summary(summary));
    lines.push(String::new());
    lines.join("\n")
}

#[must_use]
pub fn render_progress_summary(summary: ExecutionProgressSummary) -> String {
    match (summary.had_failure, summary.stopped_after_current) {
        (false, false) => "summary ok".to_owned(),
        (true, false) => "summary failed".to_owned(),
        (false, true) => "summary stopped".to_owned(),
        (true, true) => "summary failed stopped".to_owned(),
    }
}
