use std::path::{Path, PathBuf};
use std::time::Duration;

use upnow_domain::{
    ExecutionEligibility, ExecutionTargetKind, ManagerConfig, ManagerId, ManagerMode,
    ManagerUpdateInput, PackageName, PlanItemId, TargetAgeLookupResult, TargetSelection,
    UpdateSelectionPolicy, VersionPolicy, VersionText,
};
use upnow_execution::{ExecutionCommandIntent, ResolvedExecutionItem, ResolvedExecutionPlan};
use upnow_infra::{CommandOutput, Env, HttpClient, HttpResponse, ProcessRunner};
use upnow_managers::adapter::ManagerAdapter;
use upnow_managers::brew::{
    BrewManager, BrewPackageKind, commands_for_execution_plan, parse_installed_info_json,
    parse_outdated_json,
};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/managers")
}

fn text(manager: &str, path: &str) -> String {
    std::fs::read_to_string(fixtures_root().join(manager).join(path))
        .expect("fixture should be readable")
}

fn brew_manager(
    no_update: bool,
    version_policy: VersionPolicy,
    min_release_age: Duration,
) -> BrewManager {
    BrewManager::new(ManagerConfig {
        manager_id: ManagerId::new("brew").expect("valid manager id"),
        mode: ManagerMode::Apply,
        min_release_age,
        version_policy,
        no_update,
        selection: UpdateSelectionPolicy::default(),
    })
}

#[test]
fn parses_outdated_formula_targets() {
    let packages = parse_outdated_json(&text("brew", "deterministic/outdated.json"))
        .expect("outdated JSON should parse");

    assert_eq!(packages[0].name.as_str(), "alpha-ready");
    assert_eq!(packages[0].installed.as_str(), "1.0.0");
    assert_eq!(packages[0].target.as_str(), "1.2.0");
    assert_eq!(packages[0].kind, BrewPackageKind::Formula);
}

#[test]
fn parses_installed_info_for_scan() {
    let packages = parse_installed_info_json(&text("brew", "deterministic/info-installed.json"))
        .expect("installed info should parse");

    assert_eq!(packages.len(), 2);
    assert_eq!(packages[0].name.as_str(), "alpha-ready");
    assert_eq!(packages[0].version.as_str(), "1.0.0");
}

#[test]
fn update_inputs_use_brew_selected_targets_and_tap_age_evidence() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            text("brew", "deterministic/outdated.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("brew", "deterministic/info-plan.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("brew", "deterministic/tap-info.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "1000000000",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "9999999999",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "not-a-timestamp",
            "",
        )),
    ]);

    let inputs = brew_manager(
        true,
        VersionPolicy::Stable,
        Duration::from_secs(12 * 60 * 60),
    )
    .update_inputs(&process, &HttpClient::fake([]), &Env::fixed([]))
    .expect("brew update inputs should build");

    assert_eq!(inputs.len(), 4);
    let ManagerUpdateInput::Seed(seed) = &inputs[0] else {
        panic!("alpha-ready should be a seed");
    };
    assert_eq!(seed.installed.package_name.as_str(), "alpha-ready");
    assert_eq!(seed.execution_target_kind, ExecutionTargetKind::BrewFormula);
    let TargetSelection::ManagerSelected(target) = &seed.target_selection else {
        panic!("brew target should be manager-selected");
    };
    assert_eq!(target.target_version.as_str(), "1.2.0");
    assert!(matches!(target.target_age, TargetAgeLookupResult::Known(_)));

    let ManagerUpdateInput::Seed(seed) = &inputs[3] else {
        panic!("omega-error should be a seed with failed target evidence");
    };
    let TargetSelection::ManagerSelected(target) = &seed.target_selection else {
        panic!("brew target should be manager-selected");
    };
    assert!(matches!(
        target.target_age,
        TargetAgeLookupResult::LookupFailed(_)
    ));
}

#[test]
fn homebrew_pinned_outdated_items_are_skipped() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"formulae":[{"name":"pinned-pkg","installed_versions":["3.0.0"],"current_version":"3.1.0","pinned":true}],"casks":[]}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"formulae":[],"casks":[]}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "[]", "")),
    ]);

    let inputs = brew_manager(true, VersionPolicy::None, Duration::from_secs(0))
        .update_inputs(&process, &HttpClient::fake([]), &Env::fixed([]))
        .expect("brew inputs should preserve manager-pinned skips");

    assert!(matches!(
        &inputs[0],
        ManagerUpdateInput::Skipped {
            reason: upnow_domain::SkipReason::Pinned,
            ..
        }
    ));
}

#[test]
fn no_update_false_refreshes_brew_metadata_before_discovery() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(success_status(), "", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"formulae":[],"casks":[]}"#,
            "",
        )),
    ]);

    let inputs = brew_manager(false, VersionPolicy::None, Duration::from_secs(0))
        .update_inputs(&process, &HttpClient::fake([]), &Env::fixed([]))
        .expect("empty brew update inputs should build");

    assert!(inputs.is_empty());
    assert_eq!(
        fake_calls(&process),
        [
            "brew update --quiet".to_owned(),
            "brew outdated --json=v2".to_owned(),
        ]
    );
}

