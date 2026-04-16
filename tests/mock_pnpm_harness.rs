use std::env;
use std::path::PathBuf;
use std::process::{Command, Output};

mod common;

use common::{
    SandboxEnv, assert_success, scenario_path, spawn_upnow, stderr, stdout, write_executable,
};

const DETERMINISTIC_SCENARIO: &str = "tests/scenarios/pnpm/deterministic";
const HYBRID_SCENARIO: &str = "tests/scenarios/pnpm/hybrid";

const DETERMINISTIC_CONFIG: &str = r#"
[pnpm]
mode = "apply"
min_release_age = "7d"
pinned = ["pinned-pkg"]
"#;

const HYBRID_CONFIG: &str = r#"
[pnpm]
mode = "apply"
min_release_age = "7d"
pinned = ["eslint"]
"#;

struct Sandbox {
    env: SandboxEnv,
    fake_pnpm: PathBuf,
    scenario_dir: PathBuf,
}

impl Sandbox {
    fn new(scenario_rel: &str, config_toml: &str) -> Self {
        let sandbox_env = SandboxEnv::new("mock-pnpm");
        let fake_bin_dir = sandbox_env.fake_bin_dir.clone();

        sandbox_env.write_config(config_toml);

        let fake_pnpm = fake_bin_dir.join("pnpm");
        write_executable(
            &fake_pnpm,
            include_str!("fakes/pnpm.sh"),
            "fake pnpm script",
        );

        let scenario_dir = scenario_path(scenario_rel, "pnpm");

        Self {
            env: sandbox_env,
            fake_pnpm,
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

    fn run_fake_pnpm(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.fake_pnpm);
        cmd.args(args);
        self.apply_base_env(&mut cmd);

        cmd.output().expect("failed to run fake pnpm")
    }

    fn find_real_pnpm(&self) -> Option<PathBuf> {
        self.env.find_real_executable("pnpm")
    }

    fn apply_base_env(&self, cmd: &mut Command) {
        self.env.apply_base_env(cmd);
        cmd.env("UPNOW_FAKE_PNPM_SCENARIO_DIR", &self.scenario_dir);
    }
}

#[test]
fn fake_pnpm_harness_routes_commands_to_expected_fixtures() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let outdated = sandbox.run_fake_pnpm(&["outdated", "-g", "--json"]);
    assert_eq!(outdated.status.code(), Some(1), "outdated should exit 1");
    let outdated_stdout = stdout(&outdated);
    assert!(outdated_stdout.contains("alpha-ready"));
    assert!(outdated_stdout.contains("pinned-pkg"));

    let listed = sandbox.run_fake_pnpm(&["list", "-g", "--depth", "0", "--json"]);
    assert_success(&listed, "fake pnpm list");
    let listed_stdout = stdout(&listed);
    assert!(listed_stdout.contains("scan-noage"));

    let time = sandbox.run_fake_pnpm(&["view", "alpha-ready", "time", "--json"]);
    assert_success(&time, "fake pnpm view time");
    let time_stdout = stdout(&time);
    assert!(time_stdout.contains("1.2.0"));

    let missing = sandbox.run_fake_pnpm(&["view", "does-not-exist", "time", "--json"]);
    assert_eq!(
        missing.status.code(),
        Some(66),
        "missing fixture should exit 66"
    );
}

#[test]
fn deterministic_plan_covers_ready_delayed_pinned_and_error_states() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&["plan", "--plain", "--managers", "pnpm", "--show-commands"]);
    assert_success(&output, "upnow plan deterministic pnpm");

    let out = stdout(&output);
    assert!(out.contains("+ Update [pnpm] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [pnpm] beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("~ Delayed [pnpm] gamma-delayed v2.0.0 -> v2.1.0"));
    assert!(out.contains("- Skipped [pnpm] pinned-pkg v3.0.0 -> v3.0.0 (pinned)"));
    assert!(out.contains("! Error [pnpm] omega-error v0.1.0 -> v0.1.0"));

    let err = stderr(&output);
    assert!(err.contains("$ pnpm outdated -g --json"));
    assert!(err.contains("$ pnpm view alpha-ready time --json"));
}

#[test]
fn deterministic_apply_selective_path_runs_only_for_eligible_unpinned_packages() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&["apply", "--plain", "--managers", "pnpm", "--show-commands"]);
    assert_success(&output, "upnow apply deterministic pnpm");

    let out = stdout(&output);
    assert!(out.contains("+ Update [pnpm] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [pnpm] beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("~ Delayed [pnpm] gamma-delayed v2.0.0 -> v2.1.0"));
    assert!(out.contains("- Skipped [pnpm] pinned-pkg v3.0.0 -> v3.0.0 (pinned)"));

    let err = stderr(&output);
    assert!(err.contains("$ pnpm add -g alpha-ready@1.2.0"));
    assert!(err.contains("$ pnpm add -g beta-fresh-latest@1.0.5"));
    assert!(!err.contains("$ pnpm add -g pinned-pkg@3.1.0"));
    assert!(!err.contains("$ pnpm add -g gamma-delayed@2.1.0"));
}

#[test]
fn deterministic_scan_uses_fake_installed_state_and_reports_release_age_metadata() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG);

    let output = sandbox.run_upnow(&["scan", "--plain", "--verbose", "--managers", "pnpm"]);
    assert_success(&output, "upnow scan deterministic pnpm");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("= Current [pnpm] alpha-ready v1.0.0 (released:"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [pnpm] pinned-pkg v3.0.0 (released:"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [pnpm] scan-noage v5.0.0 (source: pnpm)"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
}

#[test]
#[ignore = "requires real pnpm + network; run via scripts/test-hybrid.sh"]
fn hybrid_apply_uses_real_registry_time_data_with_fake_installed_state() {
    if env::var("UPNOW_RUN_HYBRID_TESTS").as_deref() != Ok("1") {
        eprintln!("skipping hybrid test; set UPNOW_RUN_HYBRID_TESTS=1 to enable");
        return;
    }

    let sandbox = Sandbox::new(HYBRID_SCENARIO, HYBRID_CONFIG);
    let Some(real_pnpm_path) = sandbox.find_real_pnpm() else {
        panic!("hybrid test requires real pnpm in PATH");
    };

    let real_pnpm_path = real_pnpm_path.to_string_lossy().into_owned();
    let output = sandbox.run_upnow_with_env(
        &["apply", "--plain", "--managers", "pnpm", "--show-commands"],
        &[
            ("UPNOW_FAKE_PNPM_REAL_VIEW", "1"),
            ("UPNOW_REAL_PNPM_BIN", &real_pnpm_path),
        ],
    );
    assert_success(&output, "upnow apply hybrid pnpm");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("+ Update [pnpm] typescript v1.0.0 -> v"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("- Skipped [pnpm] eslint v1.0.0 -> v1.0.0 (pinned)"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        !out.contains(" react v9999.0.0 -> v"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("! Error [pnpm] zzzz-upnow-no-such-package-000000000000 v1.0.0 -> v1.0.0"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );

    assert!(
        err.contains("warning: real mutating commands are ENABLED")
            || err.contains("warning: apply runs with real mutating commands ENABLED"),
        "hybrid stderr:\n{err}"
    );
    assert!(err.contains("$ pnpm add -g typescript@"));
    assert!(!err.contains("$ pnpm add -g eslint@"));
    assert!(!err.contains("$ pnpm add -g react@"));
    assert!(!err.contains("$ pnpm add -g zzzz-upnow-no-such-package-000000000000@"));
}
