use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

mod common;

use common::{
    SandboxEnv, assert_success, command_output, path_to_string, require_real_executable,
    scenario_path, skip_hybrid_test_if_disabled, spawn_upnow, stderr, stdout, write_executable,
};

const DETERMINISTIC_SCENARIO: &str = "tests/scenarios/bun/deterministic";
const HYBRID_SCENARIO: &str = "tests/scenarios/bun/hybrid";

const DETERMINISTIC_CONFIG: &str = r#"
[bun]
mode = "apply"
min_release_age = "7d"
pinned = ["pinned-pkg"]
"#;

const HYBRID_CONFIG: &str = r#"
[bun]
mode = "apply"
min_release_age = "7d"
"#;

struct Sandbox {
    env: SandboxEnv,
    fake_bun: PathBuf,
    scenario_dir: PathBuf,
}

impl Sandbox {
    fn new(scenario_rel: &str, config_toml: &str) -> Self {
        let sandbox_env = SandboxEnv::new("mock-bun");
        let fake_bin_dir = sandbox_env.fake_bin_dir.clone();
        let bun_global_dir = sandbox_env.home_dir.join(".bun/install/global");
        fs::create_dir_all(&bun_global_dir).expect("create fake Bun global dir");
        fs::write(
            bun_global_dir.join("package.json"),
            r#"{"name":"upnow-bun-harness","version":"1.0.0"}"#,
        )
        .expect("write fake Bun global package.json");

        sandbox_env.write_config(config_toml);

        let fake_bun = fake_bin_dir.join("bun");
        write_executable(&fake_bun, include_str!("fakes/bun.sh"), "fake bun script");

        let scenario_dir = scenario_path(scenario_rel, "bun");

        Self {
            env: sandbox_env,
            fake_bun,
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

    fn run_fake_bun(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.fake_bun);
        cmd.args(args);
        self.apply_base_env(&mut cmd);

        command_output(&mut cmd, "fake bun")
    }

    fn find_real_bun(&self) -> Option<PathBuf> {
        self.env.find_real_executable("bun")
    }

    fn apply_base_env(&self, cmd: &mut Command) {
        self.env.apply_base_env(cmd);
        cmd.env("UPNOW_FAKE_BUN_SCENARIO_DIR", &self.scenario_dir);
    }
}

#[test]
fn fake_bun_harness_routes_commands_to_expected_fixtures() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let listed = sandbox.run_fake_bun(&["pm", "ls", "-g", "--json"]);
    assert_success(&listed, "fake bun pm ls");
    let listed_stdout = stdout(&listed);
    assert!(listed_stdout.contains("alpha-ready"));
    assert!(listed_stdout.contains("scan-noage"));

    let view = sandbox.run_fake_bun(&[
        "pm",
        "view",
        "alpha-ready",
        "time",
        "--json",
        "--cwd",
        "/tmp/mock",
    ]);
    assert_success(&view, "fake bun pm view alpha-ready");
    let view_stdout = stdout(&view);
    assert!(view_stdout.contains("1.2.0"));

    let scan_noage = sandbox.run_fake_bun(&[
        "pm",
        "view",
        "scan-noage",
        "time",
        "--json",
        "--cwd",
        "/tmp/mock",
    ]);
    assert_eq!(
        scan_noage.status.code(),
        Some(1),
        "scan-noage should simulate a non-zero bun view"
    );

    let missing = sandbox.run_fake_bun(&[
        "pm",
        "view",
        "does-not-exist",
        "time",
        "--json",
        "--cwd",
        "/tmp/mock",
    ]);
    assert_eq!(
        missing.status.code(),
        Some(66),
        "missing fixture should exit 66"
    );
}

