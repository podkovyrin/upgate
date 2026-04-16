use std::env;
use std::path::PathBuf;
use std::process::{Command, Output};

mod common;

use common::{
    SandboxEnv, assert_success, scenario_path, spawn_upnow, stderr, stdout, write_executable,
};

const DETERMINISTIC_SCENARIO: &str = "tests/scenarios/npm/deterministic";
const HYBRID_SCENARIO: &str = "tests/scenarios/npm/hybrid";

const DETERMINISTIC_CONFIG: &str = r#"
[npm]
mode = "apply"
min_release_age = "7d"
pinned = ["pinned-pkg"]
"#;

const HYBRID_CONFIG: &str = r#"
[npm]
mode = "apply"
min_release_age = "7d"
"#;

struct Sandbox {
    env: SandboxEnv,
    fake_npm: PathBuf,
    scenario_dir: PathBuf,
}

impl Sandbox {
    fn new(scenario_rel: &str, config_toml: &str) -> Self {
        let sandbox_env = SandboxEnv::new("mock-npm");
        let fake_bin_dir = sandbox_env.fake_bin_dir.clone();

        sandbox_env.write_config(config_toml);

        let fake_npm = fake_bin_dir.join("npm");
        write_executable(&fake_npm, include_str!("fakes/npm.sh"), "fake npm script");

        let scenario_dir = scenario_path(scenario_rel, "npm");

        Self {
            env: sandbox_env,
            fake_npm,
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

    fn run_fake_npm(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(&self.fake_npm);
        cmd.args(args);
        self.apply_base_env(&mut cmd);
        for (key, value) in extra_env {
            cmd.env(key, value);
        }

        cmd.output().expect("failed to run fake npm")
    }

    fn find_real_npm(&self) -> Option<PathBuf> {
        self.env.find_real_executable("npm")
    }

    fn apply_base_env(&self, cmd: &mut Command) {
        self.env.apply_base_env(cmd);
        cmd.env("UPNOW_FAKE_NPM_SCENARIO_DIR", &self.scenario_dir);
    }
}

#[test]
fn fake_npm_harness_routes_commands_to_expected_fixtures() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let outdated = sandbox.run_fake_npm(&["outdated", "-g", "--json"], &[]);
    assert_eq!(outdated.status.code(), Some(1), "outdated should exit 1");
    let outdated_stdout = stdout(&outdated);
    assert!(outdated_stdout.contains("alpha-ready"));
    assert!(outdated_stdout.contains("pinned-pkg"));

    let installed = sandbox.run_fake_npm(&["ls", "-g", "--depth=0", "--json"], &[]);
    assert_success(&installed, "fake npm ls");
    let installed_stdout = stdout(&installed);
    assert!(installed_stdout.contains("stale-tool"));

    let time = sandbox.run_fake_npm(&["view", "alpha-ready", "time", "--json"], &[]);
    assert_success(&time, "fake npm view time");
    let time_stdout = stdout(&time);
    assert!(time_stdout.contains("1.2.0"));

    let missing = sandbox.run_fake_npm(&["view", "does-not-exist", "time", "--json"], &[]);
    assert_eq!(
        missing.status.code(),
        Some(66),
        "missing fixture should exit 66"
    );
}

#[test]
fn deterministic_plan_covers_ready_delayed_pinned_and_error_states() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&["plan", "--plain", "--managers", "npm", "--show-commands"]);
    assert_success(&output, "upnow plan deterministic");

    let out = stdout(&output);
    assert!(out.contains("+ Update [npm] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [npm] beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("~ Delayed [npm] gamma-delayed v2.0.0 -> v2.1.0"));
    assert!(out.contains("- Skipped [npm] pinned-pkg v3.0.0 -> v3.0.0 (pinned)"));
    assert!(out.contains("! Error [npm] omega-error v0.1.0 -> v0.1.0"));

    let err = stderr(&output);
    assert!(err.contains("$ npm outdated -g --json"));
    assert!(err.contains("$ npm view alpha-ready time --json"));
}

#[test]
fn deterministic_apply_selective_path_runs_only_for_eligible_unpinned_packages() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&["apply", "--plain", "--managers", "npm", "--show-commands"]);
    assert_success(&output, "upnow apply deterministic");

    let out = stdout(&output);
    assert!(out.contains("+ Update [npm] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [npm] beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("~ Delayed [npm] gamma-delayed v2.0.0 -> v2.1.0"));
    assert!(out.contains("- Skipped [npm] pinned-pkg v3.0.0 -> v3.0.0 (pinned)"));

    let err = stderr(&output);
    assert!(err.contains("$ npm -g update alpha-ready --min-release-age 7"));
    assert!(err.contains("$ npm -g update beta-fresh-latest --min-release-age 7"));
    assert!(!err.contains("$ npm -g update --min-release-age 7"));
    assert!(!err.contains("$ npm -g update pinned-pkg --min-release-age 7"));
    assert!(!err.contains("$ npm -g update gamma-delayed --min-release-age 7"));
}

#[test]
fn deterministic_scan_uses_fake_installed_state_and_reports_release_age_metadata() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&["scan", "--plain", "--verbose", "--managers", "npm"]);
    assert_success(&output, "upnow scan deterministic");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("= Current [npm] stale-tool v1.0.0 (released:"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [npm] fresh-tool v2.0.0 (released:"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [npm] missing-age v3.0.0 (source: npm)"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
}

#[test]
fn deterministic_apply_uses_global_update_when_no_items_are_pinned() {
    let config = r#"
[npm]
mode = "apply"
min_release_age = "7d"
"#;
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, config);

    let output = sandbox.run_upnow(&["apply", "--plain", "--managers", "npm", "--show-commands"]);
    assert_success(&output, "upnow apply deterministic npm global");

    let out = stdout(&output);
    assert!(out.contains("+ Update [npm] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [npm] beta-fresh-latest v1.0.0 -> v1.0.5"));

    let err = stderr(&output);
    assert!(err.contains("$ npm -g update --min-release-age 7"));
    assert!(!err.contains("$ npm -g update alpha-ready --min-release-age 7"));
    assert!(!err.contains("$ npm -g update beta-fresh-latest --min-release-age 7"));
}

