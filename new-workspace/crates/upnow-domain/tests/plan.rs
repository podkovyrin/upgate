use upnow_domain::{
    DomainError, ExecutionEligibility, PackageName, PlanItem, PlanItemId, PlanSelection,
    SelectedItem, ToolId, UpdateCandidate, UpdatePlan, UpdateSelectionMode, UpdateSelectionPolicy,
    VersionScheme, VersionText,
};

fn candidate(name: &str) -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new(format!("pnpm:{name}")).expect("valid tool id"),
        PackageName::new(name).expect("valid package name"),
        VersionText::new("1.0.0").expect("valid installed version"),
        VersionText::new("1.2.0").expect("valid target version"),
        VersionScheme::SemVer,
        ExecutionEligibility::NativeOrExact,
    )
}

#[test]
fn update_plan_rejects_duplicate_item_ids() {
    let duplicate_id = PlanItemId::new("same-id").expect("valid plan item id");
    let result = UpdatePlan::new(
        upnow_domain::ManagerId::new("pnpm").expect("valid manager id"),
        vec![
            PlanItem::Update {
                id: duplicate_id.clone(),
                candidate: candidate("alpha-ready"),
            },
            PlanItem::Update {
                id: duplicate_id,
                candidate: candidate("beta-ready"),
            },
        ],
    );

    assert_eq!(
        result,
        Err(DomainError::DuplicatePlanItemId("same-id".to_owned()))
    );
}

#[test]
fn plan_selection_rejects_policy_exception_outside_plan() {
    let plan = UpdatePlan::new(
        upnow_domain::ManagerId::new("pnpm").expect("valid manager id"),
        vec![PlanItem::Update {
            id: PlanItemId::new("pnpm:alpha").expect("valid plan item id"),
            candidate: candidate("alpha"),
        }],
    )
    .expect("plan should be valid");
    let policy = UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Include,
        except: std::iter::once(PackageName::new("beta").expect("valid package name")).collect(),
    };

    assert_eq!(
        PlanSelection::new(
            &plan,
            vec![SelectedItem::recommended(
                PlanItemId::new("pnpm:alpha").expect("valid plan item id")
            )],
            policy,
        ),
        Err(DomainError::UnknownSelectionPackage("beta".to_owned()))
    );
}
