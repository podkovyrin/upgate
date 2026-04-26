use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::AtomicBool;
use std::{fs, net::TcpListener};

mod common;

use common::http::{
    BackgroundTcpServer, read_http_request_head, run_fake_http_server, write_http_response_text,
};
use common::{
    SandboxEnv, assert_success, command_output, path_to_string, require_real_executable,
    scenario_path, skip_hybrid_test_if_disabled, spawn_upnow, stderr, stdout, write_executable,
};

const MISE_DETERMINISTIC_SCENARIO_DIR: &str = "tests/scenarios/mise/deterministic";
const NPM_DETERMINISTIC_SCENARIO_DIR: &str = "tests/scenarios/mise/deterministic/npm";
const MISE_HYBRID_SCENARIO_DIR: &str = "tests/scenarios/mise/hybrid";
const NPM_HYBRID_SCENARIO_DIR: &str = "tests/scenarios/mise/hybrid/npm";

const DETERMINISTIC_CONFIG: &str = r#"
[mise]
mode = "apply"
min_release_age = "7d"
pinned = ["node"]
"#;

const HYBRID_CONFIG: &str = r#"
[mise]
mode = "apply"
min_release_age = "7d"
"#;

struct Sandbox {
    env: SandboxEnv,
    fake_mise: PathBuf,
    fake_npm: PathBuf,
    mise_scenario_dir: PathBuf,
    npm_scenario_dir: PathBuf,
    mise_versions_base_url: String,
    _mise_versions_server: Option<BackgroundTcpServer>,
}

impl Sandbox {
    fn new(mise_scenario_rel: &str, npm_scenario_rel: &str, config_toml: &str) -> Self {
        let sandbox_env = SandboxEnv::new("mock-mise");
        let fake_bin_dir = sandbox_env.fake_bin_dir.clone();

        sandbox_env.write_config(config_toml);

        let fake_mise = fake_bin_dir.join("mise");
        write_executable(
            &fake_mise,
            include_str!("fakes/mise.sh"),
            "fake mise script",
        );

        let fake_npm = fake_bin_dir.join("npm");
        write_executable(&fake_npm, include_str!("fakes/npm.sh"), "fake npm script");

        let mise_scenario_dir = scenario_path(mise_scenario_rel, "mise");
        let npm_scenario_dir = scenario_path(npm_scenario_rel, "npm for mise");
        let (mise_versions_base_url, mise_versions_server) =
            start_mise_versions_server(&mise_scenario_dir);

        Self {
            env: sandbox_env,
            fake_mise,
            fake_npm,
            mise_scenario_dir,
            npm_scenario_dir,
            mise_versions_base_url,
            _mise_versions_server: mise_versions_server,
        }
    }

    fn run_upnow(&self, args: &[&str]) -> Output {
        self.run_upnow_with_env(args, &[])
    }

    fn run_upnow_with_env(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        spawn_upnow(args, extra_env, |cmd| {
            self.apply_base_env(cmd);
        })
    }

    fn run_fake_mise(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.fake_mise);
        cmd.args(args);
        self.apply_base_env(&mut cmd);

        command_output(&mut cmd, "fake mise")
    }

    fn run_fake_npm(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.fake_npm);
        cmd.args(args);
        self.apply_base_env(&mut cmd);

        command_output(&mut cmd, "fake npm")
    }

    fn find_real_npm(&self) -> Option<PathBuf> {
        self.env.find_real_executable("npm")
    }

    fn apply_base_env(&self, cmd: &mut Command) {
        self.env.apply_base_env(cmd);
        cmd.env("UPNOW_FAKE_MISE_SCENARIO_DIR", &self.mise_scenario_dir);
        cmd.env("UPNOW_FAKE_NPM_SCENARIO_DIR", &self.npm_scenario_dir);
        cmd.env("UPNOW_MISE_VERSIONS_BASE_URL", &self.mise_versions_base_url);
    }
}

fn start_mise_versions_server(mise_scenario_dir: &Path) -> (String, Option<BackgroundTcpServer>) {
    let fixture_root = mise_scenario_dir.join("versions");
    if !fixture_root.is_dir() {
        return ("https://mise-versions.jdx.dev".to_string(), None);
    }

    let server = BackgroundTcpServer::start("mise versions host", move |listener, stop| {
        serve_mise_versions_host(&listener, &stop, &fixture_root);
    });

    (server.base_url(), Some(server))
}