#[test]
#[ignore = "requires real npm + network; run via scripts/test-hybrid.sh"]
fn hybrid_apply_uses_real_registry_time_data_with_fake_installed_state() {
    if env::var("UPNOW_RUN_HYBRID_TESTS").as_deref() != Ok("1") {
        eprintln!("skipping hybrid test; set UPNOW_RUN_HYBRID_TESTS=1 to enable");
        return;
    }

    let sandbox = Sandbox::new(HYBRID_SCENARIO, HYBRID_CONFIG);
    let Some(real_npm_path) = sandbox.find_real_npm() else {
        panic!("hybrid test requires real npm in PATH");
    };

    let real_npm_path = real_npm_path.to_string_lossy().into_owned();
    let output = sandbox.run_upnow_with_env(
        &["apply", "--plain", "--managers", "npm", "--show-commands"],
        &[
            ("UPNOW_FAKE_NPM_REAL_VIEW", "1"),
            ("UPNOW_REAL_NPM_BIN", &real_npm_path),
        ],
    );

    assert_success(&output, "upnow apply hybrid");

    let out = stdout(&output);
    assert!(out.contains("+ Update [npm] typescript v1.0.0 -> v"));
    assert!(out.contains("+ Update [npm] eslint v1.0.0 -> v"));
    assert!(!out.contains(" react v9999.0.0 -> v"));

    let err = stderr(&output);
    assert!(
        err.contains("warning: real mutating commands are ENABLED")
            || err.contains("warning: apply runs with real mutating commands ENABLED"),
        "hybrid stderr:\n{err}"
    );
    assert!(err.contains("$ npm -g update --min-release-age 7"));
    assert!(!err.contains("$ npm -g update typescript --min-release-age 7"));
    assert!(!err.contains("$ npm -g update eslint --min-release-age 7"));
    assert!(!err.contains("$ npm -g update react --min-release-age 7"));
}
