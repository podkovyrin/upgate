use upnow_domain::{
    AdvisoryLatestFact, BlockReason, CandidateAgeFact, CandidateAgeSource, DelayReason,
    ExecutionEligibility, ManagerId, PackageName, PlanDiagnostics, PlanItem, PlanItemId,
    PolicyWarning, ReleaseLookupError, ReleaseLookupResult, SkipReason, ToolId, UpdateCandidate,
    UpdatePlan, UpdateSeed, UpdateSelectionMode, UpdateSelectionPolicy, VersionScheme, VersionText,
};
use upnow_planning::{
    CandidateNoteKind, SelectionRowStatus, SelectionRowVisibility, TargetOption, selection_view,
};

use std::time::Duration;

#[test]
fn include_mode_exception_starts_unselected() {
    let plan = plan(vec![
        update("pnpm:alpha", "alpha", ExecutionEligibility::NativeOrExact),
        update("pnpm:pinned", "pinned", ExecutionEligibility::NativeOrExact),
    ]);
    let policy = UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Include,
        except: [package("pinned")].into_iter().collect(),
    };
    let view = selection_view(&plan, &policy);

    let alpha = row(&view.rows, "pnpm:alpha");
    let pinned = row(&view.rows, "pnpm:pinned");

    assert!(alpha.initially_selected);
    assert!(!alpha.policy_exception);
    assert!(!pinned.initially_selected);
    assert!(pinned.policy_exception);
}

#[test]
fn skip_mode_exception_starts_selected() {
    let plan = plan(vec![update(
        "pnpm:alpha",
        "alpha",
        ExecutionEligibility::NativeOrExact,
    )]);
    let policy = UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Skip,
        except: [package("alpha")].into_iter().collect(),
    };
    let view = selection_view(&plan, &policy);

    let alpha = row(&view.rows, "pnpm:alpha");

    assert!(alpha.initially_selected);
    assert!(alpha.policy_exception);
}

#[test]
fn forced_candidates_require_exact_execution_support() {
    let plan = plan(vec![
        delayed("pnpm:exact", "exact", ExecutionEligibility::ExactOnly),
        delayed("pnpm:native", "native", ExecutionEligibility::NativeOnly),
    ]);
    let view = selection_view(&plan, &UpdateSelectionPolicy::default());

    let exact = row(&view.rows, "pnpm:exact");
    let native = row(&view.rows, "pnpm:native");

    assert_eq!(exact.status, SelectionRowStatus::Delayed);
    assert_eq!(exact.default_visibility, SelectionRowVisibility::Visible);
    assert_eq!(exact.target_options.len(), 1);
    assert!(matches!(
        exact.target_options[0],
        TargetOption::ForcedCandidate { .. }
    ));
    assert!(exact.target_options[0].has_violation());
    assert_eq!(
        native.default_visibility,
        SelectionRowVisibility::HiddenUntilViewAll
    );
    assert!(native.target_options.is_empty());
}

#[test]
fn target_options_are_sourced_from_typed_plan_target() {
    let plan = plan(vec![
        update("pnpm:exact", "exact", ExecutionEligibility::ExactOnly),
        update("pnpm:native", "native", ExecutionEligibility::NativeOnly),
    ]);
    let view = selection_view(&plan, &UpdateSelectionPolicy::default());

    let exact = row(&view.rows, "pnpm:exact");
    let native = row(&view.rows, "pnpm:native");

    assert_eq!(exact.target_options.len(), 2);
    assert!(matches!(
        exact.target_options[0],
        TargetOption::Recommended { .. }
    ));
    assert!(matches!(
        exact.target_options[1],
        TargetOption::AlternateExact { .. }
    ));
    assert_eq!(exact.target_options[1].target_version().as_str(), "1.2.0");
    assert_eq!(native.target_options.len(), 1);
    assert!(matches!(
        native.target_options[0],
        TargetOption::Recommended { .. }
    ));
}

