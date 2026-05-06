use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use upnow_domain::{
    ManagerUpdateInput, PackageName, PlanItemId, ReleaseLookupResult, VersionPolicy, VersionText,
};
use upnow_execution::{ExecutionCommandIntent, ResolvedExecutionItem, ResolvedExecutionPlan};
use upnow_infra::{CommandOutput, Env, HttpClient, HttpResponse, ProcessRunner};
use upnow_managers::adapter::{CommandBuildSettings, ManagerAdapter, ReleaseLookupSubject};
use upnow_managers::uv::{
    UvManager, parse_install_target_for_package, parse_installed_tool_line,
    parse_outdated_tool_line, parse_pypi_json, tool_install_command,
};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/managers")
}

fn text(manager: &str, path: &str) -> String {
    std::fs::read_to_string(fixtures_root().join(manager).join(path))
        .expect("fixture should be readable")
}

#[test]
fn parses_installed_line() {
    let installed = parse_installed_tool_line(
        "alpha-ready v1.0.0 [required: ==1.0.0] [CPython 3.14.3]",
        "/tmp/uv-tools",
    )
    .expect("installed line should parse");
    assert_eq!(installed.name.as_str(), "alpha-ready");
    assert_eq!(installed.current.as_str(), "1.0.0");
    assert_eq!(
        installed.python_path,
        "/tmp/uv-tools/alpha-ready/bin/python"
    );
}

#[test]
fn parses_dry_run_target_for_normalized_package_name() {
    let package = PackageName::new("my-pkg").expect("valid package");
    let target = parse_install_target_for_package("Would install\n + My_Pkg==1.2.0\n", &package)
        .expect("target should parse");

    assert_eq!(target.as_str(), "1.2.0");
}

#[test]
fn parses_outdated_latest_line() {
    let (name, latest) =
        parse_outdated_tool_line("alpha-ready v1.0.0 [latest: 1.2.0] [required: ==1.0.0]")
            .expect("outdated line should parse");

    assert_eq!(name.as_str(), "alpha-ready");
    assert_eq!(latest.as_str(), "1.2.0");
}

#[test]
fn parses_pypi_release_payload() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline = parse_pypi_json(
        &package,
        &text("pipx", "deterministic/pypi/alpha-ready.json"),
    )
    .expect("PyPI payload should parse");

    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version == VersionText::new("1.2.0").expect("valid version"))
    );
}

#[test]
fn release_lookup_failure_is_item_scoped() {
    let package = PackageName::new("omega-error").expect("valid package");
    let http = HttpClient::fake([(
        "https://pypi.test/pypi/omega-error/json".to_owned(),
        HttpResponse {
            status: 200,
            body: text("pipx", "deterministic/pypi/omega-error.json"),
        },
    )]);
    let env = Env::fixed([(
        "UPNOW_UV_PYPI_BASE_URL".to_owned(),
        "https://pypi.test".to_owned(),
    )]);

    let lookup = UvManager
        .release_lookup(
            &ProcessRunner::fake([]),
            &http,
            &env,
            ReleaseLookupSubject::Package(&package),
        )
        .expect("lookup should complete");

    assert!(matches!(lookup, ReleaseLookupResult::LookupFailed(_)));
}

#[test]
fn scan_inputs_read_uv_tool_list() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "/tmp/uv-tools",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0 [required: ==1.0.0]\n",
            "",
        )),
    ]);

    let inputs = UvManager
        .scan_inputs(&process, &Env::fixed([]))
        .expect("scan should discover installed tools");

    assert_eq!(inputs.len(), 1);
    assert_eq!(
        fake_calls(&process),
        [
            "uv tool dir".to_owned(),
            "uv tool list --show-version-specifiers".to_owned(),
        ]
    );
}

