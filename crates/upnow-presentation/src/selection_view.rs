use upnow_domain::{
    AdvisoryLatestFact, BlockReason, CandidateAgeFact, CandidateEvaluationFact, DelayReason,
    ManagerId, MissingMetadataKind, PackageName, PlanDiagnostics, PlanItem, PlanItemId,
    PolicyBlockReason, PolicyWarning, ReleaseLookupError, SkipReason, UpdateCandidate, UpdatePlan,
    UpdateSeed, UpdateSelectionPolicy, VersionText,
};

use std::time::Duration;

use crate::notes;

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
/// Typed action option shown in the details picker for a real plan row.
///
/// Despite the current name, this is not display text. Each variant maps back
/// to a typed apply selection.
pub enum TargetOption {
    /// Apply the plan's normal recommended target.
    Recommended {
        target_version: VersionText,
        note_parts: Vec<CandidateNotePart>,
    },
    /// Apply the planned candidate even though normal gates did not select it.
    ForcedCandidate {
        target_version: VersionText,
        note_parts: Vec<CandidateNotePart>,
    },
    /// Apply a specific exact target produced from typed candidate diagnostics.
    AlternateExact {
        target_version: VersionText,
        note_parts: Vec<CandidateNotePart>,
    },
    /// Let the manager choose the final target for this selected tool.
    ManagerResolved { note_parts: Vec<CandidateNotePart> },
}

