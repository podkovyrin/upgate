use std::path::PathBuf;
use std::process::{Command, Output};

mod common;

use common::{
    SandboxEnv, assert_success, command_output, compact_stdout, path_to_string,
    require_real_executable, scenario_path, skip_hybrid_test_if_disabled, spawn_upnow, stderr,
    stdout, write_executable,
};

const DETERMINISTIC_SCENARIO: &str = "tests/scenarios/yarn/deterministic";
const HYBRID_SCENARIO: &str = "tests/scenarios/yarn/hybrid";

const DETERMINISTIC_CONFIG: &str = r#"
[yarn]
mode = "apply"
min_release_age = "7d"
pinned = ["pinned-pkg"]
"#;

const HYBRID_CONFIG: &str = r#"
[yarn]
mode = "apply"
min_release_age = "7d"
pinned = ["eslint"]
"#;

struct Sandbox {
    env: SandboxEnv,
    fake_yarn: PathBuf,
    scenario_dir: PathBuf,
}

impl Sandbox {
    fn new(scenario_rel: &str, config_toml: &str) -> Self {
        let sandbox_env = SandboxEnv::new("mock-yarn");
        let fake_bin_dir = sandbox_env.fake_bin_dir.clone();

        sandbox_env.write_config(config_toml);

        let fake_yarn = fake_bin_dir.join("yarn");
        write_executable(
            &fake_yarn,
            include_str!("fakes/yarn.sh"),
            "fake yarn script",
        );

        let scenario_dir = scenario_path(scenario_rel, "yarn");

        Self {
            env: sandbox_env,
            fake_yarn,
            scenario_dir,
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

    fn run_fake_yarn(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.fake_yarn);
        cmd.args(args);
        self.apply_base_env(&mut cmd);

        command_output(&mut cmd, "fake yarn")
    }

    fn find_real_yarn(&self) -> Option<PathBuf> {
        self.env.find_real_executable("yarn")
    }

    fn apply_base_env(&self, cmd: &mut Command) {
        self.env.apply_base_env(cmd);
        cmd.env("UPNOW_FAKE_YARN_SCENARIO_DIR", &self.scenario_dir);
    }
}

#[test]
fn fake_yarn_harness_routes_commands_to_expected_fixtures() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let version = sandbox.run_fake_yarn(&["--version"]);
    assert_success(&version, "fake yarn --version");
    let version_stdout = stdout(&version);
    assert!(version_stdout.contains("1.22.22"));

    let listed = sandbox.run_fake_yarn(&["global", "list", "--depth=0", "--json"]);
    assert_success(&listed, "fake yarn global list");
    let listed_stdout = stdout(&listed);
    assert!(listed_stdout.contains("alpha-ready@1.0.0"));
    assert!(listed_stdout.contains("pinned-pkg@3.0.0"));

    let time = sandbox.run_fake_yarn(&["info", "alpha-ready", "time", "--json"]);
    assert_success(&time, "fake yarn info time");
    let time_stdout = stdout(&time);
    assert!(time_stdout.contains("1.2.0"));

    let missing = sandbox.run_fake_yarn(&["info", "does-not-exist", "time", "--json"]);
    assert_eq!(
        missing.status.code(),
        Some(66),
        "missing fixture should exit 66"
    );
}

#[test]
fn deterministic_plan_covers_ready_delayed_pinned_and_error_states() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&["plan", "--plain", "--managers", "yarn", "--show-commands"]);
    assert_success(&output, "upnow plan deterministic yarn");

    let out = compact_stdout(&output);
    assert!(out.contains("+ Update [yarn] alpha-ready v1.0.0 v1.2.0"));
    assert!(
        out.contains("+ Update [yarn] beta-fresh-latest v1.0.0 v1.0.5 (latest v1.1.0 too fresh)")
    );
    assert!(out.contains(
        "~ Delayed [yarn] gamma-delayed v2.0.0 v2.1.0 (no eligible release yet; latest v2.1.0 too fresh)"
    ));
    assert!(out.contains("- Skipped [yarn] pinned-pkg v3.0.0 v3.1.0 (pinned)"));
    assert!(out.contains("! Error [yarn] omega-error v0.1.0 v0.1.0"));

    let err = stderr(&output);
    assert!(err.contains("$ yarn --version"));
    assert!(err.contains("$ yarn global list --depth=0 --json"));
    assert!(err.contains("$ yarn info alpha-ready time --json"));
}

