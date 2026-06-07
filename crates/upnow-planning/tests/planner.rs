use upnow_domain::{
    ExecutionSupport, InstalledTool, ManagerId, ManagerMetadata, ManagerUpdateInput, PackageName,
    PlanItem, PlanItemId, ReleaseEntry, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp,
    SelectedUpdate, ToolId, ToolName, UpdateCandidate, UpdatePlan, UpdateSeed, UpdateSelectionMode,
    UpdateSelectionPolicy, VersionPolicy, VersionScheme, VersionText,
};
use upnow_planning::{PlanningSettings, default_batch_selection, update_plan_from_inputs};

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
fn plan_ids_use_tool_identity_when_packages_share_a_name() {
    let package_name = PackageName::new("shared-name").expect("valid package name");
    let plan = update_plan_from_inputs(
        manager_id(),
        vec![
            ManagerUpdateInput::Seed(seed("formula:shared-name", &package_name)),
            ManagerUpdateInput::Seed(seed("cask:shared-name", &package_name)),
        ],
        PlanningSettings {
            policy: VersionPolicy::None,
            now: std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(20),
            min_release_age: std::time::Duration::ZERO,
        },
    )
    .expect("distinct tools with one package name should produce a valid plan");

    let ids: Vec<&str> = plan.items.iter().map(|item| item.id().as_str()).collect();
    assert_eq!(
        ids,
        vec!["pnpm:formula:shared-name", "pnpm:cask:shared-name"]
    );
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
fn default_batch_selection_include_mode_ignores_stale_exceptions() {
    let plan = plan();
    let policy = UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Include,
        except: std::iter::once(PackageName::new("stale-pkg").expect("valid package name"))
            .collect(),
    };
    let selection = default_batch_selection(&plan, &policy).expect("selection should be valid");

    let selected_ids = selection
        .selected_items
        .iter()
        .map(|item| item.plan_item_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(selected_ids, vec!["pnpm:alpha-ready", "pnpm:exception-pkg"]);
    assert_eq!(selection.selection_policy, policy);
}

fn seed(tool_id: &str, package_name: &PackageName) -> UpdateSeed {
    UpdateSeed::new(
        InstalledTool::new(
            manager_id(),
            ToolId::new(tool_id).expect("valid tool id"),
            package_name.clone(),
            ToolName::new(package_name.as_str()).expect("valid tool name"),
            VersionText::new("1.0.0").expect("valid installed version"),
            ManagerMetadata::empty(),
        ),
        VersionText::new("1.1.0").expect("valid target version"),
        VersionScheme::SemVer,
        ReleaseLookupResult::Known(ReleaseTimeline::new(vec![ReleaseEntry::new(
            VersionText::new("1.1.0").expect("valid release version"),
            ReleaseTimestamp::new(std::time::SystemTime::UNIX_EPOCH),
        )])),
        ExecutionSupport::native_or_exact(),
    )
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

#[test]
fn default_batch_selection_skip_mode_ignores_stale_exceptions() {
    let plan = plan();
    let policy = UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Skip,
        except: std::iter::once(PackageName::new("stale-pkg").expect("valid package name"))
            .collect(),
    };
    let selection = default_batch_selection(&plan, &policy).expect("selection should be valid");

    assert!(selection.selected_items.is_empty());
    assert_eq!(selection.selection_policy, policy);
}
