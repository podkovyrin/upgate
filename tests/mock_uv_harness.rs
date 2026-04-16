use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

mod common;

use common::{
    SandboxEnv, assert_success, scenario_path, spawn_upnow, stderr, stdout, write_executable,
};

const DETERMINISTIC_UV_SCENARIO_DIR: &str = "tests/scenarios/uv/deterministic";
const HYBRID_UV_SCENARIO_DIR: &str = "tests/scenarios/uv/hybrid";
const UV_PYTHON_WRAPPER_SCRIPT: &str = concat!(
    "#!/usr/bin/env bash\nexec \"${",
    "UPNOW_REAL_PYTHON_BIN:-python3",
    "}\" \"$@\"\n"
);

const DETERMINISTIC_CONFIG: &str = r#"
[uv]
mode = "apply"
min_release_age = "7d"
pinned = ["pinned-pkg"]
"#;

const HYBRID_CONFIG: &str = r#"
[uv]
mode = "apply"
min_release_age = "7d"
pinned = ["ruff"]
"#;

const DETERMINISTIC_TOOLS: [&str; 5] = [
    "alpha-ready",
    "beta-fresh-latest",
    "gamma-delayed",
    "pinned-pkg",
    "omega-error",
];

const HYBRID_TOOLS: [&str; 4] = [
    "httpie",
    "ruff",
    "gamma-delayed",
    "zzzz-upnow-no-such-package-000000000000",
];

struct Sandbox {
    env: SandboxEnv,
    fake_uv: PathBuf,
    uv_scenario_dir: PathBuf,
    uv_tool_dir: PathBuf,
}

