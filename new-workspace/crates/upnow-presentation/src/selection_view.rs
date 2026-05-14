use upnow_domain::{
    AdvisoryLatestFact, BlockReason, CandidateAgeFact, CandidateEvaluationFact, DelayReason,
    ManagerId, PackageName, PlanDiagnostics, PlanItem, PlanItemId, PolicyBlockReason,
    PolicyWarning, ReleaseLookupError, SkipReason, UpdateCandidate, UpdatePlan,
    UpdateSelectionPolicy, VersionText,
};

use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionView {
    pub manager_id: ManagerId,
    pub rows: Vec<SelectionRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionRow {
    pub plan_item_id: PlanItemId,
    pub package_name: PackageName,
    pub installed_version: VersionText,
    pub target_version: Option<VersionText>,
    pub status: SelectionRowStatus,
    pub default_visibility: SelectionRowVisibility,
    pub notes: Vec<CandidateNotePart>,
    pub initially_selected: bool,
    pub policy_exception: bool,
    pub target_options: Vec<TargetOption>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionRowStatus {
    Update,
    Current,
    Delayed,
    Blocked,
    Skipped,
    ResolverError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionRowVisibility {
    Visible,
    HiddenUntilViewAll,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetOption {
    Recommended {
        target_version: VersionText,
        note_parts: Vec<CandidateNotePart>,
    },
    ForcedCandidate {
        target_version: VersionText,
        note_parts: Vec<CandidateNotePart>,
    },
    AlternateExact {
        target_version: VersionText,
        note_parts: Vec<CandidateNotePart>,
    },
}

impl TargetOption {
    pub const fn target_version(&self) -> &VersionText {
        match self {
            Self::Recommended { target_version, .. }
            | Self::ForcedCandidate { target_version, .. }
            | Self::AlternateExact { target_version, .. } => target_version,
        }
    }
    pub fn note_parts(&self) -> &[CandidateNotePart] {
        match self {
            Self::Recommended { note_parts, .. }
            | Self::ForcedCandidate { note_parts, .. }
            | Self::AlternateExact { note_parts, .. } => note_parts,
        }
    }
    pub fn has_violation(&self) -> bool {
        self.note_parts().iter().any(|part| part.violation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateNotePart {
    pub kind: CandidateNoteKind,
    pub violation: bool,
}

impl CandidateNotePart {
    pub const fn normal(kind: CandidateNoteKind) -> Self {
        Self {
            kind,
            violation: false,
        }
    }
    pub const fn violation(kind: CandidateNoteKind) -> Self {
        Self {
            kind,
            violation: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateNoteKind {
    Released {
        age: Duration,
    },
    TooFresh {
        age: Option<Duration>,
        required_age: Duration,
    },
    VersionPolicyBlocked(PolicyBlockReason),
    PolicyWarning(PolicyWarning),
    MissingReleaseMetadata,
    ReleaseLookupFailed {
        error: Option<ReleaseLookupError>,
    },
    Skipped(SkipReason),
    ResolverError {
        message: String,
    },
}
pub fn selection_view(
    plan: &UpdatePlan,
    selection_policy: &UpdateSelectionPolicy,
) -> SelectionView {
    let rows = plan
        .items
        .iter()
        .map(|item| selection_row(item, selection_policy))
        .collect();

    SelectionView {
        manager_id: plan.manager_id.clone(),
        rows,
    }
}

#[expect(clippy::too_many_lines)]
fn selection_row(item: &PlanItem, selection_policy: &UpdateSelectionPolicy) -> SelectionRow {
    match item {
        PlanItem::Update { id, candidate } => {
            let selected = selection_policy.includes(&candidate.package_name);
            let notes = update_notes(candidate);
            let target_options = update_target_options(candidate, notes.clone());
            SelectionRow {
                plan_item_id: id.clone(),
                package_name: candidate.package_name.clone(),
                installed_version: candidate.installed_version.clone(),
                target_version: Some(candidate.target_version.clone()),
                status: SelectionRowStatus::Update,
                default_visibility: SelectionRowVisibility::Visible,
                notes,
                initially_selected: selected,
                policy_exception: selection_policy.except.contains(&candidate.package_name),
                target_options,
            }
        }
        PlanItem::Current { id, installed } => SelectionRow {
            plan_item_id: id.clone(),
            package_name: installed.package_name.clone(),
            installed_version: installed.installed_version.clone(),
            target_version: None,
            status: SelectionRowStatus::Current,
            default_visibility: SelectionRowVisibility::HiddenUntilViewAll,
            notes: Vec::new(),
            initially_selected: false,
            policy_exception: selection_policy.except.contains(&installed.package_name),
            target_options: Vec::new(),
        },
        PlanItem::Delayed {
            id,
            candidate,
            reason,
        } => {
            let notes = delayed_notes(reason, &candidate.diagnostics);
            let target_options = delayed_target_options(candidate, notes.clone());
            SelectionRow {
                plan_item_id: id.clone(),
                package_name: candidate.package_name.clone(),
                installed_version: candidate.installed_version.clone(),
                target_version: Some(candidate.target_version.clone()),
                status: SelectionRowStatus::Delayed,
                default_visibility: if target_options.is_empty() {
                    SelectionRowVisibility::HiddenUntilViewAll
                } else {
                    SelectionRowVisibility::Visible
                },
                notes,
                initially_selected: false,
                policy_exception: selection_policy.except.contains(&candidate.package_name),
                target_options,
            }
        }
        PlanItem::Blocked {
            id,
            seed,
            reason,
            policy_warnings,
            diagnostics,
        } => SelectionRow {
            plan_item_id: id.clone(),
            package_name: seed.installed.package_name.clone(),
            installed_version: seed.installed.installed_version.clone(),
            target_version: Some(seed.target_selection.target_version().clone()),
            status: SelectionRowStatus::Blocked,
            default_visibility: SelectionRowVisibility::HiddenUntilViewAll,
            notes: blocked_notes(reason, policy_warnings, diagnostics),
            initially_selected: false,
            policy_exception: selection_policy
                .except
                .contains(&seed.installed.package_name),
            target_options: Vec::new(),
        },
        PlanItem::Skipped {
            id,
            installed,
            reason,
        } => SelectionRow {
            plan_item_id: id.clone(),
            package_name: installed.package_name.clone(),
            installed_version: installed.installed_version.clone(),
            target_version: None,
            status: SelectionRowStatus::Skipped,
            default_visibility: SelectionRowVisibility::HiddenUntilViewAll,
            notes: vec![CandidateNotePart::normal(CandidateNoteKind::Skipped(
                reason.clone(),
            ))],
            initially_selected: false,
            policy_exception: selection_policy.except.contains(&installed.package_name),
            target_options: Vec::new(),
        },
        PlanItem::ResolverError {
            id,
            installed,
            message,
        } => SelectionRow {
            plan_item_id: id.clone(),
            package_name: installed.package_name.clone(),
            installed_version: installed.installed_version.clone(),
            target_version: None,
            status: SelectionRowStatus::ResolverError,
            default_visibility: SelectionRowVisibility::HiddenUntilViewAll,
            notes: vec![CandidateNotePart::violation(
                CandidateNoteKind::ResolverError {
                    message: message.clone(),
                },
            )],
            initially_selected: false,
            policy_exception: selection_policy.except.contains(&installed.package_name),
            target_options: Vec::new(),
        },
    }
}

fn update_target_options(
    candidate: &UpdateCandidate,
    notes: Vec<CandidateNotePart>,
) -> Vec<TargetOption> {
    let mut options = vec![TargetOption::Recommended {
        target_version: candidate.target_version.clone(),
        note_parts: notes.clone(),
    }];
    if candidate.execution_eligibility.supports_exact_target() {
        let exact_options = exact_target_options(candidate, notes);
        options.extend(exact_options);
    }
    options
}

fn delayed_target_options(
    candidate: &UpdateCandidate,
    notes: Vec<CandidateNotePart>,
) -> Vec<TargetOption> {
    if candidate.execution_eligibility.supports_exact_target() {
        return vec![TargetOption::ForcedCandidate {
            target_version: candidate.target_version.clone(),
            note_parts: notes,
        }];
    }
    Vec::new()
}

fn exact_target_options(
    candidate: &UpdateCandidate,
    fallback_notes: Vec<CandidateNotePart>,
) -> Vec<TargetOption> {
    if candidate.diagnostics.candidates.is_empty() {
        return vec![TargetOption::AlternateExact {
            target_version: candidate.target_version.clone(),
            note_parts: fallback_notes,
        }];
    }

    candidate
        .diagnostics
        .candidates
        .iter()
        .map(|evaluated| TargetOption::AlternateExact {
            target_version: evaluated.version.clone(),
            note_parts: candidate_evaluation_notes(evaluated, candidate.diagnostics.required_age),
        })
        .collect()
}

fn update_notes(candidate: &UpdateCandidate) -> Vec<CandidateNotePart> {
    let mut notes = Vec::new();
    if let Some(target) = candidate.diagnostics.selected_target.as_ref() {
        notes.push(CandidateNotePart::normal(CandidateNoteKind::Released {
            age: target.age,
        }));
    }
    if let Some(latest) = latest_too_fresh(&candidate.diagnostics) {
        notes.push(CandidateNotePart::normal(CandidateNoteKind::TooFresh {
            age: Some(latest.age),
            required_age: candidate.diagnostics.required_age,
        }));
    }
    notes.extend(policy_notes(&candidate.diagnostics));
    notes.extend(
        candidate
            .policy_warnings
            .iter()
            .copied()
            .map(|warning| CandidateNotePart::normal(CandidateNoteKind::PolicyWarning(warning))),
    );
    notes
}

fn candidate_evaluation_notes(
    candidate: &CandidateEvaluationFact,
    required_age: Duration,
) -> Vec<CandidateNotePart> {
    let mut notes = Vec::new();
    if let Some(age) = candidate.age {
        notes.push(CandidateNotePart::normal(CandidateNoteKind::Released {
            age,
        }));
    }
    if !candidate.age_allowed {
        notes.push(CandidateNotePart::violation(CandidateNoteKind::TooFresh {
            age: candidate.age,
            required_age,
        }));
    }
    if let Some(reason) = candidate.policy_block_reason.clone() {
        notes.push(CandidateNotePart::violation(
            CandidateNoteKind::VersionPolicyBlocked(reason),
        ));
    }
    if let Some(warning) = candidate.policy_warning {
        notes.push(CandidateNotePart::normal(CandidateNoteKind::PolicyWarning(
            warning,
        )));
    }
    notes
}

fn delayed_notes(reason: &DelayReason, diagnostics: &PlanDiagnostics) -> Vec<CandidateNotePart> {
    match reason {
        DelayReason::ReleaseTooFresh => {
            let mut notes = Vec::new();
            if let Some(target) = diagnostics.selected_target.as_ref() {
                notes.push(CandidateNotePart::normal(CandidateNoteKind::Released {
                    age: target.age,
                }));
            }
            notes.push(CandidateNotePart::violation(CandidateNoteKind::TooFresh {
                age: diagnostics
                    .selected_target
                    .as_ref()
                    .map(|target| target.age),
                required_age: diagnostics.required_age,
            }));
            notes
        }
    }
}

fn blocked_notes(
    reason: &BlockReason,
    policy_warnings: &[PolicyWarning],
    diagnostics: &PlanDiagnostics,
) -> Vec<CandidateNotePart> {
    let mut notes = match reason {
        BlockReason::MissingReleaseMetadata => {
            vec![CandidateNotePart::violation(
                CandidateNoteKind::MissingReleaseMetadata,
            )]
        }
        BlockReason::ReleaseLookupFailed => {
            vec![CandidateNotePart::violation(
                CandidateNoteKind::ReleaseLookupFailed {
                    error: diagnostics.lookup_failure.clone(),
                },
            )]
        }
        BlockReason::VersionPolicy(reason) => vec![CandidateNotePart::violation(
            CandidateNoteKind::VersionPolicyBlocked(reason.clone()),
        )],
    };
    notes.extend(policy_notes(diagnostics));
    notes.extend(
        policy_warnings
            .iter()
            .copied()
            .map(|warning| CandidateNotePart::normal(CandidateNoteKind::PolicyWarning(warning))),
    );
    notes
}

fn policy_notes(diagnostics: &PlanDiagnostics) -> Vec<CandidateNotePart> {
    diagnostics
        .candidates
        .iter()
        .filter_map(|candidate| {
            candidate
                .policy_block_reason
                .clone()
                .map(CandidateNoteKind::VersionPolicyBlocked)
                .map(CandidateNotePart::violation)
        })
        .collect()
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
