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
  "-g update alpha-ready --min-release-age 7")
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
fn binary_interruption_exits_130() {
    let sandbox = Sandbox::new("interruption");
    sandbox.write_executable(
        "npm",
        r#"#!/bin/sh
kill -INT $$
sleep 1
"#,
    );

    let output = sandbox.run(["--manager", "npm", "plan"]);

    assert_eq!(output.status.code(), Some(130));
}

struct Sandbox {
    root: PathBuf,
    bin: PathBuf,
    config_home: PathBuf,
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
        fs::create_dir_all(&bin).expect("test bin dir should be created");
        fs::create_dir_all(&config_home).expect("test config dir should be created");
        Self {
            root,
            bin,
            config_home,
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
        Command::new(binary_path())
            .args(args)
            .env_clear()
            .env("PATH", &self.bin)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .output()
            .expect("upnow-cli binary should run")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn binary_path() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_upnow-cli"))
}
