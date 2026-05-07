use std::collections::BTreeSet;

use upnow_domain::{
    ExecutionEligibility, ManagerId, PackageName, PinTarget, PlanItem, PlanItemId, ToolId,
    UpdateCandidate, UpdatePlan, VersionScheme, VersionText,
};
use upnow_planning::default_batch_selection;

fn manager_id() -> ManagerId {
    ManagerId::new("pnpm").expect("valid manager id")
}

fn item_id(value: &str) -> PlanItemId {
    PlanItemId::new(value).expect("valid plan item id")
}

fn candidate(package: &str) -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new(format!("pnpm:{package}")).expect("valid tool id"),
        PackageName::new(package).expect("valid package name"),
        VersionText::new("1.0.0").expect("valid installed version"),
        VersionText::new("1.1.0").expect("valid target version"),
        VersionScheme::SemVer,
        ExecutionEligibility::NativeOrExact,
    )
}

fn plan() -> UpdatePlan {
    UpdatePlan::new(
        manager_id(),
        vec![
            PlanItem::Update {
                id: item_id("pnpm:alpha-ready"),
                candidate: candidate("alpha-ready"),
            },
            PlanItem::Update {
                id: item_id("pnpm:pinned-pkg"),
                candidate: candidate("pinned-pkg"),
            },
        ],
    )
    .expect("plan should be valid")
}

#[test]
fn default_batch_selection_excludes_package_pins() {
    let plan = plan();
    let selection = default_batch_selection(
        &plan,
        &BTreeSet::from([PinTarget::Package(
            PackageName::new("pinned-pkg").expect("valid package name"),
        )]),
    )
    .expect("selection should be valid");

    assert_eq!(selection.selected_items.len(), 1);
    assert_eq!(
        selection.selected_items[0].plan_item_id.as_str(),
        "pnpm:alpha-ready"
    );
}

#[test]
fn default_batch_selection_excludes_all_updates_when_globally_pinned() {
    let plan = plan();
    let selection = default_batch_selection(&plan, &BTreeSet::from([PinTarget::All]))
        .expect("selection should be valid");

    assert!(selection.selected_items.is_empty());
}