fn serve_mise_versions_host(listener: &TcpListener, stop: &AtomicBool, fixture_root: &Path) {
    run_fake_http_server(listener, stop, |stream| {
        let Some(request) = read_http_request_head(stream) else {
            return;
        };

        let path = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/");

        if path.contains("..") {
            write_http_response_text(stream, "400 Bad Request", "text/plain", "bad request");
            return;
        }

        let rel_path = path.trim_start_matches('/');
        let file = fixture_root.join(rel_path);
        match fs::read_to_string(&file) {
            Ok(body) => write_http_response_text(stream, "200 OK", "text/plain", &body),
            Err(_) => write_http_response_text(stream, "404 Not Found", "text/plain", "not found"),
        }
    });
}

#[test]
fn fake_mise_harness_routes_commands_to_expected_fixtures() {
    let sandbox = Sandbox::new(
        MISE_DETERMINISTIC_SCENARIO_DIR,
        NPM_DETERMINISTIC_SCENARIO_DIR,
        DETERMINISTIC_CONFIG,
    );

    let dry_run = sandbox.run_fake_mise(&["upgrade", "--dry-run", "--before", "7d"]);
    assert_success(&dry_run, "fake mise dry-run");
    let dry_run_stdout = stdout(&dry_run);
    assert!(dry_run_stdout.contains("Would install npm:alpha-ready@1.2.0"));

    let ls_remote = sandbox.run_fake_mise(&["ls-remote", "--json", "core:node"]);
    assert_success(&ls_remote, "fake mise ls-remote");
    let ls_remote_stdout = stdout(&ls_remote);
    assert!(ls_remote_stdout.contains("\"20.1.0\""));

    let registry = sandbox.run_fake_mise(&["registry", "swiftformat", "--json"]);
    assert_success(&registry, "fake mise registry");
    let registry_stdout = stdout(&registry);
    assert!(registry_stdout.contains("github:nicklockwood/SwiftFormat"));

    let registry_all = sandbox.run_fake_mise(&["registry", "--json"]);
    assert_success(&registry_all, "fake mise registry all");
    let registry_all_stdout = stdout(&registry_all);
    assert!(registry_all_stdout.contains("\"fallbacktool\""));

    let outdated = sandbox.run_fake_mise(&["outdated", "--json"]);
    assert_success(&outdated, "fake mise outdated");
    let outdated_stdout = stdout(&outdated);
    assert!(outdated_stdout.contains("npm:beta-fresh-latest"));

    let npm_view = sandbox.run_fake_npm(&["view", "beta-fresh-latest@1.1.0", "time", "--json"]);
    assert_success(&npm_view, "fake npm for mise");
    let npm_view_stdout = stdout(&npm_view);
    assert!(npm_view_stdout.contains("2099-01-01"));

    let missing = sandbox.run_fake_npm(&["view", "does-not-exist@1.0.0", "time", "--json"]);
    assert_eq!(
        missing.status.code(),
        Some(66),
        "missing fixture should exit 66"
    );
}

