use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use upnow_domain::{
    ManagerConfig, ManagerId, ManagerMode, ManagerUpdateInput, PackageName, PlanItemId,
    TargetAgeLookupResult, TargetSelection, VersionPolicy, VersionText,
};
use upnow_execution::{ExecutionCommandIntent, ResolvedExecutionItem, ResolvedExecutionPlan};
use upnow_infra::{CommandOutput, Env, HttpClient, HttpResponse, ProcessRunner};
use upnow_managers::adapter::ManagerAdapter;
use upnow_managers::mise::{
    MiseManager, global_upgrade_command, parse_installed_json, parse_ls_remote_json,
    parse_outdated_json, parse_upgrade_dry_run, parse_versions_host_toml, selected_upgrade_command,
};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/managers")
}

fn text(manager: &str, path: &str) -> String {
    std::fs::read_to_string(fixtures_root().join(manager).join(path))
        .expect("fixture should be readable")
}

fn mise_manager(version_policy: VersionPolicy) -> MiseManager {
    MiseManager::new(ManagerConfig {
        manager_id: ManagerId::new("mise").expect("valid manager id"),
        mode: ManagerMode::Apply,
        min_release_age: Duration::from_secs(7 * 86_400),
        version_policy,
        no_update: false,
        pinned: BTreeSet::new(),
    })
}

#[test]
fn parses_installed_json() {
    let installed =
        parse_installed_json(&text("mise", "deterministic/ls.json")).expect("mise ls should parse");

    assert!(
        installed
            .iter()
            .any(|tool| tool.tool.as_str() == "npm:alpha-ready"
                && tool.version.as_str() == "1.0.0")
    );
}

#[test]
fn parses_upgrade_dry_run_pairs_strictly() {
    let parsed = parse_upgrade_dry_run(
        "Would uninstall npm:alpha-ready@1.0.0\nWould install npm:alpha-ready@1.2.0\n",
    )
    .expect("dry-run pairs should parse");

    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].tool.as_str(), "npm:alpha-ready");
    assert_eq!(parsed[0].from_version.as_str(), "1.0.0");
    assert_eq!(parsed[0].to_version.as_str(), "1.2.0");
}

#[test]
fn dry_run_parser_rejects_install_without_uninstall() {
    let err = parse_upgrade_dry_run("Would install node@20.1.0\n")
        .expect_err("install without uninstall should fail");

    assert!(
        err.to_string()
            .contains("was not preceded by matching uninstall")
    );
}

#[test]
fn parses_advisory_outdated_json() {
    let latest = parse_outdated_json(&text("mise", "deterministic/outdated.json"))
        .expect("outdated JSON should parse");

    assert_eq!(
        latest
            .get(&PackageName::new("npm:alpha-ready").expect("valid package"))
            .expect("latest should exist")
            .as_str(),
        "1.2.0"
    );
}

#[test]
fn parses_ls_remote_and_versions_host_release_metadata() {
    let ls_remote =
        parse_ls_remote_json("node", &text("mise", "deterministic/ls-remote/node.json"))
            .expect("ls-remote JSON should parse");
    assert!(
        ls_remote
            .versions
            .iter()
            .any(|entry| entry.version.as_str() == "20.1.0")
    );

    let versions_host =
        parse_versions_host_toml(&text("mise", "deterministic/versions/tools/emsdk.toml"))
            .expect("versions-host TOML should parse");
    assert!(
        versions_host
            .versions
            .iter()
            .any(|entry| entry.version.as_str() == "5.0.6")
    );
}

#[test]
fn release_metadata_parsers_skip_bad_timestamps_per_entry() {
    let ls_remote = parse_ls_remote_json(
        "node",
        r#"
[
  {"version":"20.1.0","created_at":"not-a-timestamp"},
  {"version":"20.2.0","created_at":"2021-01-01T00:00:00Z"}
]
"#,
    )
    .expect("bad release entries should not fail the whole ls-remote timeline");
    assert_eq!(ls_remote.versions.len(), 1);
    assert_eq!(ls_remote.versions[0].version.as_str(), "20.2.0");

    let versions_host = parse_versions_host_toml(
        r#"
[versions]
"5.0.4" = { created_at = "not-a-timestamp" }
"5.0.6" = { created_at = "2021-02-01T00:00:00Z" }
"#,
    )
    .expect("bad release entries should not fail the whole versions-host timeline");
    assert_eq!(versions_host.versions.len(), 1);
    assert_eq!(versions_host.versions[0].version.as_str(), "5.0.6");
}