impl TargetOption {
    pub const fn target_version(&self) -> Option<&VersionText> {
        match self {
            Self::Recommended { target_version, .. }
            | Self::ForcedCandidate { target_version, .. }
            | Self::AlternateExact { target_version, .. } => Some(target_version),
            Self::ManagerResolved { .. } => None,
        }
    }
    pub fn note_parts(&self) -> &[CandidateNotePart] {
        match self {
            Self::Recommended { note_parts, .. }
            | Self::ForcedCandidate { note_parts, .. }
            | Self::AlternateExact { note_parts, .. }
            | Self::ManagerResolved { note_parts } => note_parts,
        }
    }
    pub fn has_violation(&self) -> bool {
        self.note_parts()
            .iter()
            .any(CandidateNotePart::is_violation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateNotePart {
    pub kind: CandidateNoteKind,
    pub tone: CandidateNoteTone,
}

impl CandidateNotePart {
    pub const fn normal(kind: CandidateNoteKind) -> Self {
        Self {
            kind,
            tone: CandidateNoteTone::Normal,
        }
    }
    pub const fn metadata(kind: CandidateNoteKind) -> Self {
        Self {
            kind,
            tone: CandidateNoteTone::Metadata,
        }
    }
    pub const fn violation(kind: CandidateNoteKind) -> Self {
        Self {
            kind,
            tone: CandidateNoteTone::Violation,
        }
    }
    pub const fn is_violation(&self) -> bool {
        matches!(self.tone, CandidateNoteTone::Violation)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateNoteTone {
    Normal,
    Metadata,
    Violation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateNoteKind {
    Released {
        age: Duration,
    },
    TooFresh {
        version: Option<VersionText>,
        age: Option<Duration>,
        required_age: Duration,
    },
    VersionPolicyBlocked(PolicyBlockReason),
    PolicyWarning(PolicyWarning),
    MissingReleaseMetadata,
    ReleaseLookupFailed {
        error: Option<ReleaseLookupError>,
    },
    AdvisoryLookupFailed {
        error: ReleaseLookupError,
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
                target_version: candidate.target_version().cloned(),
                status: SelectionRowStatus::Update,
                default_visibility: SelectionRowVisibility::Visible,
                notes,
                initially_selected: selected,
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
                target_version: candidate.target_version().cloned(),
                status: SelectionRowStatus::Delayed,
                default_visibility: if target_options.is_empty() {
                    SelectionRowVisibility::HiddenUntilViewAll
                } else {
                    SelectionRowVisibility::Visible
                },
                notes,
                initially_selected: false,
                target_options,
            }
        }
        PlanItem::Blocked {
            id,
            seed,
            reason,
            policy_warnings,
            diagnostics,
        } => {
            let notes = blocked_notes(reason, policy_warnings, diagnostics);
            let target_options = blocked_target_options(seed, reason, notes.clone(), diagnostics);
            SelectionRow {
                plan_item_id: id.clone(),
                package_name: seed.installed.package_name.clone(),
                installed_version: seed.installed.installed_version.clone(),
                target_version: seed.target_selection.target_version().cloned(),
                status: SelectionRowStatus::Blocked,
                default_visibility: SelectionRowVisibility::HiddenUntilViewAll,
                notes,
                initially_selected: false,
                target_options,
            }
        }
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
            target_options: Vec::new(),
        },
    }
}

fn update_target_options(
    candidate: &UpdateCandidate,
    notes: Vec<CandidateNotePart>,
) -> Vec<TargetOption> {
    primary_target_options(candidate, notes, TargetOptionKind::Recommended)
}

fn delayed_target_options(
    candidate: &UpdateCandidate,
    notes: Vec<CandidateNotePart>,
) -> Vec<TargetOption> {
    if candidate.execution_support.supports_age_bypass() {
        let Some(target_version) = candidate.target_version().cloned() else {
            if candidate
                .execution_support
                .supports_manager_resolved_target()
            {
                return vec![TargetOption::ManagerResolved { note_parts: notes }];
            }
            return Vec::new();
        };
        return target_options_for_known_primary(
            candidate,
            target_version,
            notes,
            TargetOptionKind::ForcedCandidate,
        );
    }
    Vec::new()
}

fn blocked_target_options(
    seed: &UpdateSeed,
    reason: &BlockReason,
    notes: Vec<CandidateNotePart>,
    diagnostics: &PlanDiagnostics,
) -> Vec<TargetOption> {
    if matches!(reason, BlockReason::MissingReleaseMetadata)
        && diagnostics.missing_metadata == Some(MissingMetadataKind::SelectedUpdate)
        && seed.execution_support.supports_manager_resolved_target()
    {
        return vec![TargetOption::ManagerResolved { note_parts: notes }];
    }

    let Some(target_version) = seed.target_selection.target_version().cloned() else {
        return match reason {
            BlockReason::VersionPolicy(_) | BlockReason::MissingReleaseMetadata
                if seed.execution_support.supports_manager_resolved_target() =>
            {
                vec![TargetOption::ManagerResolved { note_parts: notes }]
            }
            _ => Vec::new(),
        };
    };

    if !matches!(reason, BlockReason::VersionPolicy(_))
        || !seed.execution_support.supports_age_bypass()
    {
        return Vec::new();
    }

    let candidate = UpdateCandidate::new(
        seed.installed.tool_id.clone(),
        seed.installed.package_name.clone(),
        seed.installed.installed_version.clone(),
        target_version.clone(),
        seed.version_scheme,
        seed.execution_support,
    )
    .with_execution_target_kind(seed.execution_target_kind)
    .with_diagnostics(diagnostics.clone());

    target_options_for_known_primary(
        &candidate,
        target_version,
        notes,
        TargetOptionKind::ForcedCandidate,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetOptionKind {
    Recommended,
    ForcedCandidate,
}

fn primary_target_options(
    candidate: &UpdateCandidate,
    notes: Vec<CandidateNotePart>,
    kind: TargetOptionKind,
) -> Vec<TargetOption> {
    let Some(target_version) = candidate.target_version().cloned() else {
        if candidate
            .execution_support
            .supports_manager_resolved_target()
        {
            return vec![TargetOption::ManagerResolved { note_parts: notes }];
        }
        return Vec::new();
    };

    target_options_for_known_primary(candidate, target_version, notes, kind)
}

fn target_options_for_known_primary(
    candidate: &UpdateCandidate,
    target_version: VersionText,
    notes: Vec<CandidateNotePart>,
    kind: TargetOptionKind,
) -> Vec<TargetOption> {
    let mut options = if candidate.execution_support.supports_exact_target() {
        exact_target_options(candidate)
            .into_iter()
            .map(|option| match option {
                TargetOption::AlternateExact {
                    target_version: exact_target,
                    note_parts,
                } if exact_target == target_version => {
                    primary_option(kind, exact_target, note_parts)
                }
                option => option,
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    if options.is_empty() {
        options.push(primary_option(kind, target_version, notes));
    }

    options
}

const fn primary_option(
    kind: TargetOptionKind,
    target_version: VersionText,
    note_parts: Vec<CandidateNotePart>,
) -> TargetOption {
    match kind {
        TargetOptionKind::Recommended => TargetOption::Recommended {
            target_version,
            note_parts,
        },
        TargetOptionKind::ForcedCandidate => TargetOption::ForcedCandidate {
            target_version,
            note_parts,
        },
    }
}

fn exact_target_options(candidate: &UpdateCandidate) -> Vec<TargetOption> {
    if candidate.diagnostics.candidates.is_empty() {
        return Vec::new();
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
        notes.push(CandidateNotePart::metadata(CandidateNoteKind::Released {
            age: target.age,
        }));
    }
    if let Some(latest) = latest_too_fresh(&candidate.diagnostics) {
        notes.push(CandidateNotePart::metadata(CandidateNoteKind::TooFresh {
            version: Some(latest.version.clone()),
            age: Some(latest.age),
            required_age: candidate.diagnostics.required_age,
        }));
    }
    notes.extend(policy_notes(&candidate.diagnostics));
    notes.extend(advisory_warning_notes(&candidate.diagnostics));
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
    match candidate.age {
        Some(age) if candidate.age_allowed => {
            notes.push(CandidateNotePart::metadata(CandidateNoteKind::Released {
                age,
            }));
        }
        Some(age) => {
            notes.push(CandidateNotePart::violation(CandidateNoteKind::TooFresh {
                version: None,
                age: Some(age),
                required_age,
            }));
        }
        None => {}
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
            notes.push(CandidateNotePart::violation(CandidateNoteKind::TooFresh {
                version: None,
                age: diagnostics
                    .selected_target
                    .as_ref()
                    .map(|target| target.age),
                required_age: diagnostics.required_age,
            }));
            notes.extend(advisory_warning_notes(diagnostics));
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
            vec![CandidateNotePart::metadata(
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
    notes.extend(advisory_warning_notes(diagnostics));
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

fn advisory_warning_notes(diagnostics: &PlanDiagnostics) -> Vec<CandidateNotePart> {
    let mut notes = Vec::new();
    if let Some(error) = diagnostics.advisory_lookup_failure.as_ref() {
        notes.push(CandidateNotePart::normal(
            CandidateNoteKind::AdvisoryLookupFailed {
                error: error.clone(),
            },
        ));
    }
    if let Some(AdvisoryLatestFact::LookupFailed { error, .. }) =
        diagnostics.advisory_latest.as_ref()
    {
        notes.push(CandidateNotePart::normal(
            CandidateNoteKind::AdvisoryLookupFailed {
                error: error.clone(),
            },
        ));
    }
    notes
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

pub(crate) fn note_part_text(part: &CandidateNotePart) -> String {
    match &part.kind {
        CandidateNoteKind::Released { age } => notes::released(*age),
        CandidateNoteKind::TooFresh {
            version,
            age,
            required_age,
        } => version.as_ref().map_or_else(
            || notes::too_fresh(*age, *required_age),
            |version| notes::latest_too_fresh(version, *age, Some(*required_age), true),
        ),
        CandidateNoteKind::VersionPolicyBlocked(reason) => notes::version_policy_blocked(reason),
        CandidateNoteKind::PolicyWarning(warning) => notes::policy_warning(*warning).to_owned(),
        CandidateNoteKind::MissingReleaseMetadata => "missing release metadata".to_owned(),
        CandidateNoteKind::ReleaseLookupFailed { error } => error.as_ref().map_or_else(
            || "release lookup failed".to_owned(),
            |error| format!("release lookup failed: {}", error.detail),
        ),
        CandidateNoteKind::AdvisoryLookupFailed { error } => {
            format!("advisory latest lookup failed: {}", error.detail)
        }
        CandidateNoteKind::Skipped(reason) => notes::skip_reason(reason),
        CandidateNoteKind::ResolverError { message } => message.clone(),
    }
}
