use std::fs;

mod common;

use common::{SandboxEnv, compact_stdout, spawn_upnow, stderr};

#[test]
fn missing_manager_command_is_skipped_without_failing_run() {
    let sandbox = SandboxEnv::new("mock-preflight");
    let empty_bin = sandbox.root.join("empty-bin");
    fs::create_dir_all(&empty_bin).expect("create empty bin dir");

    let output = spawn_upnow(&["plan", "--plain", "--managers", "npm"], &[], |cmd| {
        cmd.env("PATH", &empty_bin);
        cmd.env("HOME", &sandbox.home_dir);
        cmd.env("XDG_CONFIG_HOME", &sandbox.xdg_config_home);
    });

    let out = compact_stdout(&output);
    let err = stderr(&output);
    assert!(
        output.status.success(),
        "upnow should succeed when manager command is unavailable\nstdout:\n{out}\nstderr:\n{err}"
    );

    assert!(
        out.contains("- Skipped [npm] (required command 'npm' is not available)"),
        "expected skipped outcome for missing npm command\nstdout:\n{out}\nstderr:\n{err}"
    );
}
