use std::collections::BTreeSet;
use std::time::Duration;

use upgate_domain::{
    AdvisoryLatestFact, BlockReason, CandidateAgeFact, DelayReason, InstalledTool, ManagerId,
    ManagerRuleReason, PlanDiagnostics, PlanItem, PlanSelection, PolicyWarning, ScanIssue,
    ScanItem, ScanReport, SkipReason, UpdateCandidate, UpdatePlan, UpdateSeed, VersionPolicy,
};
use upgate_execution::{ExecutionReport, ExecutionStatus, ResolvedExecutionTarget};

use crate::{
    OutcomeNote, OutcomeRow, OutcomeStatusView, OutcomeTable, OutcomeVersionEmphasis,
    OutcomeVersionsView, OutcomeVisibility, OutputTheme, notes, render_outcome_table,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchRenderOptions {
    pub theme: OutputTheme,
    pub version_policy: Option<VersionPolicy>,
}

impl BatchRenderOptions {
    pub const fn new(theme: OutputTheme) -> Self {
        Self {
            theme,
            version_policy: None,
        }
    }
    pub const fn with_version_policy(mut self, version_policy: VersionPolicy) -> Self {
        self.version_policy = Some(version_policy);
        self
    }
}
pub fn apply_execution_report_table(
    report: &ExecutionReport,
    plan: &UpdatePlan,
    selection: &PlanSelection,
) -> OutcomeTable {
    let mut rows = unselected_update_rows(plan, selection);

    if report.items.is_empty() && rows.is_empty() {
        rows.push(
            OutcomeRow::manager(OutcomeStatusView::Current, report.manager_id.clone())
                .with_note(OutcomeNote::normal("no selected updates")),
        );
    }

    rows.extend(execution_report_rows(report));
    OutcomeTable::new(rows)
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
pub fn scan_report_table(report: &ScanReport) -> OutcomeTable {
    let mut rows = report
        .issues
        .iter()
        .map(|issue| scan_issue_row(&report.manager_id, issue))
        .collect::<Vec<_>>();

    rows.extend(
        report
            .items
            .iter()
            .map(|item| scan_item_row(&report.manager_id, item)),
    );
    OutcomeTable::new(rows)
}

fn scan_issue_row(manager_id: &ManagerId, issue: &ScanIssue) -> OutcomeRow {
    OutcomeRow::manager(scan_issue_status(issue), manager_id.clone())
        .with_note(note_for_scan_issue(issue))
}

fn scan_item_row(manager_id: &ManagerId, item: &ScanItem) -> OutcomeRow {
    match item {
        ScanItem::Installed(tool) => current_scan_row(manager_id, tool),
        ScanItem::InstalledWithReleaseAge { tool, age } => current_scan_row(manager_id, tool)
            .with_note(release_age_note(*age).with_visibility(OutcomeVisibility::VerboseOnly)),
        ScanItem::InstalledWithAudit { tool, age, audit } => {
            let mut row = current_scan_row(manager_id, tool);
            if let Some(age) = age {
                row = row.with_note(
                    release_age_note(*age).with_visibility(OutcomeVisibility::VerboseOnly),
                );
            }
            if let Some(note) = notes::audit_candidate(audit) {
                row = row.with_note(OutcomeNote::normal(note));
            }
            row
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
        ScanIssue::ExcludedByManagerRule(_) => OutcomeStatusView::Skipped,
    }
}

fn note_for_scan_issue(issue: &ScanIssue) -> OutcomeNote {
    OutcomeNote::normal(scan_issue_text(issue))
}
pub fn update_plan_table(plan: &UpdatePlan, options: BatchRenderOptions) -> OutcomeTable {
    let rows = plan
        .items
        .iter()
        .map(|item| plan_item_row(&plan.manager_id, item, options))
        .collect::<Vec<_>>();
    OutcomeTable::new(rows)
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
    if let Some(note) = latest_too_fresh_note(&candidate.diagnostics) {
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
    row = row.with_note(delayed_note(reason, &candidate.diagnostics));
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
    let versions = blocked_target_version(seed, reason, diagnostics).map_or_else(
        || OutcomeVersionsView::manager_resolved(seed.installed.installed_version.clone()),
        |target_version| OutcomeVersionsView::Change {
            from: seed.installed.installed_version.clone(),
            to: crate::OutcomeTargetView::Known(target_version),
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
            OutcomeStatusView::Blocked,
            manager_id.clone(),
            seed.installed.package_name.clone(),
            versions,
        )
        .with_note(OutcomeNote::normal("missing release metadata")),
        BlockReason::ReleaseLookupFailed => OutcomeRow::item(
            OutcomeStatusView::Error,
            manager_id.clone(),
            seed.installed.package_name.clone(),
            versions,
        )
        .with_note(OutcomeNote::normal(lookup_failure_text(diagnostics))),
        BlockReason::AuditVulnerable | BlockReason::AuditLookupFailed => {
            let note = diagnostics
                .audit_blocking_target
                .as_ref()
                .and_then(notes::audit_candidate)
                .unwrap_or_else(|| "audit unavailable".to_owned());
            OutcomeRow::item(
                OutcomeStatusView::Blocked,
                manager_id.clone(),
                seed.installed.package_name.clone(),
                versions,
            )
            .with_note(OutcomeNote::normal(note))
        }
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
    .with_note(OutcomeNote::normal("no newer version found"))
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
    .with_note(OutcomeNote::normal(notes::skip_reason(reason)))
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
fn unselected_update_rows(plan: &UpdatePlan, selection: &PlanSelection) -> Vec<OutcomeRow> {
    let selected_ids = selection
        .selected_items
        .iter()
        .map(|item| item.plan_item_id.clone())
        .collect::<BTreeSet<_>>();
    plan.items
        .iter()
        .filter_map(|item| match item {
            PlanItem::Update { id, candidate } if !selected_ids.contains(id) => Some(
                candidate_row(OutcomeStatusView::Skipped, &plan.manager_id, candidate)
                    .with_note(OutcomeNote::normal("not selected")),
            ),
            _ => None,
        })
        .collect()
}

fn execution_report_rows(report: &ExecutionReport) -> Vec<OutcomeRow> {
    report
        .items
        .iter()
        .map(|item| {
            let versions = match &item.target {
                ResolvedExecutionTarget::Known(target_version) => OutcomeVersionsView::change(
                    item.installed_version.clone(),
                    target_version.clone(),
                ),
                ResolvedExecutionTarget::ManagerResolved => {
                    OutcomeVersionsView::manager_resolved(item.installed_version.clone())
                }
            };
            match &item.status {
                ExecutionStatus::Succeeded { .. } => OutcomeRow::item(
                    OutcomeStatusView::Update,
                    report.manager_id.clone(),
                    item.package_name.clone(),
                    versions,
                ),
                ExecutionStatus::Failed { detail, .. } => OutcomeRow::item(
                    OutcomeStatusView::Error,
                    report.manager_id.clone(),
                    item.package_name.clone(),
                    versions,
                )
                .with_note(OutcomeNote::normal(detail)),
            }
        })
        .collect()
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
        return row.with_note(OutcomeNote::normal(notes::version_blocked_by_policy(
            latest,
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
        row = row.with_note(OutcomeNote::normal(format!(
            "advisory latest lookup failed: {}",
            error.detail
        )));
    }
    row
}

fn blocked_target_version(
    seed: &UpdateSeed,
    reason: &BlockReason,
    diagnostics: &PlanDiagnostics,
) -> Option<upgate_domain::VersionText> {
    if matches!(
        reason,
        BlockReason::AuditVulnerable | BlockReason::AuditLookupFailed
    ) && let Some(candidate) = diagnostics.audit_blocking_candidate.as_ref()
    {
        return Some(candidate.version.clone());
    }
    seed.target_selection.target_version().cloned()
}

fn latest_policy_blocked_version(
    diagnostics: &PlanDiagnostics,
) -> Option<&upgate_domain::VersionText> {
    diagnostics
        .candidates
        .iter()
        .find(|candidate| candidate.policy_block_reason.is_some())
        .map(|candidate| &candidate.version)
}

fn latest_too_fresh_note(diagnostics: &PlanDiagnostics) -> Option<OutcomeNote> {
    let latest = latest_too_fresh(diagnostics)?;
    Some(OutcomeNote::normal(notes::version_too_fresh(
        &latest.version,
    )))
}

fn delayed_note(reason: &DelayReason, diagnostics: &PlanDiagnostics) -> OutcomeNote {
    match reason {
        DelayReason::ReleaseTooFresh => {
            if let Some(target) = diagnostics.selected_target.as_ref() {
                return OutcomeNote::normal(notes::too_fresh(
                    Some(target.age),
                    diagnostics.required_age,
                ));
            }
            if let Some(latest) = latest_too_fresh(diagnostics) {
                return OutcomeNote::normal(notes::no_eligible_latest_too_fresh(&latest.version));
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

fn release_age_note(age: Duration) -> OutcomeNote {
    OutcomeNote::normal(notes::released(age))
}

fn target_release_note(diagnostics: &PlanDiagnostics) -> Option<OutcomeNote> {
    diagnostics
        .selected_target
        .as_ref()
        .map(|target| release_age_note(target.age).with_visibility(OutcomeVisibility::VerboseOnly))
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
        ScanIssue::ExcludedByManagerRule(reason) => manager_rule_reason_text(reason),
    }
}

fn manager_rule_reason_text(reason: &ManagerRuleReason) -> String {
    match reason {
        ManagerRuleReason::Dependency => "dependency".to_owned(),
        ManagerRuleReason::DefaultGem => "default gem".to_owned(),
        ManagerRuleReason::Other { detail } => detail.clone(),
    }
}
