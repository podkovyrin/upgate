use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::AtomicBool;

mod common;

use common::http::{
    BackgroundTcpServer, read_http_request_head, run_fake_http_server, write_http_response_text,
};
use common::{
    SandboxEnv, assert_success, command_output, fixture_path, scenario_path,
    skip_hybrid_test_if_disabled, spawn_upnow, stderr, stdout, write_executable,
};

const DETERMINISTIC_SCENARIO: &str = "tests/scenarios/gem/deterministic";
const HYBRID_SCENARIO: &str = "tests/scenarios/gem/hybrid";

const DETERMINISTIC_CONFIG: &str = r#"
[gem]
mode = "apply"
min_release_age = "7d"
pinned = ["pinned-pkg"]
"#;

const HYBRID_CONFIG: &str = r#"
[gem]
mode = "apply"
min_release_age = "7d"
pinned = ["bundler"]
"#;

struct Sandbox {
    env: SandboxEnv,
    fake_gem: PathBuf,
    fake_ruby: PathBuf,
    gem_scenario_dir: PathBuf,
    rubygems_base_url: String,
    rubygems_server: Option<BackgroundTcpServer>,
}

impl Sandbox {
    fn new(scenario_rel: &str, config_toml: &str, local_rubygems: bool) -> Self {
        let sandbox_env = SandboxEnv::new("mock-gem");
        let fake_bin_dir = sandbox_env.fake_bin_dir.clone();

        sandbox_env.write_config(config_toml);

        let fake_gem = fake_bin_dir.join("gem");
        write_executable(&fake_gem, include_str!("fakes/gem.sh"), "fake gem script");

        let fake_ruby = fake_bin_dir.join("ruby");
        write_executable(
            &fake_ruby,
            include_str!("fakes/ruby.sh"),
            "fake ruby script",
        );

        let gem_scenario_dir = scenario_path(scenario_rel, "gem");

        let (rubygems_base_url, rubygems_server) = if local_rubygems {
            let rubygems_fixtures_dir = fixture_path(&gem_scenario_dir, "rubygems", "gem rubygems");
            let server =
                BackgroundTcpServer::start("fake RubyGems server", move |listener, stop_flag| {
                    run_fake_rubygems_server(
                        &listener,
                        rubygems_fixtures_dir.as_path(),
                        stop_flag.as_ref(),
                    );
                });
            (server.base_url(), Some(server))
        } else {
            // Empty value falls back to the production default URL in manager code.
            (String::new(), None)
        };

        Self {
            env: sandbox_env,
            fake_gem,
            fake_ruby,
            gem_scenario_dir,
            rubygems_base_url,
            rubygems_server,
        }
    }

    fn run_upnow(&self, args: &[&str]) -> Output {
        spawn_upnow(args, &[], |cmd| {
            self.apply_base_env(cmd);
        })
    }

    fn run_fake_gem(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.fake_gem);
        cmd.args(args);
        self.apply_base_env(&mut cmd);

        command_output(&mut cmd, "fake gem")
    }

    fn run_fake_ruby(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.fake_ruby);
        cmd.args(args);
        self.apply_base_env(&mut cmd);

        command_output(&mut cmd, "fake ruby")
    }

    fn apply_base_env(&self, cmd: &mut Command) {
        self.env.apply_base_env(cmd);
        cmd.env("UPNOW_FAKE_GEM_SCENARIO_DIR", &self.gem_scenario_dir);
        cmd.env("UPNOW_GEM_RUBYGEMS_BASE_URL", &self.rubygems_base_url);
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(mut server) = self.rubygems_server.take() {
            server.shutdown();
        }
    }
}

#[test]
fn fake_gem_harness_routes_commands_to_expected_fixtures() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG, true);

    let list = sandbox.run_fake_gem(&["list"]);
    assert_success(&list, "fake gem list");
    let list_stdout = stdout(&list);
    assert!(list_stdout.contains("alpha-ready"));
    assert!(list_stdout.contains("default-skip"));

    let outdated = sandbox.run_fake_gem(&["outdated"]);
    assert_success(&outdated, "fake gem outdated");
    let outdated_stdout = stdout(&outdated);
    assert!(outdated_stdout.contains("omega-error"));

    let ruby = sandbox.run_fake_ruby(&["-e", "print RUBY_VERSION"]);
    assert_success(&ruby, "fake ruby runtime version");
    assert_eq!(stdout(&ruby), "3.4.9");

    let unsupported = sandbox.run_fake_gem(&["list", "--local"]);
    assert_eq!(
        unsupported.status.code(),
        Some(64),
        "unsupported args should exit 64"
    );
}