#[test]
fn no_update_true_skips_brew_metadata_refresh() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(
        success_status(),
        r#"{"formulae":[],"casks":[]}"#,
        "",
    ))]);

    let inputs = brew_manager(true, VersionPolicy::None, Duration::from_secs(0))
        .update_inputs(&process, &HttpClient::fake([]), &Env::fixed([]))
        .expect("empty brew update inputs should build");

    assert!(inputs.is_empty());
    assert_eq!(fake_calls(&process), ["brew outdated --json=v2".to_owned()]);
}

#[test]
fn package_info_failure_becomes_item_scoped_target_lookup_failure() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"formulae":[{"name":"alpha-ready","installed_versions":["1.0.0"],"current_version":"1.2.0","pinned":false}],"casks":[]}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            failure_status(),
            "",
            "brew info failed",
        )),
    ]);

    let inputs = brew_manager(true, VersionPolicy::None, Duration::from_secs(0))
        .update_inputs(&process, &HttpClient::fake([]), &Env::fixed([]))
        .expect("metadata failure should not abort brew discovery");

    assert_eq!(inputs.len(), 1);
    let ManagerUpdateInput::Seed(seed) = &inputs[0] else {
        panic!("alpha-ready should stay item-scoped");
    };
    let TargetSelection::ManagerSelected(target) = &seed.target_selection else {
        panic!("brew target should be manager-selected");
    };
    assert!(matches!(
        target.target_age,
        TargetAgeLookupResult::LookupFailed(_)
    ));
    assert_eq!(
        fake_calls(&process),
        [
            "brew outdated --json=v2".to_owned(),
            "brew info --json=v2 alpha-ready".to_owned(),
        ]
    );
}

#[test]
fn local_tap_git_lookup_uses_fallback_refs() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"formulae":[{"name":"alpha-ready","installed_versions":["1.0.0"],"current_version":"1.2.0","pinned":false}],"casks":[]}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"formulae":[{"full_name":"alpha-ready","tap":"local/tap","ruby_source_path":"Formula/alpha-ready.rb","installed":[{"version":"1.0.0","installed_on_request":true,"installed_as_dependency":false}]}],"casks":[]}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"[{"name":"local/tap","path":"/tmp/local-tap","remote":null,"branch":"main"}]"#,
            "",
        )),
        Ok(CommandOutput::from_parts(failure_status(), "", "bad ref")),
        Ok(CommandOutput::from_parts(
            success_status(),
            "1000000000",
            "",
        )),
    ]);

    let inputs = brew_manager(true, VersionPolicy::None, Duration::from_secs(0))
        .update_inputs(&process, &HttpClient::fake([]), &Env::fixed([]))
        .expect("fallback ref should recover target age");

    let ManagerUpdateInput::Seed(seed) = &inputs[0] else {
        panic!("alpha-ready should be a seed");
    };
    let TargetSelection::ManagerSelected(target) = &seed.target_selection else {
        panic!("brew target should be manager-selected");
    };
    assert!(matches!(target.target_age, TargetAgeLookupResult::Known(_)));
    assert_eq!(
        fake_calls(&process),
        [
            "brew outdated --json=v2".to_owned(),
            "brew info --json=v2 alpha-ready".to_owned(),
            "brew tap-info --json --installed".to_owned(),
            "git -C /tmp/local-tap log -1 --format=%ct origin/main -- Formula/alpha-ready.rb"
                .to_owned(),
            "git -C /tmp/local-tap log -1 --format=%ct origin/HEAD -- Formula/alpha-ready.rb"
                .to_owned(),
        ]
    );
}

#[test]
fn local_tap_git_lookup_reports_failure_after_all_refs_fail() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"formulae":[{"name":"alpha-ready","installed_versions":["1.0.0"],"current_version":"1.2.0","pinned":false}],"casks":[]}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"formulae":[{"full_name":"alpha-ready","tap":"local/tap","ruby_source_path":"Formula/alpha-ready.rb","installed":[{"version":"1.0.0","installed_on_request":true,"installed_as_dependency":false}]}],"casks":[]}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"[{"name":"local/tap","path":"/tmp/local-tap","remote":null,"branch":"main"}]"#,
            "",
        )),
        Ok(CommandOutput::from_parts(failure_status(), "", "bad ref")),
        Ok(CommandOutput::from_parts(failure_status(), "", "bad ref")),
        Ok(CommandOutput::from_parts(failure_status(), "", "bad ref")),
        Ok(CommandOutput::from_parts(failure_status(), "", "bad ref")),
    ]);

    let inputs = brew_manager(true, VersionPolicy::None, Duration::from_secs(0))
        .update_inputs(&process, &HttpClient::fake([]), &Env::fixed([]))
        .expect("all-ref failure should stay item-scoped");

    let ManagerUpdateInput::Seed(seed) = &inputs[0] else {
        panic!("alpha-ready should be a seed");
    };
    let TargetSelection::ManagerSelected(target) = &seed.target_selection else {
        panic!("brew target should be manager-selected");
    };
    assert!(matches!(
        target.target_age,
        TargetAgeLookupResult::LookupFailed(_)
    ));
    assert_eq!(
        fake_calls(&process),
        [
            "brew outdated --json=v2".to_owned(),
            "brew info --json=v2 alpha-ready".to_owned(),
            "brew tap-info --json --installed".to_owned(),
            "git -C /tmp/local-tap log -1 --format=%ct origin/main -- Formula/alpha-ready.rb"
                .to_owned(),
            "git -C /tmp/local-tap log -1 --format=%ct origin/HEAD -- Formula/alpha-ready.rb"
                .to_owned(),
            "git -C /tmp/local-tap log -1 --format=%ct FETCH_HEAD -- Formula/alpha-ready.rb"
                .to_owned(),
            "git -C /tmp/local-tap log -1 --format=%ct HEAD -- Formula/alpha-ready.rb".to_owned(),
        ]
    );
}

