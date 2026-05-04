use std::path::{Path, PathBuf};

use upnow_domain::{
    ExecutionEligibility, ManagerId, PackageName, PlanItem, PlanItemId, PlanSelection,
    SelectedItem, ToolId, UpdateCandidate, UpdatePlan, VersionScheme, VersionText,
};
use upnow_execution::{ExecutionCapabilities, resolve_selection_for_execution};
use upnow_managers::pnpm::{
    exact_command, exact_commands_for_execution_plan, is_no_importer_manifest_error,
    parse_installed_json, parse_outdated_json,
};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/managers/pnpm")
}

fn text(path: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(path)).expect("fixture should be readable")
}

#[test]
fn parses_installed_global_list() {
    let installed =
        parse_installed_json(&text("deterministic/list.json")).expect("list should parse");

    assert!(installed.iter().any(|package| {
        package.name.as_str() == "alpha-ready" && package.version.as_str() == "1.0.0"
    }));
    assert!(installed.iter().any(|package| {
        package.name.as_str() == "pinned-pkg" && package.version.as_str() == "3.0.0"
    }));
}

#[test]
fn parses_outdated_map_and_ignores_missing_current() {
    let parsed = parse_outdated_json(
        r#"{
            "alpha-ready": {"current": "1.0.0"},
            "missing-current": {"latest": "2.0.0"}
        }"#,
    )
    .expect("outdated JSON should parse");

    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].name.as_str(), "alpha-ready");
    assert_eq!(parsed[0].current.as_str(), "1.0.0");
}

#[test]
fn detects_pnpm_no_importer_manifest_message() {
    assert!(is_no_importer_manifest_error(
        "ERR_PNPM_NO_IMPORTER_MANIFEST_FOUND: no package.json"
    ));
    assert!(is_no_importer_manifest_error(
        "ERR_PNPM_NO_IMPORTER_MANIFEST_FOUND No package.json found"
    ));
}

#[test]
fn constructs_exact_pnpm_add_command() {
    let command = exact_command(&candidate());

    assert_eq!(command.display(), "pnpm add -g alpha-ready@1.2.0");
}

#[test]
fn creates_exact_commands_from_typed_selection() {
    let plan = plan();
    let selection = selection(&plan);
    let execution_plan = resolve_selection_for_execution(
        &plan,
        &selection,
        ExecutionCapabilities {
            exact_target: true,
            native_update: false,
            native_global_update: false,
        },
        upnow_domain::VersionPolicy::Stable,
    )
    .expect("selection should resolve");

    let commands =
        exact_commands_for_execution_plan(&execution_plan).expect("selection should be executable");

    assert_eq!(commands.len(), 1);
    assert_eq!(
        commands[0].command.display(),
        "pnpm add -g alpha-ready@1.2.0"
    );
}

fn selection(plan: &UpdatePlan) -> PlanSelection {
    PlanSelection::new(
        plan,
        vec![SelectedItem::new(
            PlanItemId::new("pnpm:alpha-ready").expect("valid id"),
            false,
        )],
        Vec::new(),
    )
    .expect("valid selection")
}

fn plan() -> UpdatePlan {
    UpdatePlan::new(
        ManagerId::new("pnpm").expect("valid manager"),
        vec![PlanItem::Update {
            id: PlanItemId::new("pnpm:alpha-ready").expect("valid id"),
            candidate: candidate(),
        }],
    )
    .expect("valid plan")
}

fn candidate() -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new("alpha-ready").expect("valid tool id"),
        PackageName::new("alpha-ready").expect("valid package"),
        VersionText::new("1.0.0").expect("valid version"),
        VersionText::new("1.2.0").expect("valid version"),
        VersionScheme::SemVer,
        ExecutionEligibility::ExactOnly,
    )
}
