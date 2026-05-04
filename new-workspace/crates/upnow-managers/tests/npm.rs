use std::path::{Path, PathBuf};
use std::time::Duration;

use upnow_domain::{
    DelayReason, ExecutionEligibility, ManagerId, PackageName, PlanItem, PlanItemId, PlanSelection,
    ReleaseLookupResult, SelectedItem, ToolId, UpdateCandidate, UpdatePlan, VersionPolicy,
    VersionScheme, VersionText,
};
use upnow_infra::{CommandOutput, ProcessRunner};
use upnow_managers::adapter::{CommandBuildSettings, ManagerAdapter};
use upnow_managers::npm::{
    NpmError, NpmManager, exact_command, global_update_command, outdated_global,
    parse_installed_json, parse_outdated_json, parse_time_json, selected_native_update_command,
};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/managers/npm")
}

fn text(path: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(path)).expect("fixture should be readable")
}

#[test]
fn parses_installed_global_list() {
    let installed =
        parse_installed_json(&text("deterministic/installed.json")).expect("list should parse");

    assert!(installed.iter().any(|package| {
        package.name.as_str() == "fresh-tool" && package.version.as_str() == "2.0.0"
    }));
    assert!(installed.iter().any(|package| {
        package.name.as_str() == "stale-tool" && package.version.as_str() == "1.0.0"
    }));
}

#[test]
fn parses_outdated_map() {
    let parsed =
        parse_outdated_json(&text("deterministic/outdated.json")).expect("outdated should parse");

    assert!(parsed.iter().any(|package| {
        package.name.as_str() == "alpha-ready" && package.current.as_str() == "1.0.0"
    }));
}

#[test]
fn outdated_global_allows_exit_one_and_records_command() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(
        exit_status(1),
        text("deterministic/outdated.json"),
        "",
    ))]);

    let outdated = outdated_global(&process).expect("outdated output should parse");

    assert!(outdated.iter().any(|package| {
        package.name.as_str() == "alpha-ready" && package.current.as_str() == "1.0.0"
    }));
    let calls = match &process {
        ProcessRunner::Fake(fake) => fake.calls(),
        ProcessRunner::Real { .. } => panic!("expected fake process"),
    };
    assert_eq!(calls[0].display(), "npm outdated -g --json");
}

#[test]
fn outdated_global_treats_empty_stdout_as_no_outdated_packages() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(exit_status(1), "", ""))]);

    let outdated = outdated_global(&process).expect("empty output should be accepted");

    assert!(outdated.is_empty());
}

#[test]
fn parses_registry_time_map() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline =
        parse_time_json(&package, &text("deterministic/time/alpha-ready.json")).expect("time map");

    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version == VersionText::new("1.2.0").expect("valid version"))
    );
}

#[test]
fn parses_registry_time_map_skipping_created_and_modified_metadata() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline = parse_time_json(
        &package,
        r#"{
            "created": "2020-01-01T00:00:00.000Z",
            "modified": "2022-01-01T00:00:00.000Z",
            "1.0.0": "2021-01-01T00:00:00.000Z"
        }"#,
    )
    .expect("time map should parse");

    assert_eq!(timeline.versions.len(), 1);
    assert_eq!(
        timeline.versions[0].version,
        VersionText::new("1.0.0").expect("valid version")
    );
}

#[test]
fn empty_time_map_is_missing_metadata_source() {
    let package = PackageName::new("empty").expect("valid package");
    let err = parse_time_json(&package, "{}").expect_err("empty time map should fail");

    let lookup = match err {
        NpmError::EmptyTimeMap { .. } => ReleaseLookupResult::MissingMetadata,
        other => panic!("unexpected error: {other}"),
    };
    assert!(matches!(lookup, ReleaseLookupResult::MissingMetadata));
}

#[test]
fn constructs_exact_npm_install_command_with_whole_day_min_age() {
    let command = exact_command(&candidate(), 7, false);

    assert_eq!(
        command.display(),
        "npm install -g alpha-ready@1.2.0 --min-release-age 7"
    );
}

#[test]
fn exact_npm_install_omits_min_age_when_bypassed() {
    let command = exact_command(&candidate(), 7, true);

    assert_eq!(command.display(), "npm install -g alpha-ready@1.2.0");
}

#[test]
fn constructs_selected_native_update_command() {
    let command = selected_native_update_command(&candidate(), 7);

    assert_eq!(
        command.display(),
        "npm -g update alpha-ready --min-release-age 7"
    );
}

#[test]
fn constructs_global_native_update_command() {
    let command = global_update_command(7);

    assert_eq!(command.display(), "npm -g update --min-release-age 7");
}

