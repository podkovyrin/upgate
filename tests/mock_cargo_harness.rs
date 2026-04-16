use std::env;
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
    SandboxEnv, assert_success, fixture_path, scenario_path, spawn_upnow, stderr, stdout,
    write_executable,
};

const DETERMINISTIC_SCENARIO: &str = "tests/scenarios/cargo/deterministic";
const HYBRID_SCENARIO: &str = "tests/scenarios/cargo/hybrid";

const DETERMINISTIC_CONFIG: &str = r#"
[cargo]
mode = "apply"
min_release_age = "7d"
pinned = ["pinned-pkg"]
"#;

const HYBRID_CONFIG: &str = r#"
[cargo]
mode = "apply"
min_release_age = "7d"
pinned = ["clap"]
"#;

const CARGO_LEDGER_JSON: &str = r#"
{
  "installs": {
    "alpha-ready 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)": {
      "bins": ["alpha-ready"],
      "features": ["fast-mode", "vendored-ssl"],
      "all_features": false,
      "no_default_features": true
    },
    "beta-fresh-latest 1.1.0 (registry+https://github.com/rust-lang/crates.io-index)": {
      "bins": ["beta-fresh-latest"],
      "features": [],
      "all_features": false,
      "no_default_features": false
    }
  }
}
"#;

struct Sandbox {
    env: SandboxEnv,
    fake_cargo: PathBuf,
    cargo_scenario_dir: PathBuf,
    cargo_home: PathBuf,
    crates_io_base_url: String,
    crates_io_server: Option<BackgroundTcpServer>,
}

impl Sandbox {
    fn new(scenario_rel: &str, config_toml: &str, local_crates_io: bool) -> Self {
        let sandbox_env = SandboxEnv::new("mock-cargo");
        let fake_bin_dir = sandbox_env.fake_bin_dir.clone();
        let cargo_home = sandbox_env.root.join("cargo-home");

        fs::create_dir_all(&cargo_home).expect("create fake CARGO_HOME dir");

        sandbox_env.write_config(config_toml);
        fs::write(cargo_home.join(".crates2.json"), CARGO_LEDGER_JSON)
            .expect("write fake cargo install ledger");

        let fake_cargo = fake_bin_dir.join("cargo");
        write_executable(
            &fake_cargo,
            include_str!("fakes/cargo.sh"),
            "fake cargo script",
        );

        let cargo_scenario_dir = scenario_path(scenario_rel, "cargo");

        let (crates_io_base_url, crates_io_server) = if local_crates_io {
            let crates_fixtures_dir = fixture_path(&cargo_scenario_dir, "crates", "cargo crates");
            let server =
                BackgroundTcpServer::start("fake crates.io server", move |listener, stop_flag| {
                    run_fake_crates_io_server(
                        &listener,
                        crates_fixtures_dir.as_path(),
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
            fake_cargo,
            cargo_scenario_dir,
            cargo_home,
            crates_io_base_url,
            crates_io_server,
        }
    }

    fn run_upnow(&self, args: &[&str]) -> Output {
        spawn_upnow(args, &[], |cmd| {
            self.apply_base_env(cmd);
        })
    }

    fn run_fake_cargo(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.fake_cargo);
        cmd.args(args);
        self.apply_base_env(&mut cmd);

        cmd.output().expect("failed to run fake cargo")
    }

    fn apply_base_env(&self, cmd: &mut Command) {
        self.env.apply_base_env(cmd);
        cmd.env("CARGO_HOME", &self.cargo_home);
        cmd.env("UPNOW_FAKE_CARGO_SCENARIO_DIR", &self.cargo_scenario_dir);
        cmd.env("UPNOW_CARGO_CRATES_IO_BASE_URL", &self.crates_io_base_url);
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(mut server) = self.crates_io_server.take() {
            server.shutdown();
        }
    }
}

#[test]
fn fake_cargo_harness_routes_commands_to_expected_fixtures() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG, true);

    let listed = sandbox.run_fake_cargo(&["install", "--list"]);
    assert_success(&listed, "fake cargo install --list");
    let listed_stdout = stdout(&listed);
    assert!(listed_stdout.contains("alpha-ready"));
    assert!(listed_stdout.contains("pinned-pkg"));

    let search = sandbox.run_fake_cargo(&["search", "alpha-ready", "--limit", "1"]);
    assert_success(&search, "fake cargo search alpha-ready");
    let search_stdout = stdout(&search);
    assert!(search_stdout.contains("alpha-ready = \"1.2.0\""));

    let missing = sandbox.run_fake_cargo(&["search", "does-not-exist", "--limit", "1"]);
    assert_eq!(
        missing.status.code(),
        Some(66),
        "missing fixture should exit 66"
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
        "cargo",
        "--show-commands",
    ]);
    assert_success(&output, "upnow plan deterministic cargo");

    let out = stdout(&output);
    assert!(
        out.contains("+ Update [cargo] alpha-ready v1.0.0 -> v1.2.0"),
        "plan stdout:\n{out}\nplan stderr:\n{}",
        stderr(&output)
    );
    assert!(
        out.contains("~ Delayed [cargo] gamma-delayed v2.0.0 -> v2.1.0"),
        "plan stdout:\n{out}\nplan stderr:\n{}",
        stderr(&output)
    );
    assert!(
        out.contains("- Skipped [cargo] pinned-pkg v3.0.0 -> v3.0.0 (pinned)"),
        "plan stdout:\n{out}\nplan stderr:\n{}",
        stderr(&output)
    );
    assert!(
        out.contains("! Error [cargo] omega-error v0.1.0 -> v0.1.0"),
        "plan stdout:\n{out}\nplan stderr:\n{}",
        stderr(&output)
    );

    let err = stderr(&output);
    assert!(err.contains("$ cargo install --list"));
    assert!(err.contains("$ cargo search alpha-ready --limit 1"));
}

