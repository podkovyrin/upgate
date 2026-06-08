#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn binary_plan_success_exits_zero() {
    let sandbox = Sandbox::new("plan-success");
    sandbox.write_executable(
        "npm",
        r#"#!/bin/sh
case "$*" in
  "outdated -g --json")
    printf '%s\n' '{"alpha-ready":{"current":"1.0.0"}}'
    ;;
  "view alpha-ready time --json")
    printf '%s\n' '{"1.0.0":"2021-01-01T00:00:00.000Z","1.2.0":"2021-12-01T00:00:00.000Z"}'
    ;;
  *)
    echo "unexpected npm command: $*" >&2
    exit 42
    ;;
esac
"#,
    );

    let output = sandbox.run(["--manager", "npm", "plan"]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[npm]"));
    assert!(stdout.contains("+ Update"));
}

#[test]
fn binary_apply_command_failure_exits_one() {
    let sandbox = Sandbox::new("apply-failure");
    sandbox.write_executable(
        "npm",
        r#"#!/bin/sh
case "$*" in
  "outdated -g --json")
    printf '%s\n' '{"alpha-ready":{"current":"1.0.0"}}'
    ;;
  "view alpha-ready time --json")
    printf '%s\n' '{"1.0.0":"2021-01-01T00:00:00.000Z","1.2.0":"2021-12-01T00:00:00.000Z"}'
    ;;
  "-g update --min-release-age 7")
    echo "install failed" >&2
    exit 1
    ;;
  *)
    echo "unexpected npm command: $*" >&2
    exit 42
    ;;
esac
"#,
    );

    let output = sandbox.run(["--manager", "npm", "--yolo", "apply"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr.contains("[npm]"));
    assert!(stderr.contains("! Error"));
    assert!(stderr.contains("alpha-ready"));
    assert!(stderr.contains("v1.2.0"));
    assert!(stderr.contains("install failed"));
}

#[test]
fn binary_apply_notice_gate_does_not_pollute_stdout_when_piped() {
    let sandbox = Sandbox::new("apply-notice-stdout");
    sandbox.write_executable(
        "npm",
        r#"#!/bin/sh
case "$*" in
  "outdated -g --json")
    printf '%s\n' '{"alpha-ready":{"current":"1.0.0"}}'
    ;;
  "view alpha-ready time --json")
    printf '%s\n' '{"1.0.0":"2021-01-01T00:00:00.000Z","1.2.0":"2021-12-01T00:00:00.000Z"}'
    ;;
  *)
    echo "unexpected npm command: $*" >&2
    exit 42
    ;;
esac
"#,
    );

    let output = sandbox.run(["--manager", "npm", "--dry-run", "--no-approval", "apply"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("[npm]"));
    assert!(stdout.contains("+ Update"));
    assert!(!stdout.contains("apply runs"));
}

#[test]
fn binary_yolo_is_apply_only() {
    let sandbox = Sandbox::new("yolo-apply-only");

    let output = sandbox.run(["--yolo", "plan"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr.contains("--yolo is only supported with apply"));
}

#[test]
fn binary_plan_trace_commands_prints_commands_to_stderr() {
    let sandbox = Sandbox::new("trace-commands");
    sandbox.write_executable(
        "npm",
        r#"#!/bin/sh
case "$*" in
  "outdated -g --json")
    printf '%s\n' '{"alpha-ready":{"current":"1.0.0"}}'
    ;;
  "view alpha-ready time --json")
    printf '%s\n' '{"1.0.0":"2021-01-01T00:00:00.000Z","1.2.0":"2021-12-01T00:00:00.000Z"}'
    ;;
  *)
    echo "unexpected npm command: $*" >&2
    exit 42
    ;;
esac
"#,
    );

    let output = sandbox.run(["--manager", "npm", "--trace-commands", "plan"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success());
    assert!(stderr.contains("$ npm outdated -g --json"));
    assert!(stderr.contains("$ npm view alpha-ready time --json"));
}

#[test]
fn binary_apply_dry_run_skips_mutating_command() {
    let sandbox = Sandbox::new("dry-run");
    let mutation_marker = sandbox.root.join("mutation-ran");
    sandbox.write_executable(
        "npm",
        &format!(
            r#"#!/bin/sh
case "$*" in
  "outdated -g --json")
    printf '%s\n' '{{"alpha-ready":{{"current":"1.0.0"}}}}'
    ;;
  "view alpha-ready time --json")
    printf '%s\n' '{{"1.0.0":"2021-01-01T00:00:00.000Z","1.2.0":"2021-12-01T00:00:00.000Z"}}'
    ;;
  "-g update --min-release-age 7")
    printf changed > {}
    ;;
  *)
    echo "unexpected npm command: $*" >&2
    exit 42
    ;;
esac
"#,
            mutation_marker.display()
        ),
    );

    let output = sandbox.run(["--manager", "npm", "--dry-run", "--yolo", "apply"]);

    assert!(output.status.success());
    assert!(!mutation_marker.exists());
}

#[test]
fn binary_interruption_exits_130() {
    let sandbox = Sandbox::new("interruption");
    sandbox.write_executable(
        "npm",
        r"#!/bin/sh
kill -INT $$
sleep 1
",
    );

    let output = sandbox.run(["--manager", "npm", "plan"]);

    assert_eq!(output.status.code(), Some(130));
}

struct Sandbox {
    root: PathBuf,
    bin: PathBuf,
    config_home: PathBuf,
    state_home: PathBuf,
    home: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "upgate-cli-exit-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after epoch")
                .as_nanos()
        ));
        let bin = root.join("bin");
        let config_home = root.join("config");
        let state_home = root.join("state");
        let home = root.join("home");
        fs::create_dir_all(&bin).expect("test bin dir should be created");
        fs::create_dir_all(&config_home).expect("test config dir should be created");
        fs::create_dir_all(&state_home).expect("test state dir should be created");
        fs::create_dir_all(&home).expect("test home dir should be created");
        Self {
            root,
            bin,
            config_home,
            state_home,
            home,
        }
    }

    fn write_executable(&self, name: &str, content: &str) {
        let path = self.bin.join(name);
        fs::write(&path, content).expect("fake manager should be written");
        let mut permissions = fs::metadata(&path)
            .expect("fake manager metadata should be readable")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("fake manager should be executable");
    }

    fn run<const N: usize>(&self, args: [&str; N]) -> std::process::Output {
        self.run_with_env(args, [])
    }

    fn run_with_env<const N: usize, const M: usize>(
        &self,
        args: [&str; N],
        envs: [(&str, &str); M],
    ) -> std::process::Output {
        let mut command = Command::new(binary_path());
        command
            .args(args)
            .env_clear()
            .env("PATH", &self.bin)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("HOME", &self.home);
        for (key, value) in envs {
            command.env(key, value);
        }
        command.output().expect("upgate binary should run")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn binary_path() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_upgate").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/upgate"),
        PathBuf::from,
    )
}