#[test]
fn adapter_uses_native_selected_update_for_no_policy_unforced_selection() {
    let manager = NpmManager;
    let plan = plan(PlanItem::Update {
        id: plan_item_id(),
        candidate: candidate(),
    });
    let selection = selection(false);

    let commands = manager
        .commands_for_selection(
            &plan,
            &selection,
            CommandBuildSettings {
                version_policy: VersionPolicy::None,
                min_release_age: Duration::from_secs(7 * 86_400 + 3_600),
            },
        )
        .expect("selection should build commands");

    assert_eq!(
        commands[0].command.display(),
        "npm -g update alpha-ready --min-release-age 7"
    );
}

#[test]
fn adapter_uses_exact_install_for_exact_only_no_policy_selection() {
    let manager = NpmManager;
    let plan = plan(PlanItem::Update {
        id: plan_item_id(),
        candidate: exact_only_candidate(),
    });
    let selection = selection(false);

    let commands = manager
        .commands_for_selection(
            &plan,
            &selection,
            CommandBuildSettings {
                version_policy: VersionPolicy::None,
                min_release_age: Duration::from_secs(7 * 86_400),
            },
        )
        .expect("selection should build commands");

    assert_eq!(
        commands[0].command.display(),
        "npm install -g alpha-ready@1.2.0 --min-release-age 7"
    );
}

#[test]
fn adapter_uses_native_selected_update_for_native_only_no_policy_selection() {
    let manager = NpmManager;
    let plan = plan(PlanItem::Update {
        id: plan_item_id(),
        candidate: native_only_candidate(),
    });
    let selection = selection(false);

    let commands = manager
        .commands_for_selection(
            &plan,
            &selection,
            CommandBuildSettings {
                version_policy: VersionPolicy::None,
                min_release_age: Duration::from_secs(7 * 86_400),
            },
        )
        .expect("selection should build commands");

    assert_eq!(
        commands[0].command.display(),
        "npm -g update alpha-ready --min-release-age 7"
    );
}

#[test]
fn adapter_uses_exact_install_for_policy_selection() {
    let manager = NpmManager;
    let plan = plan(PlanItem::Update {
        id: plan_item_id(),
        candidate: candidate(),
    });
    let selection = selection(false);

    let commands = manager
        .commands_for_selection(
            &plan,
            &selection,
            CommandBuildSettings {
                version_policy: VersionPolicy::Stable,
                min_release_age: Duration::from_secs(7 * 86_400),
            },
        )
        .expect("selection should build commands");

    assert_eq!(
        commands[0].command.display(),
        "npm install -g alpha-ready@1.2.0 --min-release-age 7"
    );
}

#[test]
fn adapter_forced_delayed_selection_uses_exact_install_and_bypasses_min_age() {
    let manager = NpmManager;
    let plan = plan(PlanItem::Delayed {
        id: plan_item_id(),
        candidate: candidate(),
        reason: DelayReason::ReleaseTooFresh,
    });
    let selection = selection(true);

    let commands = manager
        .commands_for_selection(
            &plan,
            &selection,
            CommandBuildSettings {
                version_policy: VersionPolicy::None,
                min_release_age: Duration::from_secs(7 * 86_400),
            },
        )
        .expect("forced selection should build commands");

    assert_eq!(
        commands[0].command.display(),
        "npm install -g alpha-ready@1.2.0"
    );
}

fn selection(forced: bool) -> PlanSelection {
    let plan = plan(PlanItem::Update {
        id: plan_item_id(),
        candidate: candidate(),
    });
    PlanSelection::new(
        &plan,
        vec![SelectedItem::new(plan_item_id(), forced)],
        Vec::new(),
    )
    .expect("valid selection")
}

fn plan(item: PlanItem) -> UpdatePlan {
    UpdatePlan::new(ManagerId::new("npm").expect("valid manager"), vec![item]).expect("valid plan")
}

fn plan_item_id() -> PlanItemId {
    PlanItemId::new("npm:alpha-ready").expect("valid id")
}

fn candidate() -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new("alpha-ready").expect("valid tool id"),
        PackageName::new("alpha-ready").expect("valid package"),
        VersionText::new("1.0.0").expect("valid version"),
        VersionText::new("1.2.0").expect("valid version"),
        VersionScheme::SemVer,
        ExecutionEligibility::NativeOrExact,
    )
}

fn exact_only_candidate() -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new("alpha-ready").expect("valid tool id"),
        PackageName::new("alpha-ready").expect("valid package"),
        VersionText::new("1.0.0").expect("valid version"),
        VersionText::new("1.2.0").expect("valid version"),
        VersionScheme::SemVer,
        ExecutionEligibility::ExactOnly,
    )
}

fn native_only_candidate() -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new("alpha-ready").expect("valid tool id"),
        PackageName::new("alpha-ready").expect("valid package"),
        VersionText::new("1.0.0").expect("valid version"),
        VersionText::new("1.2.0").expect("valid version"),
        VersionScheme::SemVer,
        ExecutionEligibility::NativeOnly,
    )
}

#[cfg(unix)]
fn exit_status(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(code << 8)
}

#[cfg(windows)]
fn exit_status(code: u32) -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(code)
}
