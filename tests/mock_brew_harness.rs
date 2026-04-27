use std::path::PathBuf;
use std::process::{Command, Output};

mod common;

use common::{
    SandboxEnv, assert_success, command_output, compact_stdout, path_to_string,
    require_real_executable, scenario_path, skip_hybrid_test_if_disabled, spawn_upnow, stderr,
    stdout, write_executable,
};

const DETERMINISTIC_BREW_SCENARIO_DIR: &str = "tests/scenarios/brew/deterministic";
const HYBRID_BREW_SCENARIO_DIR: &str = "tests/scenarios/brew/hybrid";

const DETERMINISTIC_CONFIG: &str = r#"
[brew]
mode = "apply"
no_update = true
min_release_age = "12h"
pinned = ["pinned-pkg"]
"#;

const HYBRID_CONFIG: &str = r#"
[brew]
mode = "apply"
no_update = true
min_release_age = "12h"
pinned = ["grep"]
"#;

struct Sandbox {
    env: SandboxEnv,
    fake_brew: PathBuf,
    fake_git: PathBuf,
    brew_scenario_dir: PathBuf,
}

impl Sandbox {
    fn new(brew_scenario_rel: &str, config_toml: &str) -> Self {
        let sandbox_env = SandboxEnv::new("mock-brew");
        let fake_bin_dir = sandbox_env.fake_bin_dir.clone();
        sandbox_env.write_config(config_toml);

        let fake_brew = fake_bin_dir.join("brew");
        write_executable(
            &fake_brew,
            include_str!("fakes/brew.sh"),
            "fake brew script",
        );

        let fake_git = fake_bin_dir.join("git");
        write_executable(&fake_git, include_str!("fakes/git.sh"), "fake git script");

        let brew_scenario_dir = scenario_path(brew_scenario_rel, "brew");

        Self {
            env: sandbox_env,
            fake_brew,
            fake_git,
            brew_scenario_dir,
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

    fn run_fake_brew(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.fake_brew);
        cmd.args(args);
        self.apply_base_env(&mut cmd);

        command_output(&mut cmd, "fake brew")
    }

    fn run_fake_git(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.fake_git);
        cmd.args(args);
        self.apply_base_env(&mut cmd);

        command_output(&mut cmd, "fake git")
    }

    fn find_real_brew(&self) -> Option<PathBuf> {
        self.env.find_real_executable("brew")
    }

    fn find_real_git(&self) -> Option<PathBuf> {
        self.env.find_real_executable("git")
    }

    fn apply_base_env(&self, cmd: &mut Command) {
        self.env.apply_base_env(cmd);
        cmd.env("UPNOW_FAKE_BREW_SCENARIO_DIR", &self.brew_scenario_dir);
    }
}

#[test]
fn fake_brew_harness_routes_commands_to_expected_fixtures() {
    let sandbox = Sandbox::new(DETERMINISTIC_BREW_SCENARIO_DIR, DETERMINISTIC_CONFIG);

    let outdated = sandbox.run_fake_brew(&["outdated", "--json=v2"]);
    assert_success(&outdated, "fake brew outdated");
    let outdated_stdout = stdout(&outdated);
    assert!(outdated_stdout.contains("alpha-ready"));
    assert!(outdated_stdout.contains("omega-error"));

    let info_plan = sandbox.run_fake_brew(&["info", "--json=v2", "alpha-ready"]);
    assert_success(&info_plan, "fake brew info plan");
    let info_plan_stdout = stdout(&info_plan);
    assert!(info_plan_stdout.contains("Formula/alpha-ready.rb"));

    let tap_info = sandbox.run_fake_brew(&["tap-info", "--json", "--installed"]);
    assert_success(&tap_info, "fake brew tap-info");
    let tap_info_stdout = stdout(&tap_info);
    assert!(tap_info_stdout.contains("local/tap"));

    let git_log = sandbox.run_fake_git(&[
        "-C",
        "/tmp/local-tap",
        "log",
        "-1",
        "--format=%ct",
        "origin/main",
        "--",
        "Formula/alpha-ready.rb",
    ]);
    assert_success(&git_log, "fake git log");
    let git_log_stdout = stdout(&git_log);
    assert_eq!(git_log_stdout.trim(), "1000000000");
}