#[test]
fn update_inputs_use_dry_run_target_as_manager_selected_target() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "Would uninstall npm:alpha-ready@1.0.0\nWould install npm:alpha-ready@1.2.0\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"npm:alpha-ready":{"latest":"1.2.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("mise", "deterministic/npm/time/alpha-ready@1.2.0.json"),
            "",
        )),
    ]);

    let inputs = mise_manager(VersionPolicy::None)
        .update_inputs(&process, &HttpClient::fake([]), &Env::fixed([]))
        .expect("update inputs should resolve");

    assert!(matches!(
        &inputs[0],
        ManagerUpdateInput::Seed(seed)
            if seed.installed.package_name.as_str() == "npm:alpha-ready"
                && matches!(
                    &seed.target_selection,
                    TargetSelection::ManagerSelected(target)
                        if target.target_version.as_str() == "1.2.0"
                            && matches!(target.target_age, TargetAgeLookupResult::Known(_))
                )
    ));
    assert_eq!(
        fake_calls(&process),
        [
            "mise upgrade --dry-run --before 7d".to_owned(),
            "mise outdated --json".to_owned(),
            "npm view alpha-ready@1.2.0 time --json".to_owned(),
        ]
    );
}

#[test]
fn missing_selected_target_metadata_stays_item_scoped() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "Would uninstall npm:missing-age@1.0.0\nWould install npm:missing-age@1.2.0\n",
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "{}", "")),
        Ok(CommandOutput::from_parts(success_status(), "{}", "")),
    ]);

    let inputs = mise_manager(VersionPolicy::None)
        .update_inputs(&process, &HttpClient::fake([]), &Env::fixed([]))
        .expect("metadata failures should stay on the item");

    assert!(matches!(
        &inputs[0],
        ManagerUpdateInput::Seed(seed)
            if matches!(
                &seed.target_selection,
                TargetSelection::ManagerSelected(target)
                    if matches!(target.target_age, TargetAgeLookupResult::MissingMetadata)
            )
    ));
}

#[test]
fn target_lookup_continues_when_backend_lacks_selected_target_metadata() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "Would uninstall fallbacktool@1.0.0\nWould install fallbacktool@1.1.0\n",
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "{}", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"backends":["github:example/fallbacktool"]}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"[{"version":"1.0.0","created_at":"2021-01-01T00:00:00Z"}]"#,
            "",
        )),
    ]);
    let http = HttpClient::fake([(
        "https://mise-versions.jdx.dev/tools/fallbacktool.toml".to_owned(),
        HttpResponse {
            status: 200,
            body: r#"
[versions]
"1.1.0" = { created_at = "2021-02-01T00:00:00Z" }
"#
            .to_owned(),
        },
    )]);

    let inputs = mise_manager(VersionPolicy::None)
        .update_inputs(&process, &http, &Env::fixed([]))
        .expect("target lookup should continue to fallback metadata");

    assert!(matches!(
        &inputs[0],
        ManagerUpdateInput::Seed(seed)
            if seed.installed.package_name.as_str() == "fallbacktool"
                && matches!(
                    &seed.target_selection,
                    TargetSelection::ManagerSelected(target)
                        if target.target_version.as_str() == "1.1.0"
                            && matches!(target.target_age, TargetAgeLookupResult::Known(_))
                )
    ));
    assert_eq!(
        fake_calls(&process),
        [
            "mise upgrade --dry-run --before 7d".to_owned(),
            "mise outdated --json".to_owned(),
            "mise registry fallbacktool --json".to_owned(),
            "mise ls-remote --json github:example/fallbacktool".to_owned(),
        ]
    );
}

#[test]
fn rejects_unsupported_policy_before_discovery() {
    let err = mise_manager(VersionPolicy::Stable)
        .update_inputs(
            &ProcessRunner::fake([]),
            &HttpClient::fake([]),
            &Env::fixed([]),
        )
        .expect_err("mise supports no-policy only");

    assert!(err.to_string().contains("does not support version policy"));
}

#[test]
fn builds_selected_and_global_resolver_commands() {
    let package = PackageName::new("node").expect("valid package");
    assert_eq!(
        selected_upgrade_command("7d", &package).display(),
        "mise upgrade --before 7d node"
    );
    assert_eq!(
        global_upgrade_command("7d").display(),
        "mise upgrade --before 7d"
    );

    let plan = ResolvedExecutionPlan {
        intents: vec![ExecutionCommandIntent::ResolverNativeGlobal(vec![
            resolved_item("mise:node", "node"),
            resolved_item("mise:swiftformat", "swiftformat"),
        ])],
    };
    let commands = mise_manager(VersionPolicy::None)
        .commands_for_execution_plan(&ProcessRunner::fake([]), &Env::fixed([]), &plan)
        .expect("global command should build");

    assert_eq!(commands[0].items.len(), 2);
    assert_eq!(commands[0].command.display(), "mise upgrade --before 7d");
}

fn resolved_item(plan_item_id: &str, package: &str) -> ResolvedExecutionItem {
    ResolvedExecutionItem {
        plan_item_id: PlanItemId::new(plan_item_id).expect("valid id"),
        package_name: PackageName::new(package).expect("valid package"),
        installed_version: VersionText::new("1.0.0").expect("valid version"),
        target_version: VersionText::new("1.2.0").expect("valid version"),
        execution_eligibility: upnow_domain::ExecutionEligibility::ResolverNativeOnly,
        execution_target_kind: upnow_domain::ExecutionTargetKind::Standard,
        exact_target_required: false,
        bypass_min_release_age: false,
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
