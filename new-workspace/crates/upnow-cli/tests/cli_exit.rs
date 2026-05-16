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

    let output = sandbox.run(["--manager", "npm", "apply"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr.contains("[npm]"));
    assert!(stderr.contains("! Error"));
    assert!(stderr.contains("alpha-ready"));
    assert!(stderr.contains("v1.2.0"));
    assert!(stderr.contains("install failed"));
}

#[test]
fn binary_apply_validates_required_mutation_mode_before_running() {
    let sandbox = Sandbox::new("apply-mutation-require");
    sandbox.write_executable(
        "npm",
        r#"#!/bin/sh
echo "npm should not run" >&2
exit 42
"#,
    );

    let output = sandbox.run_with_env(
        ["--manager", "npm", "apply"],
        [("UPNOW_REQUIRE_MUTATION_MODE", "skip")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr.contains("UPNOW_REQUIRE_MUTATION_MODE=skip"));
    assert!(!stderr.contains("npm should not run"));
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

    let output = sandbox.run_with_env(
        ["--manager", "npm", "apply"],
        [
            ("UPNOW_REQUIRE_MUTATION_MODE", "skip"),
            ("UPNOW_SKIP_MUTATING_COMMANDS", "1"),
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("[npm]"));
    assert!(stdout.contains("+ Update"));
    assert!(!stdout.contains("apply runs"));
}

#[test]
fn binary_plan_print_commands_alias_prints_commands_to_stderr() {
    let sandbox = Sandbox::new("print-commands");
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

    let output = sandbox.run(["--manager", "npm", "--print-commands", "plan"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success());
    assert!(stderr.contains("$ npm outdated -g --json"));
    assert!(stderr.contains("$ npm view alpha-ready time --json"));
}

#[cfg(debug_assertions)]
#[test]
fn binary_apply_debug_no_mutate_skips_mutating_command() {
    let sandbox = Sandbox::new("debug-no-mutate");
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

    let output = sandbox.run(["--manager", "npm", "--debug-no-mutate", "apply"]);

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
            "upnow-cli-exit-{name}-{}-{}",
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
        command.output().expect("upnow binary should run")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn binary_path() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_upnow")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/debug/upnow")
        })
}
