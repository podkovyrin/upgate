use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::AtomicBool;

mod common;

use common::http::{
    BackgroundTcpServer, read_http_request_head, run_fake_http_server, write_http_response_text,
};
use common::{
    SandboxEnv, assert_success, command_output, fixture_path, scenario_path,
    skip_hybrid_test_if_disabled, spawn_upnow, stderr, stdout, write_executable,
};

const DETERMINISTIC_SCENARIO: &str = "tests/scenarios/pipx/deterministic";
const HYBRID_SCENARIO: &str = "tests/scenarios/pipx/hybrid";

const DETERMINISTIC_CONFIG: &str = r#"
[pipx]
mode = "apply"
min_release_age = "7d"
pinned = ["pinned-pkg"]
"#;

const HYBRID_CONFIG: &str = r#"
[pipx]
mode = "apply"
min_release_age = "7d"
pinned = ["black"]
"#;

struct Sandbox {
    env: SandboxEnv,
    fake_pipx: PathBuf,
    pipx_scenario_dir: PathBuf,
    pypi_base_url: String,
    pypi_server: Option<BackgroundTcpServer>,
}

impl Sandbox {
    fn new(scenario_rel: &str, config_toml: &str, local_pypi: bool) -> Self {
        let sandbox_env = SandboxEnv::new("mock-pipx");
        let fake_bin_dir = sandbox_env.fake_bin_dir.clone();

        sandbox_env.write_config(config_toml);

        let fake_pipx = fake_bin_dir.join("pipx");
        write_executable(
            &fake_pipx,
            include_str!("fakes/pipx.sh"),
            "fake pipx script",
        );

        let pipx_scenario_dir = scenario_path(scenario_rel, "pipx");

        let (pypi_base_url, pypi_server) = if local_pypi {
            let pypi_fixtures_dir = fixture_path(&pipx_scenario_dir, "pypi", "pipx PyPI");
            let server =
                BackgroundTcpServer::start("fake PyPI server", move |listener, stop_flag| {
                    run_fake_pypi_server(
                        &listener,
                        pypi_fixtures_dir.as_path(),
                        stop_flag.as_ref(),
                    );
                });
            (server.base_url(), Some(server))
        } else {
            // Empty value falls back to the production default URL in manager code.
            (String::new(), None)
        };

        Self {
            env: sandbox_env,
            fake_pipx,
            pipx_scenario_dir,
            pypi_base_url,
            pypi_server,
        }
    }

    fn run_upnow(&self, args: &[&str]) -> Output {
        spawn_upnow(args, &[], |cmd| {
            self.apply_base_env(cmd);
        })
    }

    fn run_fake_pipx(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.fake_pipx);
        cmd.args(args);
        self.apply_base_env(&mut cmd);

        command_output(&mut cmd, "fake pipx")
    }

    fn apply_base_env(&self, cmd: &mut Command) {
        self.env.apply_base_env(cmd);
        cmd.env("UPNOW_FAKE_PIPX_SCENARIO_DIR", &self.pipx_scenario_dir);
        cmd.env("UPNOW_PIPX_PYPI_BASE_URL", &self.pypi_base_url);
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(mut server) = self.pypi_server.take() {
            server.shutdown();
        }
    }
}

#[test]
fn fake_pipx_harness_routes_commands_to_expected_fixtures() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG, true);

    let listed = sandbox.run_fake_pipx(&["list", "--json"]);
    assert_success(&listed, "fake pipx list");
    let listed_stdout = stdout(&listed);
    assert!(listed_stdout.contains("alpha-ready"));
    assert!(listed_stdout.contains("pinned-pkg"));

    let unsupported = sandbox.run_fake_pipx(&["list"]);
    assert_eq!(
        unsupported.status.code(),
        Some(64),
        "unsupported args should exit 64"
    );
}