#[test]
fn deterministic_apply_selective_path_runs_only_for_eligible_unpinned_packages() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&["apply", "--plain", "--managers", "yarn", "--show-commands"]);
    assert_success(&output, "upnow apply deterministic yarn");

    let out = compact_stdout(&output);
    assert!(out.contains("+ Update [yarn] alpha-ready v1.0.0 v1.2.0"));
    assert!(
        out.contains("+ Update [yarn] beta-fresh-latest v1.0.0 v1.0.5 (latest v1.1.0 too fresh)")
    );
    assert!(out.contains(
        "~ Delayed [yarn] gamma-delayed v2.0.0 v2.1.0 (no eligible release yet; latest v2.1.0 too fresh)"
    ));
    assert!(out.contains("- Skipped [yarn] pinned-pkg v3.0.0 v3.1.0 (pinned)"));

    let err = stderr(&output);
    assert!(err.contains("$ yarn global add alpha-ready@1.2.0"));
    assert!(err.contains("$ yarn global add beta-fresh-latest@1.0.5"));
    assert!(!err.contains("$ yarn global add pinned-pkg@3.1.0"));
    assert!(!err.contains("$ yarn global add gamma-delayed@2.1.0"));
}

#[test]
fn deterministic_scan_uses_fake_installed_state_and_reports_release_age_metadata() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&["scan", "--plain", "--verbose", "--managers", "yarn"]);
    assert_success(&output, "upnow scan deterministic yarn");

    let out = compact_stdout(&output);
    let err = stderr(&output);

    assert!(
        out.contains("= Current [yarn] alpha-ready v1.0.0 (released:"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [yarn] pinned-pkg v3.0.0 (released:"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [yarn] scan-noage v5.0.0"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
}

#[test]
#[ignore = "requires real yarn + network; run via scripts/test-hybrid.sh"]
fn hybrid_apply_uses_real_registry_time_data_with_fake_installed_state() {
    if skip_hybrid_test_if_disabled() {
        return;
    }

    let sandbox = Sandbox::new(HYBRID_SCENARIO, HYBRID_CONFIG);
    let real_yarn_path = require_real_executable(sandbox.find_real_yarn(), "yarn");

    let real_yarn_path = path_to_string(&real_yarn_path);
    let output = sandbox.run_upnow_with_env(
        &["apply", "--plain", "--managers", "yarn", "--show-commands"],
        &[
            ("UPNOW_FAKE_YARN_REAL_INFO", "1"),
            ("UPNOW_REAL_YARN_BIN", &real_yarn_path),
        ],
    );
    assert_success(&output, "upnow apply hybrid yarn");

    let out = compact_stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("+ Update [yarn] typescript v1.0.0 v"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("- Skipped [yarn] eslint v1.0.0 v") && out.contains("(pinned)"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        !out.contains(" react v9999.0.0 v"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("! Error [yarn] zzzz-upnow-no-such-package-000000000000 v1.0.0 v1.0.0"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );

    assert!(
        err.contains("warning: real mutating commands are ENABLED")
            || err.contains("warning: apply runs with real mutating commands ENABLED"),
        "hybrid stderr:\n{err}"
    );
    assert!(err.contains("$ yarn global add typescript@"));
    assert!(!err.contains("$ yarn global add eslint@"));
    assert!(!err.contains("$ yarn global add react@"));
    assert!(!err.contains("$ yarn global add zzzz-upnow-no-such-package-000000000000@"));
}
