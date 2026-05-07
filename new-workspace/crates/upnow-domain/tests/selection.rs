use upnow_domain::{
    DelayReason, DomainError, ExecutionEligibility, InstalledTool, ManagerId, ManagerMetadata,
    PackageName, PinChange, PinOperation, PinTarget, PlanItem, PlanItemId, PlanSelection,
    SelectedItem, ToolId, ToolName, UpdateCandidate, UpdatePlan, VersionScheme, VersionText,
};

fn plan_item_id(value: &str) -> PlanItemId {
    PlanItemId::new(value).expect("valid plan item id")
}

fn candidate(name: &str, execution_eligibility: ExecutionEligibility) -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new(format!("pnpm:{name}")).expect("valid tool id"),
        PackageName::new(name).expect("valid package name"),
        VersionText::new("1.0.0").expect("valid installed version"),
        VersionText::new("1.2.0").expect("valid target version"),
        VersionScheme::SemVer,
        execution_eligibility,
    )
}

fn plan() -> UpdatePlan {
    UpdatePlan::new(
        ManagerId::new("pnpm").expect("valid manager id"),
        vec![
            PlanItem::Update {
                id: plan_item_id("alpha-ready"),
                candidate: candidate("alpha-ready", ExecutionEligibility::ExactOnly),
            },
            PlanItem::Delayed {
                id: plan_item_id("delayed-native-only"),
                candidate: candidate("delayed-native-only", ExecutionEligibility::NativeOnly),
                reason: DelayReason::ReleaseTooFresh,
            },
            PlanItem::Delayed {
                id: plan_item_id("delayed-exact"),
                candidate: candidate("delayed-exact", ExecutionEligibility::ExactOnly),
                reason: DelayReason::ReleaseTooFresh,
            },
            PlanItem::Current {
                id: plan_item_id("fresh-tool"),
                installed: InstalledTool::new(
                    ManagerId::new("pnpm").expect("valid manager id"),
                    ToolId::new("pnpm:fresh-tool").expect("valid tool id"),
                    PackageName::new("fresh-tool").expect("valid package name"),
                    ToolName::new("fresh-tool").expect("valid tool name"),
                    VersionText::new("2.0.0").expect("valid version"),
                    ManagerMetadata::empty(),
                ),
            },
        ],
    )
    .expect("plan should be valid")
}

#[test]
fn plan_selection_accepts_selected_items_and_pin_changes() {
    let plan = plan();
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::new(plan_item_id("alpha-ready"), true)],
        vec![
            PinChange::new(
                PinTarget::Package(PackageName::new("fresh-tool").expect("valid package name")),
                PinOperation::Pin,
            ),
            PinChange::new(
                PinTarget::Package(PackageName::new("alpha-ready").expect("valid package name")),
                PinOperation::Unpin,
            ),
        ],
    )
    .expect("selection should reference known plan items");

    assert_eq!(
        selection.selected_items[0].plan_item_id.as_str(),
        "alpha-ready"
    );
    assert!(selection.selected_items[0].forced);
    assert_eq!(selection.pin_changes.len(), 2);
}

#[test]
fn plan_selection_accepts_global_pin_changes() {
    let plan = plan();
    let selection = PlanSelection::new(
        &plan,
        Vec::new(),
        vec![PinChange::new(PinTarget::All, PinOperation::Unpin)],
    )
    .expect("global pin changes should not be validated as packages");

    assert_eq!(selection.pin_changes[0].target, PinTarget::All);
}

#[test]
fn plan_selection_accepts_known_selected_item_ids() {
    let plan = plan();
    let selection = PlanSelection::new(
        &plan,
        vec![
            SelectedItem::new(plan_item_id("delayed-exact"), false),
            SelectedItem::new(plan_item_id("fresh-tool"), false),
        ],
        Vec::new(),
    )
    .expect("known item ids should be selectable at this boundary");

    assert_eq!(selection.selected_items.len(), 2);
}

#[test]
fn plan_selection_rejects_pin_changes_for_unknown_packages() {
    let plan = plan();
    let error = PlanSelection::new(
        &plan,
        Vec::new(),
        vec![PinChange::new(
            PinTarget::Package(PackageName::new("not-in-plan").expect("valid package name")),
            PinOperation::Pin,
        )],
    )
    .expect_err("pin change should target a package in the plan");

    assert_eq!(
        error,
        DomainError::UnknownPinTarget("not-in-plan".to_owned())
    );
}

#[test]
fn plan_selection_rejects_unknown_selected_items() {
    let plan = plan();
    let error = PlanSelection::new(
        &plan,
        vec![SelectedItem::new(plan_item_id("missing"), false)],
        Vec::new(),
    )
    .expect_err("unknown selected item should fail");

    assert_eq!(error.to_string(), "unknown plan item id `missing`");
}
