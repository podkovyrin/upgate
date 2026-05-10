use std::path::PathBuf;
use std::time::Duration;

use upnow_domain::{
    ManagerConfig, ManagerId, ManagerMode, PackageName, PlanItemId, ReleaseLookupResult,
    UpdateSelectionPolicy, VersionPolicy, VersionText,
};
use upnow_execution::{ExecutionCommandIntent, ResolvedExecutionItem, ResolvedExecutionPlan};
use upnow_infra::{CommandOutput, Env, ProcessRunner};
use upnow_managers::adapter::{ManagerAdapter, ReleaseLookupSubject};
use upnow_managers::go::{
    GoDiscoveredTool, GoManager, exact_command, lookup_release_by_module,
    parse_go_version_m_output, parse_module_time_json, parse_module_versions_json,
};

fn fixtures_dir() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/managers/go")
}

fn text(path: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(path)).expect("fixture should be readable")
}

fn go_manager() -> GoManager {
    GoManager::new(ManagerConfig {
        manager_id: ManagerId::new("go").expect("valid manager id"),
        mode: ManagerMode::Apply,
        min_release_age: Duration::from_secs(7 * 86_400),
        version_policy: VersionPolicy::None,
        no_update: false,
        selection: UpdateSelectionPolicy::default(),
    })
}

#[test]
fn parses_go_version_metadata() {
    let parsed = parse_go_version_m_output(&text("deterministic/version-m-alpha-ready.txt"))
        .expect("version metadata should parse");

    assert_eq!(parsed.install_path, "example.com/alpha/cmd/alpha-ready");
    assert_eq!(parsed.module_path, "example.com/alpha");
    assert_eq!(parsed.version, "v1.0.0");
}

#[test]
fn parse_go_version_metadata_rejects_devel_versions() {
    let raw = "path\texample.com/tool\nmod\texample.com/tool\t(devel)\th1:hash\n";

    assert!(parse_go_version_m_output(raw).is_none());
}

#[test]
fn parses_go_module_versions_and_time() {
    let versions = parse_module_versions_json(&text("deterministic/list-versions-alpha.json"))
        .expect("versions should parse");
    assert_eq!(versions, ["v1.0.0", "v1.2.0"]);

    let published = parse_module_time_json(
        "v1.2.0",
        &text("deterministic/list-module-alpha-v1.2.0.json"),
    )
    .expect("time should parse")
    .expect("time should be present");

    assert_eq!(
        published,
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_609_459_200)
    );
}

#[test]
fn release_lookup_builds_go_timeline() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"Versions":["v1.0.0","v1.2.0"]}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"Time":"2020-01-01T00:00:00Z"}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"Time":"2021-01-01T00:00:00Z"}"#,
            "",
        )),
    ]);

    let lookup =
        lookup_release_by_module(&process, "example.com/alpha").expect("lookup should not fail");

    let ReleaseLookupResult::Known(timeline) = lookup else {
        panic!("expected known timeline");
    };
    assert_eq!(timeline.versions.len(), 2);
    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version == VersionText::new("v1.2.0").expect("valid version"))
    );
}

#[test]
fn discovery_reports_missing_build_metadata_as_skipped_input() {
    let temp = temp_go_bin("go-discovery-skip");
    touch(temp.join("alpha-ready"));
    touch(temp.join("skip-nometa"));
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/version-m-alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            exit_status(1),
            "",
            "binary has no module metadata",
        )),
    ]);
    let env = Env::fixed([("GOBIN".to_owned(), temp.to_string_lossy().into_owned())]);

    let discovered = upnow_managers::go::discover_global_tools(&process, &env)
        .expect("discovery should complete");

    assert!(matches!(discovered[0], GoDiscoveredTool::Managed(_)));
    assert!(matches!(
        &discovered[1],
        GoDiscoveredTool::Skipped { name, reason }
            if name.as_str() == "skip-nometa" && reason == "missing go build metadata"
    ));
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn exact_command_uses_go_install_path() {
    let command = exact_command(
        "example.com/alpha/cmd/alpha-ready",
        &VersionText::new("v1.2.0").expect("valid version"),
    );

    assert_eq!(
        command.display(),
        "go install example.com/alpha/cmd/alpha-ready@v1.2.0"
    );
}

