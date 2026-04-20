use std::path::PathBuf;
use std::process::{Command, Output};

mod common;

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

        Self {
            env: sandbox_env,
            fake_mise,
            fake_npm,
            mise_scenario_dir,
            npm_scenario_dir,
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
    }
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
fn deterministic_plan_covers_updates_pinned_and_age_error_states() {
    let sandbox = Sandbox::new(
        MISE_DETERMINISTIC_SCENARIO_DIR,
        NPM_DETERMINISTIC_SCENARIO_DIR,
        DETERMINISTIC_CONFIG,
    );

    let output = sandbox.run_upnow(&["plan", "--plain", "--managers", "mise", "--show-commands"]);
    assert_success(&output, "upnow plan deterministic mise");

    let out = stdout(&output);
    assert!(out.contains("+ Update [mise] npm:alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [mise] npm:beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("- Skipped [mise] node v20.0.0 -> v20.1.0 (pinned)"));
    assert!(out.contains("! Error [mise] npm:gamma-error v2.0.0 -> v2.0.0"));

    let err = stderr(&output);
    assert!(err.contains("$ mise upgrade --dry-run --before 7d"));
    assert!(err.contains("$ mise outdated --json"));
    assert!(err.contains("$ npm view beta-fresh-latest@1.1.0 time --json"));
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
    assert!(out.contains("+ Update [mise] npm:alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [mise] npm:beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("- Skipped [mise] node v20.0.0 -> v20.1.0 (pinned)"));

    let err = stderr(&output);
    assert!(err.contains("$ mise upgrade --before 7d npm:alpha-ready"));
    assert!(err.contains("$ mise upgrade --before 7d npm:beta-fresh-latest"));
    assert!(!err.contains("$ mise upgrade --before 7d\n"));
    assert!(!err.contains("$ mise upgrade --before 7d node"));
}

#[test]
fn deterministic_apply_uses_global_upgrade_when_no_items_are_pinned() {
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

    let err = stderr(&output);
    assert!(err.contains("$ mise upgrade --before 7d"));
    assert!(!err.contains("$ mise upgrade --before 7d npm:alpha-ready"));
    assert!(!err.contains("$ mise upgrade --before 7d npm:beta-fresh-latest"));
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
    assert!(err.contains("$ mise upgrade --before 7d"));
    assert!(!err.contains("$ mise upgrade --before 7d npm:typescript"));
    assert!(!err.contains("$ mise upgrade --before 7d npm:react"));
    assert!(!err.contains("$ mise upgrade --before 7d npm:eslint"));
    assert!(
        !err.contains("$ mise upgrade --before 7d npm:zzzz-upnow-no-such-package-000000000000")
    );
}
