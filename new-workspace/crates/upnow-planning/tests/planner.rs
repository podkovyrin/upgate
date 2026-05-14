use upnow_domain::{
    ExecutionSupport, ManagerId, PackageName, PlanItem, PlanItemId, SelectedUpdate, ToolId,
    UpdateCandidate, UpdatePlan, UpdateSelectionMode, UpdateSelectionPolicy, VersionScheme,
    VersionText,
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
        ExecutionSupport::native_or_exact(),
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
                id: item_id("pnpm:exception-pkg"),
                candidate: candidate("exception-pkg"),
            },
        ],
    )
    .expect("plan should be valid")
}

#[test]
fn default_batch_selection_include_mode_excludes_exceptions() {
    let plan = plan();
    let policy = UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Include,
        except: std::iter::once(PackageName::new("exception-pkg").expect("valid package name"))
            .collect(),
    };
    let selection = default_batch_selection(&plan, &policy).expect("selection should be valid");

    assert_eq!(selection.selected_items.len(), 1);
    assert_eq!(
        selection.selected_items[0].plan_item_id.as_str(),
        "pnpm:alpha-ready"
    );
    assert_eq!(
        selection.selected_items[0].selected_update,
        SelectedUpdate::Recommended
    );
}

#[test]
fn default_batch_selection_skip_mode_includes_only_exceptions() {
    let plan = plan();
    let policy = UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Skip,
        except: std::iter::once(PackageName::new("exception-pkg").expect("valid package name"))
            .collect(),
    };
    let selection = default_batch_selection(&plan, &policy).expect("selection should be valid");

    assert_eq!(selection.selected_items.len(), 1);
    assert_eq!(
        selection.selected_items[0].plan_item_id.as_str(),
        "pnpm:exception-pkg"
    );
    assert_eq!(
        selection.selected_items[0].selected_update,
        SelectedUpdate::Recommended
    );
}