#[test]
fn github_fallback_encodes_query_parameters() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"formulae":[{"name":"alpha-ready","installed_versions":["1.0.0"],"current_version":"1.2.0","pinned":false}],"casks":[]}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"formulae":[{"full_name":"alpha-ready","tap":"homebrew/core","ruby_source_path":"Formula/foo & bar.rb","installed":[{"version":"1.0.0","installed_on_request":true,"installed_as_dependency":false}]}],"casks":[]}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "[]", "")),
    ]);
    let http = HttpClient::fake([(
        "https://api.github.com/repos/Homebrew/homebrew-core/commits?path=Formula%2Ffoo+%26+bar.rb&sha=main&per_page=1".to_owned(),
        HttpResponse {
            status: 200,
            body: r#"[{"commit":{"author":null,"committer":{"date":"2020-01-01T00:00:00Z"}}}]"#
                .to_owned(),
        },
    )]);

    let inputs = brew_manager(true, VersionPolicy::None, Duration::from_secs(0))
        .update_inputs(&process, &http, &Env::fixed([]))
        .expect("encoded GitHub URL should be requested");

    let ManagerUpdateInput::Seed(seed) = &inputs[0] else {
        panic!("alpha-ready should be a seed");
    };
    let TargetSelection::ManagerSelected(target) = &seed.target_selection else {
        panic!("brew target should be manager-selected");
    };
    assert!(matches!(target.target_age, TargetAgeLookupResult::Known(_)));
}

#[test]
fn builds_grouped_formula_and_cask_commands() {
    let plan = ResolvedExecutionPlan {
        intents: vec![ExecutionCommandIntent::NativeGlobal(vec![
            resolved_item(
                "brew:alpha-ready",
                "alpha-ready",
                ExecutionTargetKind::BrewFormula,
            ),
            resolved_item("brew:beta-cask", "beta-cask", ExecutionTargetKind::BrewCask),
        ])],
    };

    let commands = commands_for_execution_plan(&plan).expect("brew commands should build");

    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0].command.display(),
        "brew upgrade --formula alpha-ready"
    );
    assert_eq!(
        commands[1].command.display(),
        "brew upgrade --cask beta-cask"
    );
    assert_eq!(commands[0].items.len(), 1);
    assert_eq!(
        commands[0].items[0].plan_item_id.as_str(),
        "brew:alpha-ready"
    );
    assert_eq!(commands[0].items[0].package_name.as_str(), "alpha-ready");
    assert_eq!(commands[0].items[0].installed_version.as_str(), "1.0.0");
    assert_eq!(commands[0].items[0].target_version.as_str(), "1.2.0");
    assert_eq!(commands[1].items.len(), 1);
    assert_eq!(commands[1].items[0].plan_item_id.as_str(), "brew:beta-cask");
    assert_eq!(commands[1].items[0].package_name.as_str(), "beta-cask");
    assert_eq!(commands[1].items[0].installed_version.as_str(), "1.0.0");
    assert_eq!(commands[1].items[0].target_version.as_str(), "1.2.0");
}

fn resolved_item(
    plan_item_id: &str,
    package: &str,
    execution_target_kind: ExecutionTargetKind,
) -> ResolvedExecutionItem {
    ResolvedExecutionItem {
        plan_item_id: PlanItemId::new(plan_item_id).expect("valid id"),
        package_name: PackageName::new(package).expect("valid package"),
        installed_version: VersionText::new("1.0.0").expect("valid version"),
        target_version: VersionText::new("1.2.0").expect("valid version"),
        execution_eligibility: ExecutionEligibility::NativeOnly,
        execution_target_kind,
        exact_target_required: false,
        bypass_min_release_age: false,
    }
}

#[cfg(unix)]
fn success_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}

#[cfg(unix)]
fn failure_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(1 << 8)
}

#[cfg(windows)]
fn success_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn failure_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(1)
}

fn fake_calls(process: &ProcessRunner) -> Vec<String> {
    match process {
        ProcessRunner::Fake(fake) => fake
            .calls()
            .into_iter()
            .map(|call| call.display())
            .collect(),
        ProcessRunner::Real { .. } => Vec::new(),
    }
}
