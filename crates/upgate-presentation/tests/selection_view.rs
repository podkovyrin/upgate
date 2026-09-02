use std::time::Duration;

use upgate_domain::{
    AuditFinding, AuditLookupResult, BlockReason, CandidateEvaluationFact, DelayReason,
    ExecutionSupport, ExecutionTargetKind, InstalledTool, ManagerId, PackageName, PlanDiagnostics,
    PlanItem, PlanItemId, ReleaseLookupResult, ToolId, ToolName, UpdateCandidate, UpdatePlan,
    UpdateSeed, UpdateSelectionPolicy, VersionScheme, VersionText,
};
use upgate_presentation::{TargetOption, selection_view};

fn version(value: &str) -> VersionText {
    VersionText::new(value).expect("valid version")
}

fn manager_id() -> ManagerId {
    ManagerId::new("npm").expect("valid manager id")
}

fn installed_tool(package: &str) -> InstalledTool {
    InstalledTool::new(
        manager_id(),
        ToolId::new(package).expect("valid tool id"),
        PackageName::new(package).expect("valid package name"),
        ToolName::new(package).expect("valid tool name"),
        version("1.0.0"),
    )
}

fn finding(id: &str) -> AuditFinding {
    AuditFinding {
        id: id.to_owned(),
        aliases: Vec::new(),
        summary: None,
        severity: None,
        references: Vec::new(),
    }
}

#[test]
fn audit_blocked_picker_option_uses_the_blocked_candidate_version() {
    let audit = AuditLookupResult::Vulnerable {
        findings: vec![finding("GHSA-alpha")],
    };
    let audit_blocking_candidate = CandidateEvaluationFact {
        version: version("2.0.0"),
        age: Some(Duration::from_secs(500)),
        age_allowed: true,
        policy_block_reason: None,
        policy_warning: None,
        audit: Some(audit.clone()),
    };
    let seed = UpdateSeed::new(
        installed_tool("alpha"),
        version("3.0.0"),
        VersionScheme::SemVer,
        ReleaseLookupResult::MissingMetadata,
        ExecutionSupport::exact_only(),
    );
    let plan = UpdatePlan::new(
        manager_id(),
        vec![PlanItem::Blocked {
            id: PlanItemId::new("npm:alpha").expect("valid plan item id"),
            seed,
            reason: BlockReason::AuditVulnerable,
            policy_warnings: Vec::new(),
            diagnostics: PlanDiagnostics {
                candidates: vec![audit_blocking_candidate.clone()],
                audit_blocking_target: Some(audit),
                audit_blocking_candidate: Some(audit_blocking_candidate),
                ..PlanDiagnostics::new(Duration::ZERO)
            },
        }],
    )
    .expect("valid plan");

    let view = selection_view(&plan, &UpdateSelectionPolicy::include_all());
    let option = view.rows[0]
        .target_options
        .first()
        .expect("audit-blocked target option");

    assert!(matches!(
        option,
        TargetOption::ForcedCandidate { target_version, .. } if target_version.as_str() == "2.0.0"
    ));
    assert!(option.has_violation());
}

#[test]
fn delayed_native_brew_row_exposes_a_forced_target_option() {
    let candidate = UpdateCandidate::new(
        ToolId::new("wget").expect("valid tool id"),
        PackageName::new("wget").expect("valid package name"),
        version("1.0.0"),
        version("1.2.0"),
        VersionScheme::ManagerNative,
        ExecutionSupport::native_with_age_bypass_only(),
    )
    .with_execution_target_kind(ExecutionTargetKind::BrewFormula)
    .with_diagnostics(PlanDiagnostics {
        required_age: Duration::from_secs(7 * 24 * 60 * 60),
        ..PlanDiagnostics::new(Duration::from_secs(7 * 24 * 60 * 60))
    });
    let plan = UpdatePlan::new(
        ManagerId::new("brew").expect("valid manager id"),
        vec![PlanItem::Delayed {
            id: PlanItemId::new("brew:wget").expect("valid plan item id"),
            candidate,
            reason: DelayReason::ReleaseTooFresh,
        }],
    )
    .expect("valid plan");

    let view = selection_view(&plan, &UpdateSelectionPolicy::include_all());
    let option = view.rows[0]
        .target_options
        .first()
        .expect("too-fresh brew row should expose a forced target option");

    assert!(matches!(
        option,
        TargetOption::ForcedCandidate { target_version, .. } if target_version.as_str() == "1.2.0"
    ));
}
