#![allow(dead_code)]

pub mod http;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};

pub const HYBRID_OPT_IN_ENV: &str = "UPNOW_RUN_HYBRID_TESTS";

pub fn unique_temp_dir(prefix: &str) -> PathBuf {
    static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let seq = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!(
        "upnow-{prefix}-{}-{stamp}-{seq}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

pub struct SandboxEnv {
    pub root: PathBuf,
    pub home_dir: PathBuf,
    pub xdg_config_home: PathBuf,
    pub fake_bin_dir: PathBuf,
    pub path_env: String,
    pub original_path: String,
}

impl SandboxEnv {
    pub fn new(prefix: &str) -> Self {
        let root = unique_temp_dir(prefix);
        let home_dir = root.join("home");
        let xdg_config_home = root.join("xdg-config");
        let fake_bin_dir = root.join("fake-bin");

        fs::create_dir_all(&home_dir).expect("create fake HOME dir");
        fs::create_dir_all(&fake_bin_dir).expect("create fake bin dir");
        fs::create_dir_all(xdg_config_home.join("upnow")).expect("create fake XDG config dir");

        let original_path = env::var("PATH").unwrap_or_default();
        let path_env = if original_path.is_empty() {
            fake_bin_dir.display().to_string()
        } else {
            format!("{}:{}", fake_bin_dir.display(), original_path)
        };

        Self {
            root,
            home_dir,
            xdg_config_home,
            fake_bin_dir,
            path_env,
            original_path,
        }
    }

    pub fn write_config(&self, config_toml: &str) {
        fs::write(self.xdg_config_home.join("upnow/config.toml"), config_toml)
            .expect("write test config.toml");
    }

    pub fn apply_base_env(&self, cmd: &mut Command) {
        cmd.env("PATH", &self.path_env);
        cmd.env("HOME", &self.home_dir);
        cmd.env("XDG_CONFIG_HOME", &self.xdg_config_home);
    }

    pub fn find_real_executable(&self, name: &str) -> Option<PathBuf> {
        find_executable_in_path(name, &self.original_path)
    }
}

impl Drop for SandboxEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub fn scenario_path(rel: &str, label: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    assert!(
        path.is_dir(),
        "missing {label} scenario dir: {}",
        path.display()
    );
    path
}

pub fn fixture_path(parent: &Path, child: &str, label: &str) -> PathBuf {
    let path = parent.join(child);
    assert!(
        path.is_dir(),
        "missing {label} fixture dir: {}",
        path.display()
    );
    path
}

pub fn write_executable(path: &Path, content: &str, label: &str) {
    fs::write(path, content).unwrap_or_else(|err| panic!("write {label}: {err}"));

    #[cfg(unix)]
    {
        let mut perms = fs::metadata(path)
            .unwrap_or_else(|err| panic!("read {label} metadata: {err}"))
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap_or_else(|err| panic!("chmod {label}: {err}"));
    }
}

pub fn find_executable_in_path(name: &str, path_env: &str) -> Option<PathBuf> {
    env::split_paths(path_env)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(unix)]
pub fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && (meta.permissions().mode() & 0o111 != 0))
}

pub fn spawn_upnow<F>(args: &[&str], extra_env: &[(&str, &str)], configure: F) -> Output
where
    F: FnOnce(&mut Command),
{
    let bin = env::var("CARGO_BIN_EXE_upnow")
        .expect("CARGO_BIN_EXE_upnow is unavailable in this test context");

    let mut cmd = Command::new(bin);
    cmd.args(args);
    configure(&mut cmd);

    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    command_output(&mut cmd, "upnow")
}

pub fn command_output(command: &mut Command, label: &str) -> Output {
    command
        .output()
        .unwrap_or_else(|err| panic!("failed to run {label}: {err}"))
}

pub fn hybrid_tests_enabled() -> bool {
    env::var(HYBRID_OPT_IN_ENV).as_deref() == Ok("1")
}

pub fn skip_hybrid_test_if_disabled() -> bool {
    if hybrid_tests_enabled() {
        return false;
    }

    eprintln!("skipping hybrid test; set {HYBRID_OPT_IN_ENV}=1 to enable");
    true
}

pub fn require_real_executable(path: Option<PathBuf>, name: &str) -> PathBuf {
    path.unwrap_or_else(|| panic!("hybrid test requires real {name} in PATH"))
}

pub fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub fn assert_success(output: &Output, label: &str) {
    if output.status.success() {
        return;
    }

    panic!(
        "{label} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        stdout(output),
        stderr(output)
    );
}
