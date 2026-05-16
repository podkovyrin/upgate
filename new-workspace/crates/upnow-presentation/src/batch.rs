use std::time::Duration;

use upnow_domain::{
    AdvisoryLatestFact, BlockReason, CandidateAgeFact, DelayReason, InstalledTool, ManagerId,
    ManagerRuleReason, PlanDiagnostics, PlanIssue, PlanItem, PolicyWarning, ScanIssue, ScanItem,
    ScanReport, SkipReason, UnsupportedReason, UpdateCandidate, UpdatePlan, UpdateSeed,
    VersionPolicy,
};
use upnow_execution::{ExecutionReport, ExecutionStatus, ResolvedExecutionTarget};

use crate::{
    OutcomeNote, OutcomeRow, OutcomeStatusView, OutcomeTable, OutcomeVersionEmphasis,
    OutcomeVersionsView, OutcomeVisibility, OutputTheme, notes, render_outcome_table,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchRenderOptions {
    pub theme: OutputTheme,
    pub old_age_threshold: Option<Duration>,
    pub version_policy: Option<VersionPolicy>,
}

impl BatchRenderOptions {
    pub const fn new(theme: OutputTheme) -> Self {
        Self {
            theme,
            old_age_threshold: None,
            version_policy: None,
        }
    }
    pub const fn with_old_age_threshold(mut self, old_age_threshold: Duration) -> Self {
        self.old_age_threshold = Some(old_age_threshold);
        self
    }
    pub const fn with_version_policy(mut self, version_policy: VersionPolicy) -> Self {
        self.version_policy = Some(version_policy);
        self
    }
}
pub fn render_scan_report(report: &ScanReport, options: BatchRenderOptions) -> String {
    render_batch_table(&scan_report_table(report, options), options.theme)
}
pub fn render_update_plan(plan: &UpdatePlan, options: BatchRenderOptions) -> String {
    render_batch_table(&update_plan_table(plan, options), options.theme)
}
pub fn render_execution_report(
    report: &ExecutionReport,
    issues: &[PlanIssue],
    options: BatchRenderOptions,
) -> String {
    render_batch_table(&execution_report_table(report, issues), options.theme)
}
pub fn render_manager_error(
    manager_id: &ManagerId,
    command: &str,
    detail: &str,
    theme: OutputTheme,
) -> String {
    render_batch_table(&manager_error_table(manager_id, command, detail), theme)
}
pub fn render_batch_table(table: &OutcomeTable, theme: OutputTheme) -> String {
    let rendered = render_outcome_table(table, theme);
    if rendered.is_empty() {
        return rendered;
    }
    format!("{rendered}\n")
}
pub fn manager_error_table(manager_id: &ManagerId, command: &str, detail: &str) -> OutcomeTable {
    let row = OutcomeRow::manager(OutcomeStatusView::Error, manager_id.clone())
        .with_note(OutcomeNote::normal(format!("{command} failed: {detail}")));
    OutcomeTable::new(vec![row])
}
pub fn scan_report_table(report: &ScanReport, options: BatchRenderOptions) -> OutcomeTable {
    let mut rows = report
        .issues
        .iter()
        .map(|issue| scan_issue_row(&report.manager_id, issue))
        .collect::<Vec<_>>();

    rows.extend(
        report
            .items
            .iter()
            .map(|item| scan_item_row(&report.manager_id, item, options)),
    );
    OutcomeTable::new(rows)
}

fn scan_issue_row(manager_id: &ManagerId, issue: &ScanIssue) -> OutcomeRow {
    let status = match issue {
        ScanIssue::DiscoveryFailed { .. }
        | ScanIssue::ReleaseLookupFailed { .. }
        | ScanIssue::MissingReleaseMetadata => OutcomeStatusView::Error,
        ScanIssue::UnsupportedManagerVersion { .. } | ScanIssue::ExcludedByManagerRule(_) => {
            OutcomeStatusView::Skipped
        }
    };
    OutcomeRow::manager(status, manager_id.clone()).with_note(note_for_scan_issue(issue))
}

fn scan_item_row(
    manager_id: &ManagerId,
    item: &ScanItem,
    options: BatchRenderOptions,
) -> OutcomeRow {
    match item {
        ScanItem::Installed(tool) => current_scan_row(manager_id, tool),
        ScanItem::InstalledWithReleaseAge { tool, age } => {
            let is_old = options
                .old_age_threshold
                .is_some_and(|threshold| *age >= threshold);
            current_scan_row(manager_id, tool).with_note(
                release_age_note(*age, is_old).with_visibility(OutcomeVisibility::VerboseOnly),
            )
        }
        ScanItem::Skipped { tool, reason } => OutcomeRow::item(
            scan_issue_status(reason),
            manager_id.clone(),
            tool.package_name.clone(),
            OutcomeVersionsView::Current {
                version: tool.installed_version.clone(),
            },
        )
        .with_note(note_for_scan_issue(reason)),
    }
}

fn current_scan_row(manager_id: &ManagerId, tool: &InstalledTool) -> OutcomeRow {
    OutcomeRow::item(
        OutcomeStatusView::Current,
        manager_id.clone(),
        tool.package_name.clone(),
        OutcomeVersionsView::Current {
            version: tool.installed_version.clone(),
        },
    )
}

const fn scan_issue_status(issue: &ScanIssue) -> OutcomeStatusView {
    match issue {
        ScanIssue::ReleaseLookupFailed { .. }
        | ScanIssue::DiscoveryFailed { .. }
        | ScanIssue::MissingReleaseMetadata => OutcomeStatusView::Error,
        ScanIssue::UnsupportedManagerVersion { .. } | ScanIssue::ExcludedByManagerRule(_) => {
            OutcomeStatusView::Skipped
        }
    }
}

fn note_for_scan_issue(issue: &ScanIssue) -> OutcomeNote {
    match issue {
        ScanIssue::DiscoveryFailed { .. } | ScanIssue::ReleaseLookupFailed { .. } => {
            OutcomeNote::normal(scan_issue_text(issue))
        }
        ScanIssue::MissingReleaseMetadata
        | ScanIssue::ExcludedByManagerRule(_)
        | ScanIssue::UnsupportedManagerVersion { .. } => {
            OutcomeNote::metadata(scan_issue_text(issue))
        }
    }
}
pub fn update_plan_table(plan: &UpdatePlan, options: BatchRenderOptions) -> OutcomeTable {
    let mut rows = plan
        .issues
        .iter()
        .map(|issue| plan_issue_row(&plan.manager_id, issue))
        .collect::<Vec<_>>();
    rows.extend(
        plan.items
            .iter()
            .map(|item| plan_item_row(&plan.manager_id, item, options)),
    );
    OutcomeTable::new(rows)
}

fn plan_issue_row(manager_id: &ManagerId, issue: &PlanIssue) -> OutcomeRow {
    let status = match issue {
        PlanIssue::UnsupportedManagerVersion { .. } => OutcomeStatusView::Skipped,
        PlanIssue::DiscoveryFailed { .. } => OutcomeStatusView::Error,
    };
    OutcomeRow::manager(status, manager_id.clone())
        .with_note(OutcomeNote::normal(plan_issue_text(issue)))
}

fn plan_item_row(
    manager_id: &ManagerId,
    item: &PlanItem,
    options: BatchRenderOptions,
) -> OutcomeRow {
    match item {
        PlanItem::Update { candidate, .. } => update_row(manager_id, candidate, options),
        PlanItem::Current { installed, .. } => current_no_newer_row(manager_id, installed),
        PlanItem::Delayed {
            candidate, reason, ..
        } => delayed_row(manager_id, candidate, reason, options),
        PlanItem::Blocked {
            seed,
            reason,
            policy_warnings,
            diagnostics,
            ..
        } => blocked_row(
            manager_id,
            seed,
            reason,
            policy_warnings,
            diagnostics,
            options,
        ),
        PlanItem::Skipped {
            installed, reason, ..
        } => skipped_plan_row(manager_id, installed, reason),
        PlanItem::ResolverError {
            installed, message, ..
        } => OutcomeRow::item(
            OutcomeStatusView::Error,
            manager_id.clone(),
            installed.package_name.clone(),
            OutcomeVersionsView::Current {
                version: installed.installed_version.clone(),
            },
        )
        .with_note(OutcomeNote::normal(message)),
    }
}

fn update_row(
    manager_id: &ManagerId,
    candidate: &UpdateCandidate,
    options: BatchRenderOptions,
) -> OutcomeRow {
    let mut row = candidate_row(OutcomeStatusView::Update, manager_id, candidate);
    if let Some(note) = target_release_note(&candidate.diagnostics) {
        row = row.with_note(note);
    }
    if let Some(note) = latest_too_fresh_note(&candidate.diagnostics, options.theme) {
        row = row.with_note(note);
    }
    row = append_advisory_warning_notes(row, &candidate.diagnostics);
    row = append_policy_notes(row, &candidate.diagnostics, options.version_policy);
    append_policy_warning_notes(row, &candidate.policy_warnings)
}

fn delayed_row(
    manager_id: &ManagerId,
    candidate: &UpdateCandidate,
    reason: &DelayReason,
    options: BatchRenderOptions,
) -> OutcomeRow {
    let mut row = candidate_row(OutcomeStatusView::Delayed, manager_id, candidate);
    if let Some(note) = target_release_note(&candidate.diagnostics) {
        row = row.with_note(note);
    }
    row = row.with_note(delayed_note(reason, &candidate.diagnostics, options.theme));
    row = append_advisory_warning_notes(row, &candidate.diagnostics);
    row = append_policy_notes(row, &candidate.diagnostics, options.version_policy);
    append_policy_warning_notes(row, &candidate.policy_warnings)
}

fn blocked_row(
    manager_id: &ManagerId,
    seed: &UpdateSeed,
    reason: &BlockReason,
    policy_warnings: &[PolicyWarning],
    diagnostics: &PlanDiagnostics,
    options: BatchRenderOptions,
) -> OutcomeRow {
    let versions = seed.target_selection.target_version().map_or_else(
        || OutcomeVersionsView::manager_resolved(seed.installed.installed_version.clone()),
        |target_version| OutcomeVersionsView::Change {
            from: seed.installed.installed_version.clone(),
            to: crate::OutcomeTargetView::Known(target_version.clone()),
            emphasis: OutcomeVersionEmphasis::Current,
        },
    );
    let mut row = match reason {
        BlockReason::VersionPolicy(_) => OutcomeRow::item(
            OutcomeStatusView::Current,
            manager_id.clone(),
            seed.installed.package_name.clone(),
            OutcomeVersionsView::Current {
                version: seed.installed.installed_version.clone(),
            },
        ),
        BlockReason::MissingReleaseMetadata => OutcomeRow::item(
            OutcomeStatusView::Skipped,
            manager_id.clone(),
            seed.installed.package_name.clone(),
            versions,
        )
        .with_note(OutcomeNote::metadata("missing release metadata")),
        BlockReason::ReleaseLookupFailed => OutcomeRow::item(
            OutcomeStatusView::Error,
            manager_id.clone(),
            seed.installed.package_name.clone(),
            versions,
        )
        .with_note(OutcomeNote::normal(lookup_failure_text(diagnostics))),
    };
    if matches!(reason, BlockReason::VersionPolicy(_)) {
        row = append_policy_notes(row, diagnostics, options.version_policy);
    }
    row = append_advisory_warning_notes(row, diagnostics);
    append_policy_warning_notes(row, policy_warnings)
}

fn current_no_newer_row(manager_id: &ManagerId, installed: &InstalledTool) -> OutcomeRow {
    OutcomeRow::item(
        OutcomeStatusView::Current,
        manager_id.clone(),
        installed.package_name.clone(),
        OutcomeVersionsView::Current {
            version: installed.installed_version.clone(),
        },
    )
    .with_note(OutcomeNote::metadata("no newer version found"))
    .with_visibility(OutcomeVisibility::VerboseOnly)
}

fn skipped_plan_row(
    manager_id: &ManagerId,
    installed: &InstalledTool,
    reason: &SkipReason,
) -> OutcomeRow {
    OutcomeRow::item(
        OutcomeStatusView::Skipped,
        manager_id.clone(),
        installed.package_name.clone(),
        OutcomeVersionsView::Current {
            version: installed.installed_version.clone(),
        },
    )
    .with_note(OutcomeNote::metadata(notes::skip_reason(reason)))
}

fn candidate_row(
    status: OutcomeStatusView,
    manager_id: &ManagerId,
    candidate: &UpdateCandidate,
) -> OutcomeRow {
    OutcomeRow::item(
        status,
        manager_id.clone(),
        candidate.package_name.clone(),
        candidate.target_version().map_or_else(
            || OutcomeVersionsView::manager_resolved(candidate.installed_version.clone()),
            |target_version| {
                OutcomeVersionsView::change(
                    candidate.installed_version.clone(),
                    target_version.clone(),
                )
            },
        ),
    )
}
pub fn execution_report_table(report: &ExecutionReport, issues: &[PlanIssue]) -> OutcomeTable {
    let mut rows = issues
        .iter()
        .map(|issue| plan_issue_row(&report.manager_id, issue))
        .collect::<Vec<_>>();

    if report.items.is_empty() {
        rows.push(
            OutcomeRow::manager(OutcomeStatusView::Current, report.manager_id.clone())
                .with_note(OutcomeNote::metadata("no selected updates")),
        );
    }

    rows.extend(report.items.iter().map(|item| {
        let versions = match &item.target {
            ResolvedExecutionTarget::Known(target_version) => {
                OutcomeVersionsView::change(item.installed_version.clone(), target_version.clone())
            }
            ResolvedExecutionTarget::ManagerResolved => {
                OutcomeVersionsView::manager_resolved(item.installed_version.clone())
            }
        };
        match &item.status {
            ExecutionStatus::Succeeded {
                command,
                skipped_mutation,
            } => {
                let mut row = OutcomeRow::item(
                    OutcomeStatusView::Update,
                    report.manager_id.clone(),
                    item.package_name.clone(),
                    versions,
                )
                .with_note(OutcomeNote::metadata(command));
                if *skipped_mutation {
                    row = row.with_note(OutcomeNote::metadata("mutation skipped"));
                }
                row
            }
            ExecutionStatus::Failed { command, detail } => OutcomeRow::item(
                OutcomeStatusView::Error,
                report.manager_id.clone(),
                item.package_name.clone(),
                versions,
            )
            .with_note(OutcomeNote::metadata(command))
            .with_note(OutcomeNote::normal(detail)),
        }
    }));

    OutcomeTable::new(rows)
}

fn append_policy_notes(
    row: OutcomeRow,
    diagnostics: &PlanDiagnostics,
    version_policy: Option<VersionPolicy>,
) -> OutcomeRow {
    let Some(policy) = version_policy else {
        return row;
    };
    if policy == VersionPolicy::None {
        return row;
    }
    if let Some(latest) = latest_policy_blocked_version(diagnostics) {
        return row.with_note(OutcomeNote::normal(notes::latest_blocked_by_policy(
            latest, policy,
        )));
    }
    row
}

fn append_policy_warning_notes(mut row: OutcomeRow, warnings: &[PolicyWarning]) -> OutcomeRow {
    for warning in warnings {
        row = row.with_note(OutcomeNote::normal(notes::version_policy_warning(*warning)));
    }
    row
}

fn append_advisory_warning_notes(mut row: OutcomeRow, diagnostics: &PlanDiagnostics) -> OutcomeRow {
    let advisory_failure = diagnostics.advisory_lookup_failure.as_ref().or(
        match diagnostics.advisory_latest.as_ref() {
            Some(AdvisoryLatestFact::LookupFailed { error, .. }) => Some(error),
            _ => None,
        },
    );
    if let Some(error) = advisory_failure {
        row = row.with_note(OutcomeNote::warning(format!(
            "advisory latest lookup failed: {}",
            error.detail
        )));
    }
    row
}

fn latest_policy_blocked_version(
    diagnostics: &PlanDiagnostics,
) -> Option<&upnow_domain::VersionText> {
    diagnostics
        .candidates
        .iter()
        .find(|candidate| candidate.policy_block_reason.is_some())
        .map(|candidate| &candidate.version)
}

fn latest_too_fresh_note(diagnostics: &PlanDiagnostics, theme: OutputTheme) -> Option<OutcomeNote> {
    let latest = latest_too_fresh(diagnostics)?;
    let note = notes::latest_too_fresh(
        &latest.version,
        Some(latest.age),
        Some(diagnostics.required_age),
        theme.verbose,
    );
    Some(OutcomeNote::metadata(note))
}

fn delayed_note(
    reason: &DelayReason,
    diagnostics: &PlanDiagnostics,
    theme: OutputTheme,
) -> OutcomeNote {
    match reason {
        DelayReason::ReleaseTooFresh => {
            if let Some(target) = diagnostics.selected_target.as_ref() {
                return OutcomeNote::normal(notes::too_fresh(
                    Some(target.age),
                    diagnostics.required_age,
                ));
            }
            if let Some(latest) = latest_too_fresh(diagnostics) {
                return OutcomeNote::normal(notes::no_eligible_latest_too_fresh(
                    &latest.version,
                    Some(latest.age),
                    Some(diagnostics.required_age),
                    theme.verbose,
                ));
            }
            OutcomeNote::normal(format!(
                "no eligible release yet; required age {}",
                notes::human_age(diagnostics.required_age)
            ))
        }
    }
}

fn latest_too_fresh(diagnostics: &PlanDiagnostics) -> Option<&CandidateAgeFact> {
    diagnostics
        .latest_overall
        .as_ref()
        .filter(|latest| latest.age < diagnostics.required_age)
        .or_else(|| {
            diagnostics
                .advisory_latest
                .as_ref()
                .and_then(advisory_latest_age_fact)
                .filter(|latest| latest.age < diagnostics.required_age)
        })
}

fn advisory_latest_age_fact(advisory: &AdvisoryLatestFact) -> Option<&CandidateAgeFact> {
    match advisory {
        AdvisoryLatestFact::Known {
            latest_version,
            candidates,
        } => candidates
            .iter()
            .find(|candidate| &candidate.version == latest_version)
            .or_else(|| candidates.first()),
        AdvisoryLatestFact::MissingMetadata { .. } | AdvisoryLatestFact::LookupFailed { .. } => {
            None
        }
    }
}

fn release_age_note(age: Duration, is_old: bool) -> OutcomeNote {
    let text = notes::released(age);
    if is_old {
        return OutcomeNote::warning(text);
    }
    OutcomeNote::metadata(text)
}

fn target_release_note(diagnostics: &PlanDiagnostics) -> Option<OutcomeNote> {
    diagnostics.selected_target.as_ref().map(|target| {
        release_age_note(target.age, false).with_visibility(OutcomeVisibility::VerboseOnly)
    })
}

fn lookup_failure_text(diagnostics: &PlanDiagnostics) -> String {
    diagnostics.lookup_failure.as_ref().map_or_else(
        || "release lookup failed".to_owned(),
        |err| err.detail.clone(),
    )
}

fn scan_issue_text(issue: &ScanIssue) -> String {
    match issue {
        ScanIssue::DiscoveryFailed { detail } | ScanIssue::ReleaseLookupFailed { detail } => {
            detail.clone()
        }
        ScanIssue::MissingReleaseMetadata => "missing release metadata".to_owned(),
        ScanIssue::UnsupportedManagerVersion {
            installed_version,
            reason,
        } => format!(
            "unsupported manager version {} {}",
            installed_version.as_str(),
            unsupported_reason_text(reason)
        ),
        ScanIssue::ExcludedByManagerRule(reason) => manager_rule_reason_text(reason),
    }
}

fn plan_issue_text(issue: &PlanIssue) -> String {
    match issue {
        PlanIssue::DiscoveryFailed { detail } => detail.clone(),
        PlanIssue::UnsupportedManagerVersion {
            installed_version,
            reason,
        } => format!(
            "unsupported manager version {} {}",
            installed_version.as_str(),
            unsupported_reason_text(reason)
        ),
    }
}

const fn unsupported_reason_text(reason: &UnsupportedReason) -> &'static str {
    match reason {
        UnsupportedReason::YarnModernGlobalUnsupported => {
            "global upgrades are not supported for Yarn 2+"
        }
    }
}

