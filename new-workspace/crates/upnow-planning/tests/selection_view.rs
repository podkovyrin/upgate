use upnow_domain::{
    DelayReason, ExecutionEligibility, ManagerId, PackageName, PlanItem, PlanItemId, ToolId,
    UpdateCandidate, UpdatePlan, UpdateSelectionMode, UpdateSelectionPolicy, VersionScheme,
    VersionText,
};
use upnow_planning::{SelectionRowStatus, selection_view};

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
    assert!(exact.forced_candidate_available);
    assert!(!native.forced_candidate_available);
}

#[test]
fn alternate_exact_targets_are_sourced_from_typed_plan_target() {
    let plan = plan(vec![
        update("pnpm:exact", "exact", ExecutionEligibility::ExactOnly),
        update("pnpm:native", "native", ExecutionEligibility::NativeOnly),
    ]);
    let view = selection_view(&plan, &UpdateSelectionPolicy::default());

    let exact = row(&view.rows, "pnpm:exact");
    let native = row(&view.rows, "pnpm:native");

    assert_eq!(
        exact.alternate_exact_targets,
        vec![VersionText::new("1.2.0").expect("valid version")]
    );
    assert!(native.alternate_exact_targets.is_empty());
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

fn plan_item_id(id: &str) -> PlanItemId {
    PlanItemId::new(id).expect("valid plan item id")
}