#[test]
fn deterministic_plan_covers_update_delayed_pinned_and_error_states() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG, true);

    let output = sandbox.run_upnow(&[
        "plan",
        "--plain",
        "--verbose",
        "--managers",
        "pipx",
        "--show-commands",
    ]);
    assert_success(&output, "upnow plan deterministic pipx");

    let out = stdout(&output);
    assert!(out.contains("+ Update [pipx] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("~ Delayed [pipx] gamma-delayed v2.0.0 -> v2.1.0"));
    assert!(out.contains("- Skipped [pipx] pinned-pkg v3.0.0 -> v3.1.0 (pinned)"));
    assert!(out.contains("! Error [pipx] omega-error v0.1.0 -> v0.1.0"));

    let err = stderr(&output);
    assert!(err.contains("$ pipx list --json"));
}

#[test]
fn deterministic_apply_selective_path_runs_updates_only_for_eligible_unpinned_items() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG, true);

    let output = sandbox.run_upnow(&[
        "apply",
        "--plain",
        "--verbose",
        "--managers",
        "pipx",
        "--show-commands",
    ]);
    assert_success(&output, "upnow apply deterministic pipx");

    let out = stdout(&output);
    assert!(out.contains("+ Update [pipx] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("~ Delayed [pipx] gamma-delayed v2.0.0 -> v2.1.0"));
    assert!(out.contains("- Skipped [pipx] pinned-pkg v3.0.0 -> v3.1.0 (pinned)"));

    let err = stderr(&output);
    assert!(err.contains("$ pipx upgrade alpha-ready==1.2.0"));
    assert!(!err.contains("$ pipx upgrade pinned-pkg==3.1.0"));
    assert!(!err.contains("$ pipx upgrade gamma-delayed==2.1.0"));
    assert!(!err.contains("$ pipx upgrade omega-error==0.2.0"));
}

#[test]
fn deterministic_scan_reports_current_state_and_missing_age_fallback() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG, true);

    let output = sandbox.run_upnow(&["scan", "--plain", "--verbose", "--managers", "pipx"]);
    assert_success(&output, "upnow scan deterministic pipx");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("= Current [pipx] alpha-ready v1.0.0 (released:"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [pipx] scan-noage v5.0.0"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
}

#[test]
#[ignore = "requires network + real PyPI; run via scripts/test-hybrid.sh"]
fn hybrid_apply_uses_real_pypi_data_with_fake_installed_state() {
    if skip_hybrid_test_if_disabled() {
        return;
    }

    let sandbox = Sandbox::new(HYBRID_SCENARIO, HYBRID_CONFIG, false);
    let output = sandbox.run_upnow(&[
        "apply",
        "--plain",
        "--verbose",
        "--managers",
        "pipx",
        "--show-commands",
    ]);
    assert_success(&output, "upnow apply hybrid pipx");

    let out = stdout(&output);
    assert!(out.contains("+ Update [pipx] requests v2.0.0 -> v"));
    assert!(
        out.contains("- Skipped [pipx] black v1.0.0 -> v") && out.contains("(pinned)"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{}",
        stderr(&output)
    );
    assert!(!out.contains(" packaging v9999.0.0 -> v"));
    assert!(
        out.contains("! Error [pipx] zzzz-upnow-no-such-package-000000000000 v1.0.0 -> v1.0.0")
    );

    let err = stderr(&output);
    assert!(
        err.contains("warning: real mutating commands are ENABLED")
            || err.contains("warning: apply runs with real mutating commands ENABLED"),
        "hybrid stderr:\n{err}"
    );
    assert!(err.contains("$ pipx upgrade requests=="));
    assert!(!err.contains("$ pipx upgrade black=="));
    assert!(!err.contains("$ pipx upgrade packaging=="));
    assert!(!err.contains("$ pipx upgrade zzzz-upnow-no-such-package-000000000000=="));
}

fn run_fake_pypi_server(listener: &TcpListener, fixtures_dir: &std::path::Path, stop: &AtomicBool) {
    run_fake_http_server(listener, stop, |stream| {
        handle_fake_pypi_connection(stream, fixtures_dir);
    });
}

fn handle_fake_pypi_connection(stream: &mut TcpStream, fixtures_dir: &std::path::Path) {
    let Some(request) = read_http_request_head(stream) else {
        return;
    };
    let Some(first_line) = request.lines().next() else {
        return;
    };

    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();

    if method != "GET" {
        write_http_response_text(
            stream,
            "405 Method Not Allowed",
            "text/plain",
            "method not allowed",
        );
        return;
    }

    let Some(pkg) = target
        .strip_prefix("/pypi/")
        .and_then(|rest| rest.strip_suffix("/json"))
        .filter(|name| !name.is_empty() && !name.contains('/'))
    else {
        write_http_response_text(stream, "404 Not Found", "text/plain", "not found");
        return;
    };

    let fixture_path = fixtures_dir.join(format!("{pkg}.json"));
    match fs::read_to_string(&fixture_path) {
        Ok(body) => write_http_response_text(stream, "200 OK", "application/json", &body),
        Err(_) => write_http_response_text(stream, "404 Not Found", "text/plain", "not found"),
    }
}
