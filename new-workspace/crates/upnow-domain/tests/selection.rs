use upnow_domain::{
    DelayReason, ExecutionEligibility, InstalledTool, ManagerId, ManagerMetadata, PackageName,
    PlanItem, PlanItemId, PlanSelection, SelectedItem, SelectedTarget, ToolId, ToolName,
    UpdateCandidate, UpdatePlan, UpdateSelectionMode, UpdateSelectionPolicy, VersionScheme,
    VersionText,
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
fn plan_selection_accepts_selected_items_and_selection_policy() {
    let plan = plan();
    let policy = UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Include,
        except: [PackageName::new("fresh-tool").expect("valid package name")]
            .into_iter()
            .collect(),
    };
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::forced_candidate(plan_item_id("alpha-ready"))],
        policy,
    )
    .expect("selection should reference known plan items");

    assert_eq!(
        selection.selected_items[0].plan_item_id.as_str(),
        "alpha-ready"
    );
    assert_eq!(
        selection.selected_items[0].target,
        SelectedTarget::ForcedCandidate
    );
    assert!(
        selection
            .selection_policy
            .except
            .contains(&PackageName::new("fresh-tool").expect("valid package name"))
    );
}

#[test]
fn selected_item_preserves_alternate_exact_target() {
    let target_version = VersionText::new("1.1.0").expect("valid target version");
    let selected = SelectedItem::alternate_exact(plan_item_id("alpha-ready"), target_version);

    assert!(matches!(
        selected.target,
        SelectedTarget::AlternateExact { ref target_version }
            if target_version.as_str() == "1.1.0"
    ));
}

#[test]
fn forced_candidate_selection_preserves_default_policy() {
    let plan = plan();
    let selection = PlanSelection::new(
        &plan,
        vec![SelectedItem::forced_candidate(plan_item_id(
            "delayed-exact",
        ))],
        UpdateSelectionPolicy::default(),
    )
    .expect("forced candidate selection should be valid");

    assert!(selection.selection_policy.is_default());
}

#[test]
fn plan_selection_accepts_known_selected_item_ids() {
    let plan = plan();
    let selection = PlanSelection::new(
        &plan,
        vec![
            SelectedItem::recommended(plan_item_id("delayed-exact")),
            SelectedItem::recommended(plan_item_id("fresh-tool")),
        ],
        UpdateSelectionPolicy::default(),
    )
    .expect("known item ids should be selectable at this boundary");

    assert_eq!(selection.selected_items.len(), 2);
}

#[test]
fn plan_selection_accepts_stale_selection_exceptions() {
    let plan = plan();
    let policy = UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Include,
        except: [PackageName::new("not-in-plan").expect("valid package name")]
            .into_iter()
            .collect(),
    };
    let selection =
        PlanSelection::new(&plan, Vec::new(), policy).expect("stale exceptions are inert config");

    assert_eq!(
        selection.selection_policy.except,
        [PackageName::new("not-in-plan").expect("valid package name")]
            .into_iter()
            .collect()
    );
}

#[test]
fn plan_selection_rejects_unknown_selected_items() {
    let plan = plan();
    let error = PlanSelection::new(
        &plan,
        vec![SelectedItem::recommended(plan_item_id("missing"))],
        UpdateSelectionPolicy::default(),
    )
    .expect_err("unknown selected item should fail");

    assert_eq!(error.to_string(), "unknown plan item id `missing`");
}

#[test]
fn plan_selection_rejects_duplicate_selected_items() {
    let plan = plan();
    let error = PlanSelection::new(
        &plan,
        vec![
            SelectedItem::recommended(plan_item_id("alpha-ready")),
            SelectedItem::alternate_exact(
                plan_item_id("alpha-ready"),
                VersionText::new("1.1.0").expect("valid target"),
            ),
        ],
        UpdateSelectionPolicy::default(),
    )
    .expect_err("duplicate selected item should fail");

    assert_eq!(
        error.to_string(),
        "duplicate selected plan item id `alpha-ready`"
    );
}

#[test]
fn include_mode_includes_non_exceptions_and_excludes_exceptions() {
    let policy = UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Include,
        except: [PackageName::new("eslint").expect("valid package")]
            .into_iter()
            .collect(),
    };

    assert!(!policy.includes(&PackageName::new("eslint").expect("valid package")));
    assert!(policy.includes(&PackageName::new("webpack").expect("valid package")));
}

#[test]
fn skip_mode_excludes_non_exceptions_and_includes_exceptions() {
    let policy = UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Skip,
        except: [PackageName::new("typescript").expect("valid package")]
            .into_iter()
            .collect(),
    };

    assert!(policy.includes(&PackageName::new("typescript").expect("valid package")));
    assert!(!policy.includes(&PackageName::new("webpack").expect("valid package")));
}