#[test]
fn row_visibility_matches_selection_surface_rules() {
    let plan = plan(vec![
        update("pnpm:update", "update", ExecutionEligibility::NativeOnly),
        current("pnpm:current", "current"),
        delayed("pnpm:delayed", "delayed", ExecutionEligibility::NativeOnly),
        blocked("pnpm:blocked", "blocked"),
        skipped("pnpm:skipped", "skipped"),
        resolver_error("pnpm:error", "error"),
    ]);
    let view = selection_view(&plan, &UpdateSelectionPolicy::default());

    assert_eq!(
        row(&view.rows, "pnpm:update").default_visibility,
        SelectionRowVisibility::Visible
    );
    assert_eq!(
        row(&view.rows, "pnpm:current").default_visibility,
        SelectionRowVisibility::HiddenUntilViewAll
    );
    assert_eq!(
        row(&view.rows, "pnpm:delayed").default_visibility,
        SelectionRowVisibility::HiddenUntilViewAll
    );
    assert_eq!(
        row(&view.rows, "pnpm:blocked").default_visibility,
        SelectionRowVisibility::HiddenUntilViewAll
    );
    assert_eq!(
        row(&view.rows, "pnpm:skipped").default_visibility,
        SelectionRowVisibility::HiddenUntilViewAll
    );
    assert_eq!(
        row(&view.rows, "pnpm:error").default_visibility,
        SelectionRowVisibility::HiddenUntilViewAll
    );
}

#[test]
fn typed_note_parts_preserve_violation_flags() {
    let diagnostics = PlanDiagnostics {
        required_age: Duration::from_secs(7 * 24 * 60 * 60),
        selected_target: Some(CandidateAgeFact::new(
            VersionText::new("1.2.0").expect("valid target"),
            Duration::from_secs(24 * 60 * 60),
            CandidateAgeSource::ReleaseTimeline,
        )),
        ..PlanDiagnostics::default()
    };
    let plan = plan(vec![PlanItem::Delayed {
        id: plan_item_id("pnpm:alpha"),
        candidate: candidate("alpha", ExecutionEligibility::ExactOnly)
            .with_diagnostics(diagnostics),
        reason: DelayReason::ReleaseTooFresh,
    }]);
    let view = selection_view(&plan, &UpdateSelectionPolicy::default());
    let alpha = row(&view.rows, "pnpm:alpha");

    assert_eq!(alpha.notes.len(), 2);
    assert!(!alpha.notes[0].violation);
    assert!(alpha.notes[1].violation);
    assert!(matches!(
        alpha.notes[1].kind,
        CandidateNoteKind::TooFresh {
            age: Some(_),
            required_age
        } if required_age == Duration::from_secs(7 * 24 * 60 * 60)
    ));
}

#[test]
fn update_notes_include_released_and_advisory_latest_too_fresh_facts() {
    let diagnostics = PlanDiagnostics {
        required_age: Duration::from_secs(7 * 24 * 60 * 60),
        selected_target: Some(CandidateAgeFact::new(
            VersionText::new("1.2.0").expect("valid selected target"),
            Duration::from_secs(9 * 24 * 60 * 60),
            CandidateAgeSource::ReleaseTimeline,
        )),
        advisory_latest: Some(AdvisoryLatestFact::Known {
            latest_version: VersionText::new("1.3.0").expect("valid latest"),
            candidates: vec![CandidateAgeFact::new(
                VersionText::new("1.3.0").expect("valid latest"),
                Duration::from_secs(24 * 60 * 60),
                CandidateAgeSource::ReleaseTimeline,
            )],
        }),
        ..PlanDiagnostics::default()
    };
    let plan = plan(vec![PlanItem::Update {
        id: plan_item_id("pnpm:alpha"),
        candidate: candidate("alpha", ExecutionEligibility::ExactOnly)
            .with_diagnostics(diagnostics),
    }]);
    let view = selection_view(&plan, &UpdateSelectionPolicy::default());
    let alpha = row(&view.rows, "pnpm:alpha");

    assert!(matches!(
        alpha.notes[0].kind,
        CandidateNoteKind::Released { age }
            if age == Duration::from_secs(9 * 24 * 60 * 60)
    ));
    assert!(alpha.notes.iter().any(|note| matches!(
        note.kind,
        CandidateNoteKind::TooFresh {
            age: Some(age),
            required_age
        } if age == Duration::from_secs(24 * 60 * 60)
            && required_age == Duration::from_secs(7 * 24 * 60 * 60)
    )));
    assert_eq!(alpha.target_options[0].note_parts(), alpha.notes.as_slice());
}

#[test]
fn policy_warning_notes_are_typed() {
    let plan = plan(vec![PlanItem::Update {
        id: plan_item_id("pnpm:alpha"),
        candidate: candidate("alpha", ExecutionEligibility::NativeOnly)
            .with_policy_warnings(vec![PolicyWarning::InstalledTrackUnknownFallbackStable]),
    }]);
    let view = selection_view(&plan, &UpdateSelectionPolicy::default());
    let alpha = row(&view.rows, "pnpm:alpha");

    assert!(alpha.notes.iter().any(|note| matches!(
        note.kind,
        CandidateNoteKind::PolicyWarning(PolicyWarning::InstalledTrackUnknownFallbackStable)
    )));
}