#[test]
fn deterministic_plan_covers_ready_delayed_pinned_and_error_states() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&["plan", "--plain", "--managers", "bun", "--show-commands"]);
    assert_success(&output, "upnow plan deterministic bun");

    let out = stdout(&output);
    assert!(out.contains("+ Update [bun] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [bun] beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("~ Delayed [bun] gamma-delayed v2.0.0 -> v2.1.0"));
    assert!(out.contains("- Skipped [bun] pinned-pkg v3.0.0 -> v3.1.0 (pinned)"));
    assert!(out.contains("! Error [bun] omega-error v0.1.0 -> v0.1.0"));

    let err = stderr(&output);
    assert!(err.contains("$ bun pm ls -g --json"));
    assert!(err.contains("$ bun pm view alpha-ready time --json --cwd"));
}

#[test]
fn deterministic_apply_selective_path_runs_only_for_eligible_unpinned_packages() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&["apply", "--plain", "--managers", "bun", "--show-commands"]);
    assert_success(&output, "upnow apply deterministic bun");

    let out = stdout(&output);
    assert!(out.contains("+ Update [bun] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [bun] beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("~ Delayed [bun] gamma-delayed v2.0.0 -> v2.1.0"));
    assert!(out.contains("- Skipped [bun] pinned-pkg v3.0.0 -> v3.1.0 (pinned)"));

    let err = stderr(&output);
    assert!(err.contains("$ bun update -g alpha-ready@1.2.0 --minimum-release-age 604800"));
    assert!(err.contains("$ bun update -g beta-fresh-latest@1.0.5 --minimum-release-age 604800"));
    assert!(!err.contains("$ bun update -g --minimum-release-age 604800"));
    assert!(!err.contains("$ bun update -g pinned-pkg@3.1.0 --minimum-release-age 604800"));
    assert!(!err.contains("$ bun update -g gamma-delayed@2.1.0 --minimum-release-age 604800"));
}

#[test]
fn deterministic_apply_uses_global_update_when_no_items_are_pinned() {
    let config = r#"
[bun]
mode = "apply"
min_release_age = "7d"
"#;
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, config);

    let output = sandbox.run_upnow(&["apply", "--plain", "--managers", "bun", "--show-commands"]);
    assert_success(&output, "upnow apply deterministic bun global");

    let out = stdout(&output);
    assert!(out.contains("+ Update [bun] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [bun] beta-fresh-latest v1.0.0 -> v1.0.5"));

    let err = stderr(&output);
    assert!(err.contains("$ bun update -g --minimum-release-age 604800"));
    assert!(!err.contains("$ bun update -g alpha-ready@1.2.0 --minimum-release-age 604800"));
    assert!(!err.contains("$ bun update -g beta-fresh-latest@1.0.5 --minimum-release-age 604800"));
}

#[test]
fn deterministic_scan_uses_fake_installed_state_and_reports_release_age_metadata() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&["scan", "--plain", "--verbose", "--managers", "bun"]);
    assert_success(&output, "upnow scan deterministic bun");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("= Current [bun] alpha-ready v1.0.0 (released:"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [bun] pinned-pkg v3.0.0 (released:"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [bun] scan-noage v5.0.0"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
}

#[test]
#[ignore = "requires real bun + network; run via scripts/test-hybrid.sh"]
fn hybrid_apply_uses_real_registry_time_data_with_fake_installed_state() {
    if skip_hybrid_test_if_disabled() {
        return;
    }

    let sandbox = Sandbox::new(HYBRID_SCENARIO, HYBRID_CONFIG);
    let real_bun_path = require_real_executable(sandbox.find_real_bun(), "bun");

    let real_bun_path = path_to_string(&real_bun_path);
    let output = sandbox.run_upnow_with_env(
        &["apply", "--plain", "--managers", "bun", "--show-commands"],
        &[
            ("UPNOW_FAKE_BUN_REAL_VIEW", "1"),
            ("UPNOW_REAL_BUN_BIN", &real_bun_path),
        ],
    );
    assert_success(&output, "upnow apply hybrid bun");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("+ Update [bun] typescript v1.0.0 -> v"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("+ Update [bun] eslint v1.0.0 -> v"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        !out.contains(" react v9999.0.0 -> v"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("! Error [bun] zzzz-upnow-no-such-package-000000000000 v1.0.0 -> v1.0.0"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );

    assert!(
        err.contains("warning: real mutating commands are ENABLED")
            || err.contains("warning: apply runs with real mutating commands ENABLED"),
        "hybrid stderr:\n{err}"
    );
    assert!(err.contains("$ bun update -g --minimum-release-age 604800"));
    assert!(!err.contains("$ bun update -g typescript@"));
    assert!(!err.contains("$ bun update -g eslint@"));
    assert!(!err.contains("$ bun update -g react@"));
    assert!(!err.contains("$ bun update -g zzzz-upnow-no-such-package-000000000000@"));
}
