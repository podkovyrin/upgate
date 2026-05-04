use std::path::{Path, PathBuf};

use upnow_domain::{
    ExecutionEligibility, ManagerId, PackageName, PlanItem, PlanItemId, PlanSelection,
    ReleaseLookupResult, SelectedItem, ToolId, UpdateCandidate, UpdatePlan, VersionScheme,
    VersionText,
};
use upnow_infra::{CommandOutput, ProcessRunner};
use upnow_managers::yarn::{
    YarnError, exact_command, installed_global, parse_global_list_jsonl, parse_time_jsonl,
    parse_yarn_major_version,
};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/managers/yarn")
}

fn text(path: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(path)).expect("fixture should be readable")
}

#[test]
fn parses_yarn_major_versions() {
    assert_eq!(parse_yarn_major_version("1.22.22\n"), Some(1));
    assert_eq!(parse_yarn_major_version("v4.3.1\n"), Some(4));
    assert_eq!(parse_yarn_major_version(""), None);
}

#[test]
fn parses_global_list_jsonl() {
    let installed = parse_global_list_jsonl(&text("deterministic/global-list.jsonl"))
        .expect("global list should parse");

    assert!(installed.iter().any(|package| {
        package.name.as_str() == "alpha-ready" && package.version.as_str() == "1.0.0"
    }));
    assert!(installed.iter().any(|package| {
        package.name.as_str() == "pinned-pkg" && package.version.as_str() == "3.0.0"
    }));
}

#[test]
fn parses_global_list_with_scoped_package() {
    let raw =
        r#"{"type":"tree","data":{"trees":[{"name":"npm@11.12.0"},{"name":"@scope/tool@2.3.4"}]}}"#;

    let installed = parse_global_list_jsonl(raw).expect("global list should parse");

    assert!(installed.iter().any(|package| {
        package.name.as_str() == "@scope/tool" && package.version.as_str() == "2.3.4"
    }));
}

#[test]
fn parses_registry_time_jsonl() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline =
        parse_time_jsonl(&package, &text("deterministic/time/alpha-ready.jsonl")).expect("time");

    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version == VersionText::new("1.2.0").expect("valid version"))
    );
}

#[test]
fn empty_or_missing_inspect_data_is_missing_metadata_source() {
    let package = PackageName::new("empty").expect("valid package");
    let err = parse_time_jsonl(&package, r#"{"type":"info","data":"ignored"}"#)
        .expect_err("missing inspect data should fail");

    let lookup = match err {
        YarnError::EmptyTimeMap { .. } => ReleaseLookupResult::MissingMetadata,
        other => panic!("unexpected error: {other}"),
    };
    assert!(matches!(lookup, ReleaseLookupResult::MissingMetadata));
}

#[test]
fn yarn_two_plus_installed_discovery_returns_empty_inventory() {
    let process =
        ProcessRunner::fake([Ok(CommandOutput::from_parts(success_status(), "4.3.1", ""))]);

    let installed = installed_global(&process).expect("Yarn 2+ should not fail discovery");

    assert!(installed.is_empty());
    assert_eq!(fake_calls(&process), ["yarn --version"]);
}

#[test]
fn constructs_exact_yarn_global_add_command() {
    let command = exact_command(&candidate());

    assert_eq!(command.display(), "yarn global add alpha-ready@1.2.0");
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

#[allow(dead_code)]
fn selection(plan: &UpdatePlan) -> PlanSelection {
    PlanSelection::new(
        plan,
        vec![SelectedItem::new(
            PlanItemId::new("yarn:alpha-ready").expect("valid id"),
            false,
        )],
        Vec::new(),
    )
    .expect("valid selection")
}

#[allow(dead_code)]
fn plan() -> UpdatePlan {
    UpdatePlan::new(
        ManagerId::new("yarn").expect("valid manager"),
        vec![PlanItem::Update {
            id: PlanItemId::new("yarn:alpha-ready").expect("valid id"),
            candidate: candidate(),
        }],
    )
    .expect("valid plan")
}

fn fake_calls(process: &ProcessRunner) -> Vec<String> {
    match process {
        ProcessRunner::Fake(fake) => fake
            .calls()
            .iter()
            .map(upnow_infra::CommandSpec::display)
            .collect(),
        ProcessRunner::Real { .. } => panic!("expected fake process"),
    }
}

#[cfg(unix)]
fn success_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn success_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}