#[test]
fn adapter_rediscovers_go_metadata_to_build_execution_command() {
    let temp = temp_go_bin("go-command");
    touch(temp.join("alpha-ready"));
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(
        success_status(),
        text("deterministic/version-m-alpha-ready.txt"),
        "",
    ))]);
    let env = Env::fixed([("GOBIN".to_owned(), temp.to_string_lossy().into_owned())]);
    let plan = ResolvedExecutionPlan {
        intents: vec![ExecutionCommandIntent::Exact(ResolvedExecutionItem {
            plan_item_id: PlanItemId::new("go:alpha-ready").expect("valid id"),
            package_name: PackageName::new("alpha-ready").expect("valid package"),
            installed_version: VersionText::new("v1.0.0").expect("valid version"),
            target_version: VersionText::new("v1.2.0").expect("valid version"),
            execution_eligibility: upnow_domain::ExecutionEligibility::ExactOnly,
            execution_target_kind: upnow_domain::ExecutionTargetKind::Standard,
            exact_target_required: true,
            bypass_min_release_age: false,
        })],
    };

    let commands = go_manager()
        .commands_for_execution_plan(&process, &env, &plan)
        .expect("command should build");

    assert_eq!(
        commands[0].command.display(),
        "go install example.com/alpha/cmd/alpha-ready@v1.2.0"
    );
    assert_eq!(
        fake_calls(&process),
        [format!(
            "go version -m {}",
            temp.join("alpha-ready").display()
        )]
    );
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn verbose_installed_lookup_uses_current_module_version_only() {
    let temp = temp_go_bin("go-verbose-lookup");
    touch(temp.join("alpha-ready"));
    let tool = installed_tool();
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/version-m-alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"Time":"2020-01-01T00:00:00Z"}"#,
            "",
        )),
    ]);
    let env = Env::fixed([("GOBIN".to_owned(), temp.to_string_lossy().into_owned())]);
    let lookup = go_manager()
        .release_lookup(
            &process,
            &upnow_infra::HttpClient::fake([]),
            &env,
            ReleaseLookupSubject::Installed(&tool),
        )
        .expect("lookup should complete");

    let ReleaseLookupResult::Known(timeline) = lookup else {
        panic!("expected known timeline");
    };
    assert_eq!(timeline.versions.len(), 1);
    assert_eq!(
        fake_calls(&process),
        [
            format!("go version -m {}", temp.join("alpha-ready").display()),
            "go list -m -json example.com/alpha@v1.0.0".to_owned(),
        ]
    );
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn verbose_installed_lookup_treats_missing_time_as_no_age() {
    let temp = temp_go_bin("go-verbose-lookup-missing");
    touch(temp.join("alpha-ready"));
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/version-m-alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "{}", "")),
    ]);
    let env = Env::fixed([("GOBIN".to_owned(), temp.to_string_lossy().into_owned())]);

    let lookup = go_manager()
        .release_lookup(
            &process,
            &upnow_infra::HttpClient::fake([]),
            &env,
            ReleaseLookupSubject::Installed(&installed_tool()),
        )
        .expect("missing timestamp should complete");

    assert_eq!(lookup, ReleaseLookupResult::MissingMetadata);
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn verbose_installed_lookup_reports_failed_time_lookup() {
    let temp = temp_go_bin("go-verbose-lookup-failed");
    touch(temp.join("alpha-ready"));
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/version-m-alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            exit_status(1),
            "",
            "module unavailable",
        )),
    ]);
    let env = Env::fixed([("GOBIN".to_owned(), temp.to_string_lossy().into_owned())]);

    let lookup = go_manager()
        .release_lookup(
            &process,
            &upnow_infra::HttpClient::fake([]),
            &env,
            ReleaseLookupSubject::Installed(&installed_tool()),
        )
        .expect("ordinary lookup failure should be item-scoped");

    assert!(matches!(lookup, ReleaseLookupResult::LookupFailed(_)));
    let _ = std::fs::remove_dir_all(temp);
}

fn installed_tool() -> upnow_domain::InstalledTool {
    upnow_domain::InstalledTool::new(
        upnow_domain::ManagerId::new("go").expect("valid manager"),
        upnow_domain::ToolId::new("alpha-ready").expect("valid tool"),
        PackageName::new("alpha-ready").expect("valid package"),
        upnow_domain::ToolName::new("alpha-ready").expect("valid tool name"),
        VersionText::new("v1.0.0").expect("valid version"),
        upnow_domain::ManagerMetadata::empty(),
    )
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

fn temp_go_bin(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("temp dir should be created");
    path
}

fn touch(path: PathBuf) {
    std::fs::write(path, "").expect("fake binary should be writable");
}

#[cfg(unix)]
fn success_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}

#[cfg(unix)]
fn exit_status(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(code << 8)
}

#[cfg(windows)]
fn success_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn exit_status(code: u32) -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(code)
}
