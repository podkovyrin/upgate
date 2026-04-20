use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

mod common;

use common::{
    SandboxEnv, assert_success, command_output, path_to_string, require_real_executable,
    skip_hybrid_test_if_disabled, spawn_upnow, stderr, stdout, write_executable,
};

const DETERMINISTIC_CONFIG: &str = r#"
[go]
mode = "apply"
min_release_age = "7d"
pinned = ["pinned-pkg"]
"#;

const HYBRID_CONFIG: &str = r#"
[go]
mode = "apply"
min_release_age = "7d"
pinned = ["pinned-pkg"]
"#;

const DETERMINISTIC_TOOL_BINARIES: [&str; 7] = [
    "alpha-ready",
    "beta-fresh-latest",
    "gamma-delayed",
    "omega-error",
    "pinned-pkg",
    "scan-noage",
    "skip-nometa",
];

const HYBRID_TOOL_BINARIES: [&str; 4] =
    ["alpha-ready", "gamma-delayed", "omega-error", "pinned-pkg"];

struct Sandbox {
    env: SandboxEnv,
    fake_go: PathBuf,
    gobin_dir: PathBuf,
}

impl Sandbox {
    fn new(config_toml: &str, tool_binaries: &[&str]) -> Self {
        let sandbox_env = SandboxEnv::new("mock-go");
        let fake_bin_dir = sandbox_env.fake_bin_dir.clone();
        let gobin_dir = sandbox_env.root.join("gobin");

        fs::create_dir_all(&gobin_dir).expect("create fake GOBIN dir");

        sandbox_env.write_config(config_toml);

        let fake_go = fake_bin_dir.join("go");
        write_executable(&fake_go, include_str!("fakes/go.sh"), "fake go script");

        for tool in tool_binaries {
            let path = gobin_dir.join(tool);
            write_executable(&path, "#!/bin/sh\nexit 0\n", "fake tool binary");
        }

        Self {
            env: sandbox_env,
            fake_go,
            gobin_dir,
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

    fn run_fake_go(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.fake_go);
        cmd.args(args);
        self.apply_base_env(&mut cmd);

        command_output(&mut cmd, "fake go")
    }

    fn find_real_go(&self) -> Option<PathBuf> {
        self.env.find_real_executable("go")
    }

    fn apply_base_env(&self, cmd: &mut Command) {
        self.env.apply_base_env(cmd);
        cmd.env("GOBIN", &self.gobin_dir);
    }
}

#[test]
fn fake_go_harness_routes_commands_to_expected_fixtures() {
    let sandbox = Sandbox::new(DETERMINISTIC_CONFIG, &DETERMINISTIC_TOOL_BINARIES);

    let bin_path = sandbox.gobin_dir.join("alpha-ready");
    let bin_path_str = path_to_string(&bin_path);
    let version = sandbox.run_fake_go(&["version", "-m", &bin_path_str]);
    assert_success(&version, "fake go version -m alpha-ready");
    let metadata_output = stdout(&version);
    assert!(metadata_output.contains("example.com/alpha"));

    let versions = sandbox.run_fake_go(&["list", "-m", "-json", "-versions", "example.com/alpha"]);
    assert_success(&versions, "fake go list versions");
    let listing_output = stdout(&versions);
    assert!(listing_output.contains("v1.2.0"));

    let skip_path = sandbox.gobin_dir.join("skip-nometa");
    let skip_path_str = path_to_string(&skip_path);
    let skip = sandbox.run_fake_go(&["version", "-m", &skip_path_str]);
    assert_eq!(
        skip.status.code(),
        Some(1),
        "skip-nometa should fail metadata probe"
    );
}

#[test]
fn deterministic_plan_covers_ready_delayed_pinned_skipped_and_error_states() {
    let sandbox = Sandbox::new(DETERMINISTIC_CONFIG, &DETERMINISTIC_TOOL_BINARIES);

    let output = sandbox.run_upnow(&[
        "plan",
        "--plain",
        "--verbose",
        "--managers",
        "go",
        "--show-commands",
    ]);
    assert_success(&output, "upnow plan deterministic go");

    let out = stdout(&output);
    assert!(out.contains("+ Update [go] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [go] beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("~ Delayed [go] gamma-delayed v2.0.0 -> v2.1.0"));
    assert!(out.contains("- Skipped [go] pinned-pkg v3.0.0 -> v3.1.0 (pinned)"));
    assert!(out.contains("! Error [go] omega-error v0.1.0 -> v0.1.0"));
    assert!(out.contains("- Skipped [go] skip-nometa * -> * (missing go build metadata)"));

    let err = stderr(&output);
    assert!(err.contains("$ go version -m"));
    assert!(err.contains("$ go list -m -json -versions example.com/alpha"));
}

#[test]
fn deterministic_apply_selective_path_runs_updates_only_for_eligible_unpinned_tools() {
    let sandbox = Sandbox::new(DETERMINISTIC_CONFIG, &DETERMINISTIC_TOOL_BINARIES);

    let output = sandbox.run_upnow(&["apply", "--plain", "--managers", "go", "--show-commands"]);
    assert_success(&output, "upnow apply deterministic go");

    let out = stdout(&output);
    assert!(out.contains("+ Update [go] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("+ Update [go] beta-fresh-latest v1.0.0 -> v1.0.5"));
    assert!(out.contains("~ Delayed [go] gamma-delayed v2.0.0 -> v2.1.0"));
    assert!(out.contains("- Skipped [go] pinned-pkg v3.0.0 -> v3.1.0 (pinned)"));

    let err = stderr(&output);
    assert!(err.contains("$ go install example.com/alpha/cmd/alpha-ready@v1.2.0"));
    assert!(err.contains("$ go install example.com/beta/cmd/beta-fresh-latest@v1.0.5"));
    assert!(!err.contains("$ go install example.com/pinned/cmd/pinned-pkg@v3.1.0"));
    assert!(!err.contains("$ go install example.com/gamma/cmd/gamma-delayed@v2.1.0"));
}

#[test]
fn deterministic_scan_reports_current_items_without_forcing_release_age_for_missing_time() {
    let sandbox = Sandbox::new(DETERMINISTIC_CONFIG, &DETERMINISTIC_TOOL_BINARIES);

    let output = sandbox.run_upnow(&["scan", "--plain", "--verbose", "--managers", "go"]);
    assert_success(&output, "upnow scan deterministic go");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("= Current [go] alpha-ready v1.0.0 (released:"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [go] scan-noage v5.0.0"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("- Skipped [go] skip-nometa * -> * (missing go build metadata)"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
}

#[test]
#[ignore = "requires real go + network; run via scripts/test-hybrid.sh"]
fn hybrid_apply_uses_real_module_data_with_fake_installed_state() {
    if skip_hybrid_test_if_disabled() {
        return;
    }

    let sandbox = Sandbox::new(HYBRID_CONFIG, &HYBRID_TOOL_BINARIES);
    let real_go_path = require_real_executable(sandbox.find_real_go(), "go");

    let real_go_path = path_to_string(&real_go_path);
    let output = sandbox.run_upnow_with_env(
        &["apply", "--plain", "--managers", "go", "--show-commands"],
        &[
            ("UPNOW_FAKE_GO_HYBRID", "1"),
            ("UPNOW_FAKE_GO_REAL_LIST", "1"),
            ("UPNOW_REAL_GO_BIN", &real_go_path),
        ],
    );
    assert_success(&output, "upnow apply hybrid go");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("+ Update [go] alpha-ready v1.0.0 -> v"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("- Skipped [go] pinned-pkg v1.0.0 -> v") && out.contains("(pinned)"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        !out.contains(" gamma-delayed v9999.0.0 -> v"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("! Error [go] omega-error v1.0.0 -> v1.0.0"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );

    assert!(
        err.contains("warning: real mutating commands are ENABLED")
            || err.contains("warning: apply runs with real mutating commands ENABLED"),
        "hybrid stderr:\n{err}"
    );
    assert!(err.contains("$ go install rsc.io/quote@v"));
    assert!(!err.contains("$ go install github.com/spf13/cobra/cobra@"));
    assert!(!err.contains("$ go install golang.org/x/tools/cmd/stringer@"));
    assert!(!err.contains("$ go install zzzz-upnow-no-such-module-000000000000/cmd/nope@"));
}