fn manager_rule_reason_text(reason: &ManagerRuleReason) -> String {
    match reason {
        ManagerRuleReason::Dependency => "dependency".to_owned(),
        ManagerRuleReason::DefaultGem => "default gem".to_owned(),
        ManagerRuleReason::Other { detail } => detail.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use upnow_domain::{
        CandidateAgeFact, CandidateAgeSource, ExecutionSupport, PlanItemId, ReleaseEvidenceSource,
        ReleaseLookupError, ReleaseTimestamp, ToolId, VersionReleaseEvidence, VersionScheme,
        VersionText,
    };

    use super::*;
    use crate::OutcomeNoteTone;

    // This test must exist because advisory failures are user-visible warnings,
    // while the uv dry-run selected target remains the product authority.
    #[test]
    fn advisory_lookup_failure_renders_as_warning_without_blocking_update() {
        let manager_id = manager_id("uv");
        let package_name = package_name("ruff");
        let candidate = UpdateCandidate::new(
            ToolId::new("uv:ruff").expect("valid tool id"),
            package_name,
            version("0.1.0"),
            version("0.2.0"),
            VersionScheme::Pep440,
            ExecutionSupport::resolver_native(
                upnow_domain::MinAgeConstraintSupport::Optional,
                true,
                false,
            ),
        )
        .with_diagnostics(PlanDiagnostics {
            required_age: Duration::from_secs(604_800),
            advisory_lookup_failure: Some(ReleaseLookupError::new(
                "uv tool list --outdated failed (exit 1): network unavailable",
            )),
            ..PlanDiagnostics::default()
        });
        let plan = UpdatePlan::new(
            manager_id,
            vec![PlanItem::Update {
                id: PlanItemId::new("uv:ruff").expect("valid plan item id"),
                candidate,
            }],
        )
        .expect("valid plan");

        let table = update_plan_table(&plan, BatchRenderOptions::new(OutputTheme::plain(false)));

        let [row] = table.rows.as_slice() else {
            panic!("expected one rendered row");
        };
        assert_eq!(row.status, OutcomeStatusView::Update);
        assert!(row.notes.iter().any(|note| {
            note.tone == OutcomeNoteTone::Warning
                && note.text.contains("advisory latest lookup failed")
                && note.text.contains("uv tool list --outdated failed")
        }));
    }

    // This test must exist because plan verbose release age is user-visible CLI
    // behavior and should remain a presentation concern over typed plan evidence.
    #[test]
    fn plan_verbose_renders_target_release_age() {
        let manager_id = manager_id("npm");
        let package_name = package_name("alpha");
        let target = version("2.0.0");
        let candidate = UpdateCandidate::new(
            ToolId::new("npm:alpha").expect("valid tool id"),
            package_name,
            version("1.0.0"),
            target.clone(),
            VersionScheme::SemVer,
            ExecutionSupport::exact_only(),
        )
        .with_diagnostics(PlanDiagnostics {
            selected_target: Some(CandidateAgeFact::new(
                target.clone(),
                Duration::from_secs(3 * 24 * 60 * 60),
                CandidateAgeSource::ReleaseTimeline,
                VersionReleaseEvidence::new(
                    target,
                    ReleaseTimestamp::new(UNIX_EPOCH),
                    ReleaseEvidenceSource::ReleaseTimeline,
                ),
            )),
            ..PlanDiagnostics::default()
        });
        let plan = UpdatePlan::new(
            manager_id,
            vec![PlanItem::Update {
                id: PlanItemId::new("npm:alpha").expect("valid plan item id"),
                candidate,
            }],
        )
        .expect("valid plan");

        let regular = render_update_plan(&plan, BatchRenderOptions::new(OutputTheme::plain(false)));
        let verbose = render_update_plan(&plan, BatchRenderOptions::new(OutputTheme::plain(true)));

        assert!(!regular.contains("released: 3d"));
        assert!(verbose.contains("released: 3d"));
    }

    fn manager_id(value: &str) -> ManagerId {
        ManagerId::new(value).expect("valid manager id")
    }

    fn package_name(value: &str) -> upnow_domain::PackageName {
        upnow_domain::PackageName::new(value).expect("valid package name")
    }

    fn version(value: &str) -> VersionText {
        VersionText::new(value).expect("valid version")
    }
}