#[test]
fn update_inputs_use_exclude_newer_dry_run_target() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "/tmp/uv-tools",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0 [required: ==1.0.0]\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0 [latest: 1.2.0] [required: ==1.0.0]\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("uv", "deterministic/pip-plan/alpha-ready.txt"),
            "",
        )),
    ]);
    let http = HttpClient::fake([(
        "https://pypi.test/pypi/alpha-ready/json".to_owned(),
        HttpResponse {
            status: 200,
            body: text("pipx", "deterministic/pypi/alpha-ready.json"),
        },
    )]);
    let env = Env::fixed([(
        "UPNOW_UV_PYPI_BASE_URL".to_owned(),
        "https://pypi.test".to_owned(),
    )]);

    let inputs = UvManager
        .update_inputs(
            &process,
            &http,
            &env,
            VersionPolicy::None,
            Duration::from_secs(7 * 86_400),
            fixed_now(),
        )
        .expect("update inputs should resolve");

    assert_eq!(inputs.len(), 1);
    assert!(matches!(
        &inputs[0],
        ManagerUpdateInput::Seed(seed)
            if seed.installed.package_name.as_str() == "alpha-ready"
                && seed.discovered_target.as_str() == "1.2.0"
    ));
    assert_eq!(
        fake_calls(&process),
        [
            "uv tool dir".to_owned(),
            "uv tool list --show-version-specifiers".to_owned(),
            "uv tool list --outdated".to_owned(),
            "uv pip install --dry-run -p /tmp/uv-tools/alpha-ready/bin/python --upgrade --exclude-newer 7d alpha-ready>=1.0.0".to_owned(),
        ]
    );
}

#[test]
fn dry_run_failure_becomes_resolver_error_input() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "/tmp/uv-tools",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "omega-error v0.1.0 [required: ==0.1.0]\n",
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
        Ok(CommandOutput::from_parts(
            exit_status(1),
            "",
            "resolver failed for omega-error",
        )),
    ]);

    let inputs = UvManager
        .update_inputs(
            &process,
            &HttpClient::fake([]),
            &Env::fixed([]),
            VersionPolicy::None,
            Duration::from_secs(7 * 86_400),
            fixed_now(),
        )
        .expect("resolver errors should be item scoped");

    assert!(matches!(
        &inputs[0],
        upnow_domain::ManagerUpdateInput::ResolverError { message, .. }
            if message.contains("resolver failed for omega-error")
    ));
}

#[test]
fn outdated_lookup_failure_keeps_planning_with_dry_run_target() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "/tmp/uv-tools",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0 [required: ==1.0.0]\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            exit_status(1),
            "",
            "outdated failed",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("uv", "deterministic/pip-plan/alpha-ready.txt"),
            "",
        )),
    ]);
    let http = HttpClient::fake([(
        "https://pypi.test/pypi/alpha-ready/json".to_owned(),
        HttpResponse {
            status: 200,
            body: text("pipx", "deterministic/pypi/alpha-ready.json"),
        },
    )]);
    let env = Env::fixed([(
        "UPNOW_UV_PYPI_BASE_URL".to_owned(),
        "https://pypi.test".to_owned(),
    )]);

    let inputs = UvManager
        .update_inputs(
            &process,
            &http,
            &env,
            VersionPolicy::None,
            Duration::from_secs(7 * 86_400),
            fixed_now(),
        )
        .expect("outdated lookup failures should not fail uv planning");

    assert!(matches!(
        &inputs[0],
        ManagerUpdateInput::Seed(seed) if seed.discovered_target.as_str() == "1.2.0"
    ));
}

#[test]
fn constructs_native_selected_uv_install_command() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let command = tool_install_command(&package, "7d");

    assert_eq!(
        command.display(),
        "uv tool install --upgrade --exclude-newer 7d alpha-ready"
    );
}

#[test]
fn adapter_builds_native_selected_command() {
    let plan = ResolvedExecutionPlan {
        intents: vec![ExecutionCommandIntent::NativeSelected(
            ResolvedExecutionItem {
                plan_item_id: PlanItemId::new("uv:alpha-ready").expect("valid id"),
                package_name: PackageName::new("alpha-ready").expect("valid package"),
                installed_version: VersionText::new("1.0.0").expect("valid version"),
                target_version: VersionText::new("1.2.0").expect("valid version"),
                execution_eligibility: upnow_domain::ExecutionEligibility::NativeOnly,
                forced: false,
            },
        )],
    };

    let commands = UvManager
        .commands_for_execution_plan(
            &ProcessRunner::fake([]),
            &Env::fixed([]),
            &plan,
            CommandBuildSettings {
                min_release_age: Duration::from_secs(7 * 86_400),
            },
        )
        .expect("native selected command should build");

    assert_eq!(
        commands[0].command.display(),
        "uv tool install --upgrade --exclude-newer 7d alpha-ready"
    );
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

fn fixed_now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_640_995_200)
}