#[test]
fn deterministic_apply_selective_path_runs_updates_only_for_eligible_unpinned_crates() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG, true);

    let output = sandbox.run_upnow(&[
        "apply",
        "--plain",
        "--verbose",
        "--managers",
        "cargo",
        "--show-commands",
    ]);
    assert_success(&output, "upnow apply deterministic cargo");

    let out = stdout(&output);
    assert!(
        out.contains("+ Update [cargo] alpha-ready v1.0.0 -> v1.2.0"),
        "apply stdout:\n{out}\napply stderr:\n{}",
        stderr(&output)
    );
    assert!(
        out.contains("~ Delayed [cargo] gamma-delayed v2.0.0 -> v2.1.0"),
        "apply stdout:\n{out}\napply stderr:\n{}",
        stderr(&output)
    );
    assert!(
        out.contains("- Skipped [cargo] pinned-pkg v3.0.0 -> v3.0.0 (pinned)"),
        "apply stdout:\n{out}\napply stderr:\n{}",
        stderr(&output)
    );

    let err = stderr(&output);
    assert!(err.contains("$ cargo install --force --bin alpha-ready --features fast-mode,vendored-ssl --no-default-features alpha-ready@1.2.0"));
    assert!(!err.contains("$ cargo install --force pinned-pkg@3.1.0"));
    assert!(!err.contains("$ cargo install --force gamma-delayed@2.1.0"));
    assert!(!err.contains("$ cargo install --force omega-error@0.2.0"));
}

#[test]
fn deterministic_scan_reports_current_state_and_missing_age_fallback() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG, true);

    let output = sandbox.run_upnow(&["scan", "--plain", "--verbose", "--managers", "cargo"]);
    assert_success(&output, "upnow scan deterministic cargo");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("= Current [cargo] alpha-ready v1.0.0 (released:"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [cargo] omega-error v0.1.0 (source: crates.io)"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
}

#[test]
#[ignore = "requires network + real crates.io; run via scripts/test-hybrid.sh"]
fn hybrid_apply_uses_real_crates_io_data_with_fake_installed_state() {
    if env::var("UPNOW_RUN_HYBRID_TESTS").as_deref() != Ok("1") {
        eprintln!("skipping hybrid test; set UPNOW_RUN_HYBRID_TESTS=1 to enable");
        return;
    }

    let sandbox = Sandbox::new(HYBRID_SCENARIO, HYBRID_CONFIG, false);
    let output = sandbox.run_upnow(&[
        "apply",
        "--plain",
        "--verbose",
        "--managers",
        "cargo",
        "--show-commands",
    ]);
    assert_success(&output, "upnow apply hybrid cargo");

    let out = stdout(&output);
    assert!(out.contains("+ Update [cargo] serde v1.0.0 -> v"));
    assert!(out.contains("- Skipped [cargo] clap v1.0.0 -> v1.0.0 (pinned)"));
    assert!(out.contains("~ Delayed [cargo] semver v9999.0.0 -> v9999.0.0"));
    assert!(out.contains("! Error [cargo] zzzz-upnow-no-such-crate-000000000000 v1.0.0 -> v1.0.0"));

    let err = stderr(&output);
    assert!(
        err.contains("warning: real mutating commands are ENABLED")
            || err.contains("warning: apply runs with real mutating commands ENABLED"),
        "hybrid stderr:\n{err}"
    );
    assert!(err.contains("$ cargo install --force serde@"));
    assert!(!err.contains("$ cargo install --force clap@"));
    assert!(!err.contains("$ cargo install --force semver@"));
    assert!(!err.contains("$ cargo install --force zzzz-upnow-no-such-crate-000000000000@"));
}

fn run_fake_crates_io_server(
    listener: &TcpListener,
    fixtures_dir: &std::path::Path,
    stop: &AtomicBool,
) {
    run_fake_http_server(listener, stop, |stream| {
        handle_fake_crates_io_connection(stream, fixtures_dir);
    });
}

fn handle_fake_crates_io_connection(stream: &mut TcpStream, fixtures_dir: &std::path::Path) {
    let Some(request) = read_http_request_head(stream) else {
        return;
    };
    let Some(first_line) = request.lines().next() else {
        return;
    };

    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let path_only = target.split('?').next().unwrap_or(target);

    if method != "GET" {
        write_http_response_text(
            stream,
            "405 Method Not Allowed",
            "text/plain",
            "method not allowed",
        );
        return;
    }

    let Some(crate_name) = path_only
        .strip_prefix("/api/v1/crates/")
        .filter(|name| !name.is_empty() && !name.contains('/'))
    else {
        write_http_response_text(stream, "404 Not Found", "text/plain", "not found");
        return;
    };

    let fixture_path = fixtures_dir.join(format!("{crate_name}.json"));
    match fs::read_to_string(&fixture_path) {
        Ok(body) => write_http_response_text(stream, "200 OK", "application/json", &body),
        Err(_) => write_http_response_text(stream, "404 Not Found", "text/plain", "not found"),
    }
}