#[test]
fn deterministic_plan_covers_updates_pinned_and_metadata_states() {
    let sandbox = Sandbox::new(
        MISE_DETERMINISTIC_SCENARIO_DIR,
        NPM_DETERMINISTIC_SCENARIO_DIR,
        DETERMINISTIC_CONFIG,
    );

    let output = sandbox.run_upnow(&[
        "plan",
        "--plain",
        "--verbose",
        "--managers",
        "mise",
        "--show-commands",
    ]);
    assert_success(&output, "upnow plan deterministic mise");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(out.contains("+ Update [mise] npm:alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [mise] npm:beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("- Skipped [mise] node v20.0.0 -> v20.1.0 (pinned)"));
    assert!(out.contains("+ Update [mise] swiftformat v0.59.1 -> v0.61.0"));
    assert!(out.contains("+ Update [mise] emsdk v5.0.4 -> v5.0.6"));
    assert!(out.contains("+ Update [mise] fallbacktool v1.0.0 -> v1.1.0"));
    assert!(out.contains("+ Update [mise] github:example/fullfallback v2.0.0 -> v2.1.0"));
    assert!(out.contains("+ Update [mise] npm:gamma-error v2.0.0 -> v2.0.1"));
    assert!(out.contains("~ Delayed [mise] fresh-tool v1.0.0 -> v1.1.0"));
    assert!(out.contains("- Skipped [mise] nometa-tool v1.0.0 -> v1.1.0"));

    assert!(err.contains("$ mise upgrade --dry-run --before 7d"));
    assert!(err.contains("$ mise outdated --json"));
    assert!(err.contains("$ npm view beta-fresh-latest@1.0.5 time --json"));
    assert!(err.contains("$ npm view beta-fresh-latest@1.1.0 time --json"));
    assert!(err.contains("$ npm view gamma-error@2.0.1 time --json"));
    assert!(err.contains("$ npm view gamma-error@2.1.0 time --json"));
    assert!(err.contains("$ mise registry fallbacktool --json"));
    assert!(err.contains("$ mise ls-remote --json github:example/fallbacktool"));
    assert!(err.contains("$ mise ls-remote --json github:example/fullfallback"));
    assert!(err.contains("$ mise registry --json"));
    assert!(err.contains("$ mise registry node --json"));
    assert!(err.contains("$ mise ls-remote --json core:node"));
    assert!(err.contains("$ mise registry swiftformat --json"));
    assert!(err.contains("$ mise ls-remote --json github:nicklockwood/SwiftFormat"));
    assert!(err.contains("$ mise registry emsdk --json"));
    assert!(err.contains("$ mise ls-remote --json asdf:mise-plugins/mise-emsdk"));
}

#[test]
fn deterministic_apply_selective_path_runs_only_for_unpinned_eligible_items() {
    let sandbox = Sandbox::new(
        MISE_DETERMINISTIC_SCENARIO_DIR,
        NPM_DETERMINISTIC_SCENARIO_DIR,
        DETERMINISTIC_CONFIG,
    );

    let output = sandbox.run_upnow(&["apply", "--plain", "--managers", "mise", "--show-commands"]);
    assert_success(&output, "upnow apply deterministic mise");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(out.contains("+ Update [mise] npm:alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [mise] npm:beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("+ Update [mise] swiftformat v0.59.1 -> v0.61.0"));
    assert!(out.contains("- Skipped [mise] node v20.0.0 -> v20.1.0 (pinned)"));
    assert!(out.contains("+ Update [mise] emsdk v5.0.4 -> v5.0.6"));
    assert!(out.contains("+ Update [mise] fallbacktool v1.0.0 -> v1.1.0"));
    assert!(out.contains("+ Update [mise] github:example/fullfallback v2.0.0 -> v2.1.0"));
    assert!(out.contains("+ Update [mise] npm:gamma-error v2.0.0 -> v2.0.1"));
    assert!(out.contains("~ Delayed [mise] fresh-tool v1.0.0 -> v1.1.0"));

    assert!(err.contains("$ mise upgrade npm:alpha-ready@1.2.0"));
    assert!(err.contains("$ mise upgrade npm:beta-fresh-latest@1.0.5"));
    assert!(err.contains("$ mise upgrade npm:gamma-error@2.0.1"));
    assert!(err.contains("$ mise upgrade swiftformat@0.61.0"));
    assert!(err.contains("$ mise upgrade emsdk@5.0.6"));
    assert!(err.contains("$ mise upgrade fallbacktool@1.1.0"));
    assert!(err.contains("$ mise upgrade github:example/fullfallback@2.1.0"));
    assert!(!err.contains("$ mise upgrade node@20.1.0"));
    assert!(!err.contains("$ mise upgrade fresh-tool@1.1.0"));
    assert!(!err.contains("$ mise upgrade nometa-tool@1.1.0"));
    assert!(!err.contains("$ mise upgrade\n"));
}

#[test]
fn deterministic_apply_runs_per_item_exact_targets_when_no_items_are_pinned() {
    let config = r#"
[mise]
mode = "apply"
min_release_age = "7d"
"#;
    let sandbox = Sandbox::new(
        MISE_DETERMINISTIC_SCENARIO_DIR,
        NPM_DETERMINISTIC_SCENARIO_DIR,
        config,
    );

    let output = sandbox.run_upnow(&["apply", "--plain", "--managers", "mise", "--show-commands"]);
    assert_success(&output, "upnow apply deterministic mise global");

    let out = stdout(&output);
    assert!(out.contains("+ Update [mise] npm:alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [mise] npm:beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("+ Update [mise] node v20.0.0 -> v20.1.0"));
    assert!(out.contains("+ Update [mise] swiftformat v0.59.1 -> v0.61.0"));
    assert!(out.contains("+ Update [mise] emsdk v5.0.4 -> v5.0.6"));
    assert!(out.contains("+ Update [mise] fallbacktool v1.0.0 -> v1.1.0"));
    assert!(out.contains("+ Update [mise] github:example/fullfallback v2.0.0 -> v2.1.0"));
    assert!(out.contains("+ Update [mise] npm:gamma-error v2.0.0 -> v2.0.1"));

    let err = stderr(&output);
    assert!(err.contains("$ mise upgrade npm:alpha-ready@1.2.0"));
    assert!(err.contains("$ mise upgrade npm:beta-fresh-latest@1.0.5"));
    assert!(err.contains("$ mise upgrade npm:gamma-error@2.0.1"));
    assert!(err.contains("$ mise upgrade node@20.1.0"));
    assert!(err.contains("$ mise upgrade swiftformat@0.61.0"));
    assert!(err.contains("$ mise upgrade emsdk@5.0.6"));
    assert!(err.contains("$ mise upgrade fallbacktool@1.1.0"));
    assert!(err.contains("$ mise upgrade github:example/fullfallback@2.1.0"));
    assert!(!err.contains("$ mise upgrade fresh-tool@1.1.0"));
    assert!(!err.contains("$ mise upgrade\n"));
}

#[test]
fn apply_still_runs_when_optional_delayed_latest_metadata_is_missing() {
    let sandbox = Sandbox::new(
        "tests/scenarios/mise/missing-delayed-latest",
        "tests/scenarios/mise/missing-delayed-latest/npm",
        HYBRID_CONFIG,
    );

    let output = sandbox.run_upnow(&["apply", "--plain", "--managers", "mise", "--show-commands"]);
    assert_success(
        &output,
        "upnow apply mise with missing delayed-latest metadata",
    );

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("+ Update [mise] npm:optional-latest-missing v1.0.0 -> v1.1.0"),
        "stdout:\n{out}\nstderr:\n{err}"
    );
    assert!(
        !out.contains("! Error [mise] npm:optional-latest-missing"),
        "stdout:\n{out}\nstderr:\n{err}"
    );
    assert!(err.contains("$ npm view optional-latest-missing@1.1.0 time --json"));
    assert!(err.contains("$ npm view optional-latest-missing@1.2.0 time --json"));
    assert!(err.contains("$ mise upgrade npm:optional-latest-missing@1.1.0"));
    assert!(!err.contains("$ mise upgrade\n"));
}

#[test]
fn deterministic_scan_uses_installed_state_and_handles_missing_npm_age() {
    let sandbox = Sandbox::new(
        MISE_DETERMINISTIC_SCENARIO_DIR,
        NPM_DETERMINISTIC_SCENARIO_DIR,
        DETERMINISTIC_CONFIG,
    );

    let output = sandbox.run_upnow(&["scan", "--plain", "--verbose", "--managers", "mise"]);
    assert_success(&output, "upnow scan deterministic mise");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("= Current [mise] npm:alpha-ready v1.0.0 (released:"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [mise] npm:scan-noage v5.0.0"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [mise] node v20.0.0"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
}

#[test]
fn configured_version_policy_is_rejected_for_mise() {
    let config = r#"
[mise]
mode = "apply"
version_policy = "any"
"#;
    let sandbox = Sandbox::new(
        MISE_DETERMINISTIC_SCENARIO_DIR,
        NPM_DETERMINISTIC_SCENARIO_DIR,
        config,
    );

    let output = sandbox.run_upnow(&["plan", "--plain", "--managers", "mise"]);

    assert!(
        !output.status.success(),
        "upnow should reject mise version_policy\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("version_policy \"any\" is not supported by this manager"),
        "stderr:\n{}",
        stderr(&output)
    );
}

#[test]
#[ignore = "requires real npm + network; run via scripts/test-hybrid.sh"]
fn hybrid_apply_uses_real_npm_time_data_with_fake_mise_state() {
    if skip_hybrid_test_if_disabled() {
        return;
    }

    let sandbox = Sandbox::new(
        MISE_HYBRID_SCENARIO_DIR,
        NPM_HYBRID_SCENARIO_DIR,
        HYBRID_CONFIG,
    );
    let real_npm_path = require_real_executable(sandbox.find_real_npm(), "npm");

    let real_npm_path = path_to_string(&real_npm_path);
    let output = sandbox.run_upnow_with_env(
        &["apply", "--plain", "--managers", "mise", "--show-commands"],
        &[
            ("UPNOW_FAKE_NPM_REAL_VIEW", "1"),
            ("UPNOW_REAL_NPM_BIN", &real_npm_path),
        ],
    );
    assert_success(&output, "upnow apply hybrid mise");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("+ Update [mise] npm:typescript v1.0.0 -> v5.9.3"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("+ Update [mise] npm:react v1.0.0 -> v19.1.0"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("+ Update [mise] npm:eslint v1.0.0 -> v9.39.1"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("! Error [mise] npm:zzzz-upnow-no-such-package-000000000000 v1.0.0 -> v1.0.0"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );

    assert!(
        err.contains("warning: real mutating commands are ENABLED")
            || err.contains("warning: apply runs with real mutating commands ENABLED"),
        "hybrid stderr:\n{err}"
    );
    assert!(err.contains("$ mise upgrade npm:typescript@"));
    assert!(err.contains("$ mise upgrade npm:react@"));
    assert!(err.contains("$ mise upgrade npm:eslint@"));
    assert!(!err.contains("$ mise upgrade\n"));
    assert!(!err.contains("$ mise upgrade npm:zzzz-upnow-no-such-package-000000000000@"));
}