#[test]
fn deterministic_plan_covers_update_delayed_pinned_and_error_states() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG, true);

    let output = sandbox.run_upnow(&[
        "plan",
        "--plain",
        "--verbose",
        "--managers",
        "gem",
        "--show-commands",
    ]);
    assert_success(&output, "upnow plan deterministic gem");

    let out = stdout(&output);
    assert!(out.contains("+ Update [gem] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("~ Delayed [gem] gamma-delayed v2.0.0 -> v2.1.0"));
    assert!(out.contains("- Skipped [gem] pinned-pkg v3.0.0 -> v3.1.0 (pinned)"));
    assert!(out.contains("! Error [gem] omega-error v0.1.0 -> v0.1.0"));
    assert!(!out.contains("default-skip"));

    let err = stderr(&output);
    assert!(err.contains("$ gem list"));
    assert!(err.contains("$ gem outdated"));
    assert!(err.contains("$ ruby -e print RUBY_VERSION"));
}

#[test]
fn deterministic_apply_selective_path_runs_updates_only_for_eligible_unpinned_items() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG, true);

    let output = sandbox.run_upnow(&[
        "apply",
        "--plain",
        "--verbose",
        "--managers",
        "gem",
        "--show-commands",
    ]);
    assert_success(&output, "upnow apply deterministic gem");

    let out = stdout(&output);
    assert!(out.contains("+ Update [gem] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("~ Delayed [gem] gamma-delayed v2.0.0 -> v2.1.0"));
    assert!(out.contains("- Skipped [gem] pinned-pkg v3.0.0 -> v3.1.0 (pinned)"));

    let err = stderr(&output);
    assert!(err.contains("$ gem install alpha-ready -v 1.2.0"));
    assert!(!err.contains("$ gem install pinned-pkg -v 3.1.0"));
    assert!(!err.contains("$ gem install gamma-delayed -v 2.1.0"));
    assert!(!err.contains("$ gem install omega-error -v 0.2.0"));
}

#[test]
fn deterministic_scan_reports_current_state_and_missing_age_fallback() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG, true);

    let output = sandbox.run_upnow(&["scan", "--plain", "--verbose", "--managers", "gem"]);
    assert_success(&output, "upnow scan deterministic gem");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("= Current [gem] alpha-ready v1.0.0 (released:"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [gem] scan-noage v5.0.0"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(!out.contains("default-skip"));
}

#[test]
#[ignore = "requires network + real RubyGems; run via scripts/test-hybrid.sh"]
fn hybrid_apply_uses_real_rubygems_data_with_fake_installed_state() {
    if skip_hybrid_test_if_disabled() {
        return;
    }

    let sandbox = Sandbox::new(HYBRID_SCENARIO, HYBRID_CONFIG, false);
    let output = sandbox.run_upnow(&[
        "apply",
        "--plain",
        "--verbose",
        "--managers",
        "gem",
        "--show-commands",
    ]);
    assert_success(&output, "upnow apply hybrid gem");

    let out = stdout(&output);
    assert!(out.contains("+ Update [gem] rake v1.0.0 -> v"));
    assert!(
        out.contains("- Skipped [gem] bundler v1.0.0 -> v") && out.contains("(pinned)"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{}",
        stderr(&output)
    );
    assert!(!out.contains(" json v9999.0.0 -> v"));
    assert!(out.contains("! Error [gem] zzzz-upnow-no-such-gem-000000000000 v1.0.0 -> v1.0.0"));

    let err = stderr(&output);
    assert!(
        err.contains("warning: real mutating commands are ENABLED")
            || err.contains("warning: apply runs with real mutating commands ENABLED"),
        "hybrid stderr:\n{err}"
    );
    assert!(err.contains("$ gem install rake -v "));
    assert!(!err.contains("$ gem install bundler -v "));
    assert!(!err.contains("$ gem install json -v "));
    assert!(!err.contains("$ gem install zzzz-upnow-no-such-gem-000000000000 -v "));
}

fn run_fake_rubygems_server(listener: &TcpListener, fixtures_dir: &Path, stop: &AtomicBool) {
    run_fake_http_server(listener, stop, |stream| {
        handle_fake_rubygems_connection(stream, fixtures_dir);
    });
}

fn handle_fake_rubygems_connection(stream: &mut TcpStream, fixtures_dir: &Path) {
    let Some(request) = read_http_request_head(stream) else {
        return;
    };
    let Some(first_line) = request.lines().next() else {
        return;
    };

    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let path_only = target.split('?').next().unwrap_or(target);

    if method != "GET" {
        write_http_response_text(
            stream,
            "405 Method Not Allowed",
            "text/plain",
            "method not allowed",
        );
        return;
    }

    let Some(gem_name) = path_only
        .strip_prefix("/api/v1/versions/")
        .and_then(|rest| rest.strip_suffix(".json"))
        .filter(|name| !name.is_empty() && !name.contains('/'))
    else {
        write_http_response_text(stream, "404 Not Found", "text/plain", "not found");
        return;
    };

    let fixture_path = fixtures_dir.join(format!("{gem_name}.json"));
    match fs::read_to_string(&fixture_path) {
        Ok(body) => write_http_response_text(stream, "200 OK", "application/json", &body),
        Err(_) => write_http_response_text(stream, "404 Not Found", "text/plain", "not found"),
    }
}
