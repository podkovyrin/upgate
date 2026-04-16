#![allow(dead_code)]

pub(crate) mod http;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub(crate) fn unique_temp_dir(prefix: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = env::temp_dir().join(format!("upnow-{prefix}-{}-{stamp}", std::process::id()));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

pub(crate) struct SandboxEnv {
    pub(crate) root: PathBuf,
    pub(crate) home_dir: PathBuf,
    pub(crate) xdg_config_home: PathBuf,
    pub(crate) fake_bin_dir: PathBuf,
    pub(crate) path_env: String,
    pub(crate) original_path: String,
}

impl SandboxEnv {
    pub(crate) fn new(prefix: &str) -> Self {
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

    pub(crate) fn write_config(&self, config_toml: &str) {
        fs::write(self.xdg_config_home.join("upnow/config.toml"), config_toml)
            .expect("write test config.toml");
    }

    pub(crate) fn apply_base_env(&self, cmd: &mut Command) {
        cmd.env("PATH", &self.path_env);
        cmd.env("HOME", &self.home_dir);
        cmd.env("XDG_CONFIG_HOME", &self.xdg_config_home);
    }

    pub(crate) fn find_real_executable(&self, name: &str) -> Option<PathBuf> {
        find_executable_in_path(name, &self.original_path)
    }
}

impl Drop for SandboxEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(crate) fn scenario_path(rel: &str, label: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    assert!(
        path.is_dir(),
        "missing {label} scenario dir: {}",
        path.display()
    );
    path
}

pub(crate) fn fixture_path(parent: &Path, child: &str, label: &str) -> PathBuf {
    let path = parent.join(child);
    assert!(
        path.is_dir(),
        "missing {label} fixture dir: {}",
        path.display()
    );
    path
}

pub(crate) fn write_executable(path: &Path, content: &str, label: &str) {
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

pub(crate) fn find_executable_in_path(name: &str, path_env: &str) -> Option<PathBuf> {
    env::split_paths(path_env)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(unix)]
pub(crate) fn is_executable_file(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(meta) => meta.is_file() && (meta.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

pub(crate) fn spawn_upnow<F>(args: &[&str], extra_env: &[(&str, &str)], configure: F) -> Output
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

    cmd.output().expect("failed to run upnow")
}

pub(crate) fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub(crate) fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub(crate) fn assert_success(output: &Output, label: &str) {
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