impl Sandbox {
    fn new(uv_scenario_rel: &str, config_toml: &str, tools: &[&str]) -> Self {
        let sandbox_env = SandboxEnv::new("mock-uv");
        let fake_bin_dir = sandbox_env.fake_bin_dir.clone();
        let uv_tool_dir = sandbox_env.root.join("uv-tools");

        fs::create_dir_all(&uv_tool_dir).expect("create fake uv tool dir");

        sandbox_env.write_config(config_toml);

        let fake_uv = fake_bin_dir.join("uv");
        write_executable(&fake_uv, include_str!("fakes/uv.sh"), "fake uv script");

        for tool in tools {
            let tool_python_dir = uv_tool_dir.join(tool).join("bin");
            fs::create_dir_all(&tool_python_dir).expect("create fake uv tool python directory");
            let tool_python = tool_python_dir.join("python");
            write_executable(
                &tool_python,
                UV_PYTHON_WRAPPER_SCRIPT,
                "fake uv tool python wrapper",
            );
        }

        let uv_scenario_dir = scenario_path(uv_scenario_rel, "uv");

        Self {
            env: sandbox_env,
            fake_uv,
            uv_scenario_dir,
            uv_tool_dir,
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

    fn run_fake_uv(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.fake_uv);
        cmd.args(args);
        self.apply_base_env(&mut cmd);

        cmd.output().expect("failed to run fake uv")
    }

    fn find_real_uv(&self) -> Option<PathBuf> {
        self.env.find_real_executable("uv")
    }

    fn find_real_python(&self) -> Option<PathBuf> {
        self.env
            .find_real_executable("python3")
            .or_else(|| self.env.find_real_executable("python"))
    }

    fn apply_base_env(&self, cmd: &mut Command) {
        self.env.apply_base_env(cmd);
        cmd.env_remove("UPNOW_FAKE_UV_REAL_PIP_DRY_RUN");
        cmd.env_remove("UPNOW_REAL_UV_BIN");
        cmd.env_remove("UPNOW_FAKE_UV_KEEP_FAKE_TOOLS");
        cmd.env("UPNOW_FAKE_UV_SCENARIO_DIR", &self.uv_scenario_dir);
        cmd.env("UPNOW_FAKE_UV_TOOL_DIR", &self.uv_tool_dir);
    }
}

#[test]
fn fake_uv_harness_routes_commands_to_expected_fixtures() {
    let sandbox = Sandbox::new(
        DETERMINISTIC_UV_SCENARIO_DIR,
        DETERMINISTIC_CONFIG,
        &DETERMINISTIC_TOOLS,
    );

    let tool_dir = sandbox.run_fake_uv(&["tool", "dir"]);
    assert_success(&tool_dir, "fake uv tool dir");
    let tool_dir_stdout = stdout(&tool_dir);
    assert!(tool_dir_stdout.contains("uv-tools"));

    let listed = sandbox.run_fake_uv(&["tool", "list", "--show-version-specifiers"]);
    assert_success(&listed, "fake uv tool list");
    let listed_stdout = stdout(&listed);
    assert!(listed_stdout.contains("alpha-ready v1.0.0"));

    let pip_plan = sandbox.run_fake_uv(&[
        "pip",
        "install",
        "--dry-run",
        "-p",
        "/tmp/python",
        "--upgrade",
        "--exclude-newer",
        "7d",
        "alpha-ready>=1.0.0",
    ]);
    assert_success(&pip_plan, "fake uv pip install --dry-run");
    let pip_plan_stderr = stderr(&pip_plan);
    assert!(pip_plan_stderr.contains("+ alpha-ready==1.2.0"));

    let missing = sandbox.run_fake_uv(&[
        "pip",
        "install",
        "--dry-run",
        "-p",
        "/tmp/python",
        "--upgrade",
        "--exclude-newer",
        "7d",
        "does-not-exist>=1.0.0",
    ]);
    assert_eq!(
        missing.status.code(),
        Some(66),
        "missing fixture should exit 66"
    );
}

#[test]
fn deterministic_plan_covers_update_delayed_pinned_and_error_states() {
    let sandbox = Sandbox::new(
        DETERMINISTIC_UV_SCENARIO_DIR,
        DETERMINISTIC_CONFIG,
        &DETERMINISTIC_TOOLS,
    );

    let output = sandbox.run_upnow(&[
        "plan",
        "--plain",
        "--verbose",
        "--managers",
        "uv",
        "--show-commands",
    ]);
    assert_success(&output, "upnow plan deterministic uv");

    let out = stdout(&output);
    assert!(out.contains("+ Update [uv] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [uv] beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("~ Delayed [uv] gamma-delayed v2.0.0 -> v2.0.0"));
    assert!(out.contains("- Skipped [uv] pinned-pkg v3.0.0 -> v3.0.0 (pinned)"));
    assert!(out.contains("! Error [uv] omega-error v0.1.0 -> v0.1.0"));

    let err = stderr(&output);
    assert!(err.contains("$ uv tool dir"));
    assert!(err.contains("$ uv tool list --show-version-specifiers"));
    assert!(err.contains("$ uv pip install --dry-run"));
}

#[test]
fn deterministic_apply_selective_path_runs_updates_only_for_eligible_unpinned_items() {
    let sandbox = Sandbox::new(
        DETERMINISTIC_UV_SCENARIO_DIR,
        DETERMINISTIC_CONFIG,
        &DETERMINISTIC_TOOLS,
    );

    let output = sandbox.run_upnow(&[
        "apply",
        "--plain",
        "--verbose",
        "--managers",
        "uv",
        "--show-commands",
    ]);
    assert_success(&output, "upnow apply deterministic uv");

    let out = stdout(&output);
    assert!(out.contains("+ Update [uv] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [uv] beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("~ Delayed [uv] gamma-delayed v2.0.0 -> v2.0.0"));
    assert!(out.contains("- Skipped [uv] pinned-pkg v3.0.0 -> v3.0.0 (pinned)"));

    let err = stderr(&output);
    assert!(err.contains("$ uv tool install --upgrade --exclude-newer 7d alpha-ready"));
    assert!(err.contains("$ uv tool install --upgrade --exclude-newer 7d beta-fresh-latest"));
    assert!(!err.contains("$ uv tool install --upgrade --exclude-newer 7d pinned-pkg"));
    assert!(!err.contains("$ uv tool install --upgrade --exclude-newer 7d gamma-delayed"));
    assert!(!err.contains("$ uv tool install --upgrade --exclude-newer 7d omega-error"));
}

#[test]
fn deterministic_scan_reports_current_state_without_network_age_lookup() {
    let sandbox = Sandbox::new(
        DETERMINISTIC_UV_SCENARIO_DIR,
        DETERMINISTIC_CONFIG,
        &DETERMINISTIC_TOOLS,
    );

    let output = sandbox.run_upnow(&["scan", "--plain", "--managers", "uv"]);
    assert_success(&output, "upnow scan deterministic uv");

    let out = stdout(&output);
    assert!(out.contains("= Current [uv] alpha-ready v1.0.0"));
    assert!(out.contains("= Current [uv] beta-fresh-latest v1.0.0"));
    assert!(out.contains("= Current [uv] gamma-delayed v2.0.0"));
    assert!(out.contains("= Current [uv] pinned-pkg v3.0.0"));
    assert!(out.contains("= Current [uv] omega-error v0.1.0"));
}

#[test]
#[ignore = "requires real uv + python + network; run via scripts/test-hybrid.sh"]
fn hybrid_apply_uses_real_pypi_resolution_with_fake_installed_state() {
    if env::var("UPNOW_RUN_HYBRID_TESTS").as_deref() != Ok("1") {
        eprintln!("skipping hybrid test; set UPNOW_RUN_HYBRID_TESTS=1 to enable");
        return;
    }

    let sandbox = Sandbox::new(HYBRID_UV_SCENARIO_DIR, HYBRID_CONFIG, &HYBRID_TOOLS);
    let Some(real_uv_path) = sandbox.find_real_uv() else {
        panic!("hybrid test requires real uv in PATH");
    };
    let Some(real_python_path) = sandbox.find_real_python() else {
        panic!("hybrid test requires real python in PATH");
    };

    let real_uv_path = real_uv_path.to_string_lossy().into_owned();
    let real_python_path = real_python_path.to_string_lossy().into_owned();
    let output = sandbox.run_upnow_with_env(
        &[
            "apply",
            "--plain",
            "--verbose",
            "--managers",
            "uv",
            "--show-commands",
        ],
        &[
            ("UPNOW_FAKE_UV_REAL_PIP_DRY_RUN", "1"),
            ("UPNOW_REAL_UV_BIN", &real_uv_path),
            ("UPNOW_REAL_PYTHON_BIN", &real_python_path),
            ("UPNOW_FAKE_UV_KEEP_FAKE_TOOLS", "gamma-delayed"),
        ],
    );
    assert_success(&output, "upnow apply hybrid uv");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("+ Update [uv] httpie v1.0.0 -> v"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("- Skipped [uv] ruff v0.1.0 -> v0.1.0 (pinned)"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("~ Delayed [uv] gamma-delayed v2.0.0 -> v2.0.0"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("! Error [uv] zzzz-upnow-no-such-package-000000000000 v1.0.0 -> v1.0.0"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );

    assert!(
        err.contains("warning: real mutating commands are ENABLED")
            || err.contains("warning: apply runs with real mutating commands ENABLED"),
        "hybrid stderr:\n{err}"
    );
    assert!(err.contains("$ uv tool install --upgrade --exclude-newer 7d httpie"));
    assert!(!err.contains("$ uv tool install --upgrade --exclude-newer 7d ruff"));
    assert!(!err.contains("$ uv tool install --upgrade --exclude-newer 7d gamma-delayed"));
    assert!(!err.contains(
        "$ uv tool install --upgrade --exclude-newer 7d zzzz-upnow-no-such-package-000000000000"
    ));
}