#[test]
fn lookup_failure_notes_preserve_typed_error_detail() {
    let plan = plan(vec![PlanItem::Blocked {
        id: plan_item_id("pnpm:alpha"),
        seed: UpdateSeed::new(
            installed("alpha"),
            VersionText::new("1.2.0").expect("valid target version"),
            VersionScheme::SemVer,
            ReleaseLookupResult::LookupFailed(ReleaseLookupError::new("registry timeout")),
            ExecutionEligibility::NativeOnly,
        ),
        reason: BlockReason::ReleaseLookupFailed,
        policy_warnings: Vec::new(),
        diagnostics: PlanDiagnostics::default()
            .with_lookup_failure(ReleaseLookupError::new("registry timeout")),
    }]);
    let view = selection_view(&plan, &UpdateSelectionPolicy::default());
    let alpha = row(&view.rows, "pnpm:alpha");

    assert!(matches!(
        &alpha.notes[0].kind,
        CandidateNoteKind::ReleaseLookupFailed {
            error: Some(error)
        } if error.detail == "registry timeout"
    ));
    assert!(alpha.notes[0].violation);
}

fn row<'a>(rows: &'a [upnow_planning::SelectionRow], id: &str) -> &'a upnow_planning::SelectionRow {
    rows.iter()
        .find(|row| row.plan_item_id.as_str() == id)
        .expect("row should exist")
}

fn plan(items: Vec<PlanItem>) -> UpdatePlan {
    UpdatePlan::new(ManagerId::new("pnpm").expect("valid manager"), items).expect("valid plan")
}

fn update(id: &str, name: &str, execution_eligibility: ExecutionEligibility) -> PlanItem {
    PlanItem::Update {
        id: plan_item_id(id),
        candidate: candidate(name, execution_eligibility),
    }
}

fn delayed(id: &str, name: &str, execution_eligibility: ExecutionEligibility) -> PlanItem {
    PlanItem::Delayed {
        id: plan_item_id(id),
        candidate: candidate(name, execution_eligibility),
        reason: DelayReason::ReleaseTooFresh,
    }
}

fn current(id: &str, name: &str) -> PlanItem {
    PlanItem::Current {
        id: plan_item_id(id),
        installed: installed(name),
    }
}

fn blocked(id: &str, name: &str) -> PlanItem {
    PlanItem::Blocked {
        id: plan_item_id(id),
        seed: UpdateSeed::new(
            installed(name),
            VersionText::new("1.2.0").expect("valid target version"),
            VersionScheme::SemVer,
            ReleaseLookupResult::MissingMetadata,
            ExecutionEligibility::NativeOnly,
        ),
        reason: BlockReason::MissingReleaseMetadata,
        policy_warnings: Vec::new(),
        diagnostics: PlanDiagnostics::default(),
    }
}

fn skipped(id: &str, name: &str) -> PlanItem {
    PlanItem::Skipped {
        id: plan_item_id(id),
        installed: installed(name),
        reason: SkipReason::Pinned,
    }
}

fn resolver_error(id: &str, name: &str) -> PlanItem {
    PlanItem::ResolverError {
        id: plan_item_id(id),
        installed: installed(name),
        message: "resolver failed".to_owned(),
    }
}

fn candidate(name: &str, execution_eligibility: ExecutionEligibility) -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new(format!("pnpm:{name}")).expect("valid tool"),
        package(name),
        VersionText::new("1.0.0").expect("valid installed version"),
        VersionText::new("1.2.0").expect("valid target version"),
        VersionScheme::SemVer,
        execution_eligibility,
    )
}

fn package(name: &str) -> PackageName {
    PackageName::new(name).expect("valid package")
}

fn installed(name: &str) -> upnow_domain::InstalledTool {
    upnow_domain::InstalledTool::new(
        ManagerId::new("pnpm").expect("valid manager"),
        ToolId::new(format!("pnpm:{name}")).expect("valid tool"),
        package(name),
        upnow_domain::ToolName::new(name).expect("valid tool name"),
        VersionText::new("1.0.0").expect("valid installed version"),
        upnow_domain::ManagerMetadata::empty(),
    )
}

fn plan_item_id(id: &str) -> PlanItemId {
    PlanItemId::new(id).expect("valid plan item id")
}