#[test]
fn deterministic_plan_covers_update_delayed_pinned_and_age_check_error_states() {
    let sandbox = Sandbox::new(DETERMINISTIC_BREW_SCENARIO_DIR, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&[
        "plan",
        "--plain",
        "--verbose",
        "--managers",
        "brew",
        "--show-commands",
    ]);
    assert_success(&output, "upnow plan deterministic brew");

    let out = compact_stdout(&output);
    assert!(out.contains("+ Update [brew] alpha-ready v1.0.0 v1.2.0"));
    assert!(out.contains("~ Delayed [brew] beta-fresh-latest v1.0.0 v1.1.0 (too fresh: 0s < 12h)"));
    assert!(out.contains("- Skipped [brew] pinned-pkg v3.0.0 v3.1.0 (pinned)"));
    assert!(out.contains("! Error [brew] omega-error v0.1.0 v0.2.0"));

    let err = stderr(&output);
    assert!(err.contains("$ brew outdated --json=v2"));
    assert!(err.contains("$ brew info --json=v2"));
    assert!(err.contains("$ brew tap-info --json --installed"));
    assert!(err.contains("$ git -C /tmp/local-tap log -1 --format=%ct"));
}

#[test]
fn deterministic_apply_selective_path_runs_formula_upgrade_for_only_unpinned_eligible_items() {
    let sandbox = Sandbox::new(DETERMINISTIC_BREW_SCENARIO_DIR, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&[
        "apply",
        "--plain",
        "--verbose",
        "--managers",
        "brew",
        "--show-commands",
    ]);
    assert_success(&output, "upnow apply deterministic brew");

    let out = compact_stdout(&output);
    assert!(out.contains("+ Update [brew] alpha-ready v1.0.0 v1.2.0"));
    assert!(out.contains("~ Delayed [brew] beta-fresh-latest v1.0.0 v1.1.0 (too fresh: 0s < 12h)"));
    assert!(out.contains("- Skipped [brew] pinned-pkg v3.0.0 v3.1.0 (pinned)"));

    let err = stderr(&output);
    assert!(err.contains("$ brew upgrade --formula alpha-ready"));
    assert!(!err.contains("$ brew upgrade --formula alpha-ready pinned-pkg"));
    assert!(!err.contains("$ brew upgrade --formula pinned-pkg"));
    assert!(!err.contains("$ brew upgrade --cask"));
}

#[test]
fn deterministic_scan_reports_current_installed_state() {
    let sandbox = Sandbox::new(DETERMINISTIC_BREW_SCENARIO_DIR, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&["scan", "--plain", "--managers", "brew"]);
    assert_success(&output, "upnow scan deterministic brew");

    let out = compact_stdout(&output);
    assert!(out.contains("= Current [brew] alpha-ready v1.0.0"));
    assert!(out.contains("= Current [brew] pinned-pkg v3.0.0"));
}

#[test]
#[ignore = "requires real brew + git + network; run via scripts/test-hybrid.sh"]
fn hybrid_apply_uses_real_tap_metadata_and_git_history_with_fake_outdated_state() {
    if skip_hybrid_test_if_disabled() {
        return;
    }

    let sandbox = Sandbox::new(HYBRID_BREW_SCENARIO_DIR, HYBRID_CONFIG);
    let real_brew_path = require_real_executable(sandbox.find_real_brew(), "brew");
    let real_git_path = require_real_executable(sandbox.find_real_git(), "git");

    let real_brew_path = path_to_string(&real_brew_path);
    let real_git_path = path_to_string(&real_git_path);
    let output = sandbox.run_upnow_with_env(
        &[
            "apply",
            "--plain",
            "--verbose",
            "--managers",
            "brew",
            "--show-commands",
        ],
        &[
            ("UPNOW_FAKE_BREW_REAL_TAP_INFO", "1"),
            ("UPNOW_REAL_BREW_BIN", &real_brew_path),
            ("UPNOW_FAKE_GIT_REAL_LOG", "1"),
            ("UPNOW_REAL_GIT_BIN", &real_git_path),
            (
                "UPNOW_FAKE_GIT_KEEP_FAKE_PATHS",
                "Formula/jq.rb,Formula/zzzz-upnow-no-such-formula-000000000000.rb",
            ),
        ],
    );
    assert_success(&output, "upnow apply hybrid brew");

    let out = compact_stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("+ Update [brew] wget v1.0.0 v1.2.0"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("~ Delayed [brew] jq v1.0.0 v1.1.0 (too fresh:"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("- Skipped [brew] grep v1.0.0 v1.1.0 (pinned)"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("! Error [brew] curl v1.0.0 v1.1.0"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );

    assert!(
        err.contains("warning: real mutating commands are ENABLED")
            || err.contains("warning: apply runs with real mutating commands ENABLED"),
        "hybrid stderr:\n{err}"
    );
    assert!(err.contains("$ brew tap-info --json --installed"));
    assert!(err.contains("$ git -C "));
    assert!(err.contains("$ brew upgrade --formula wget"));
    assert!(!err.contains("$ brew upgrade --formula grep"));
    assert!(!err.contains("$ brew upgrade --formula jq"));
    assert!(!err.contains("$ brew upgrade --formula curl"));
}
