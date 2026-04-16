use flate2::Compression;
use flate2::write::GzEncoder;
use std::env;
use std::fs;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::AtomicBool;

mod common;

use common::http::{
    BackgroundTcpServer, read_http_request_head, run_fake_http_server, write_http_response_bytes,
};
use common::{
    SandboxEnv, assert_success, fixture_path, scenario_path, spawn_upnow, stderr, stdout,
    write_executable,
};

const DETERMINISTIC_SCENARIO: &str = "tests/scenarios/dotnet/deterministic";
const HYBRID_SCENARIO: &str = "tests/scenarios/dotnet/hybrid";

const DETERMINISTIC_CONFIG: &str = r#"
[dotnet]
mode = "apply"
min_release_age = "7d"
pinned = ["pinned-pkg"]
"#;

const HYBRID_CONFIG: &str = r#"
[dotnet]
mode = "apply"
min_release_age = "7d"
pinned = ["serilog"]
"#;

struct Sandbox {
    env: SandboxEnv,
    fake_dotnet: PathBuf,
    dotnet_scenario_dir: PathBuf,
    nuget_base_url: String,
    nuget_server: Option<BackgroundTcpServer>,
}

impl Sandbox {
    fn new(scenario_rel: &str, config_toml: &str, local_nuget: bool) -> Self {
        let sandbox_env = SandboxEnv::new("mock-dotnet");
        let fake_bin_dir = sandbox_env.fake_bin_dir.clone();

        sandbox_env.write_config(config_toml);

        let fake_dotnet = fake_bin_dir.join("dotnet");
        write_executable(
            &fake_dotnet,
            include_str!("fakes/dotnet.sh"),
            "fake dotnet script",
        );

        let dotnet_scenario_dir = scenario_path(scenario_rel, "dotnet");

        let (nuget_base_url, nuget_server) = if local_nuget {
            let nuget_fixtures_dir = fixture_path(&dotnet_scenario_dir, "nuget", "dotnet nuget");
            let server =
                BackgroundTcpServer::start("fake NuGet server", move |listener, stop_flag| {
                    run_fake_nuget_server(
                        &listener,
                        nuget_fixtures_dir.as_path(),
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
            fake_dotnet,
            dotnet_scenario_dir,
            nuget_base_url,
            nuget_server,
        }
    }

    fn run_upnow(&self, args: &[&str]) -> Output {
        spawn_upnow(args, &[], |cmd| {
            self.apply_base_env(cmd);
        })
    }

    fn run_fake_dotnet(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.fake_dotnet);
        cmd.args(args);
        self.apply_base_env(&mut cmd);

        cmd.output().expect("failed to run fake dotnet")
    }

    fn apply_base_env(&self, cmd: &mut Command) {
        self.env.apply_base_env(cmd);
        cmd.env("UPNOW_FAKE_DOTNET_SCENARIO_DIR", &self.dotnet_scenario_dir);
        cmd.env("UPNOW_DOTNET_NUGET_BASE_URL", &self.nuget_base_url);
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(mut server) = self.nuget_server.take() {
            server.shutdown();
        }
    }
}

#[test]
fn fake_dotnet_harness_routes_commands_to_expected_fixtures() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG, true);

    let list = sandbox.run_fake_dotnet(&["tool", "list", "--global", "--format", "json"]);
    assert_success(&list, "fake dotnet tool list");
    let list_stdout = stdout(&list);
    assert!(list_stdout.contains("alpha-ready"));
    assert!(list_stdout.contains("pinned-pkg"));

    let update = sandbox.run_fake_dotnet(&[
        "tool",
        "update",
        "--global",
        "alpha-ready",
        "--version",
        "1.2.0",
        "--allow-downgrade",
    ]);
    assert_success(&update, "fake dotnet tool update");

    let unsupported = sandbox.run_fake_dotnet(&["tool", "list"]);
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
        "dotnet",
        "--show-commands",
    ]);
    assert_success(&output, "upnow plan deterministic dotnet");

    let out = stdout(&output);
    assert!(out.contains("+ Update [dotnet] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("~ Delayed [dotnet] gamma-delayed v2.0.0 -> v2.1.0"));
    assert!(out.contains("- Skipped [dotnet] pinned-pkg v3.0.0 -> v3.0.0 (pinned)"));
    assert!(out.contains("! Error [dotnet] omega-error v0.1.0 -> v0.1.0"));

    let err = stderr(&output);
    assert!(err.contains("$ dotnet tool list --global --format json"));
}

#[test]
fn deterministic_apply_selective_path_runs_updates_only_for_eligible_unpinned_items() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG, true);

    let output = sandbox.run_upnow(&[
        "apply",
        "--plain",
        "--verbose",
        "--managers",
        "dotnet",
        "--show-commands",
    ]);
    assert_success(&output, "upnow apply deterministic dotnet");

    let out = stdout(&output);
    assert!(out.contains("+ Update [dotnet] alpha-ready v1.0.0 -> v1.2.0"));
    assert!(out.contains("~ Delayed [dotnet] gamma-delayed v2.0.0 -> v2.1.0"));
    assert!(out.contains("- Skipped [dotnet] pinned-pkg v3.0.0 -> v3.0.0 (pinned)"));

    let err = stderr(&output);
    assert!(
        err.contains("$ dotnet tool update --global alpha-ready --version 1.2.0 --allow-downgrade")
    );
    assert!(
        !err.contains("$ dotnet tool update --global pinned-pkg --version 3.1.0 --allow-downgrade")
    );
    assert!(
        !err.contains(
            "$ dotnet tool update --global gamma-delayed --version 2.1.0 --allow-downgrade"
        )
    );
    assert!(
        !err.contains(
            "$ dotnet tool update --global omega-error --version 0.2.0 --allow-downgrade"
        )
    );
}

#[test]
fn deterministic_scan_reports_current_state_and_missing_age_fallback() {
    let sandbox = Sandbox::new(DETERMINISTIC_SCENARIO, DETERMINISTIC_CONFIG, true);

    let output = sandbox.run_upnow(&["scan", "--plain", "--verbose", "--managers", "dotnet"]);
    assert_success(&output, "upnow scan deterministic dotnet");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("= Current [dotnet] beta-fresh-latest v1.1.0 (released:"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
    assert!(
        out.contains("= Current [dotnet] scan-noage v5.0.0 (source: nuget)"),
        "scan stdout:\n{out}\nscan stderr:\n{err}"
    );
}

#[test]
#[ignore = "requires network + real NuGet; run via scripts/test-hybrid.sh"]
fn hybrid_apply_uses_real_nuget_data_with_fake_installed_state() {
    if env::var("UPNOW_RUN_HYBRID_TESTS").as_deref() != Ok("1") {
        eprintln!("skipping hybrid test; set UPNOW_RUN_HYBRID_TESTS=1 to enable");
        return;
    }

    let sandbox = Sandbox::new(HYBRID_SCENARIO, HYBRID_CONFIG, false);
    let output = sandbox.run_upnow(&[
        "apply",
        "--plain",
        "--verbose",
        "--managers",
        "dotnet",
        "--show-commands",
    ]);
    assert_success(&output, "upnow apply hybrid dotnet");

    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("+ Update [dotnet] nuget.versioning v1.0.0 -> v"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("- Skipped [dotnet] serilog v1.0.0 -> v1.0.0 (pinned)"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("~ Delayed [dotnet] nuget.frameworks v9999.0.0 -> v"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );
    assert!(
        out.contains("! Error [dotnet] zzzz-upnow-no-such-nuget-000000000000 v1.0.0 -> v1.0.0"),
        "hybrid stdout:\n{out}\nhybrid stderr:\n{err}"
    );

    assert!(
        err.contains("warning: real mutating commands are ENABLED")
            || err.contains("warning: apply runs with real mutating commands ENABLED"),
        "hybrid stderr:\n{err}"
    );
    assert!(err.contains("$ dotnet tool update --global nuget.versioning --version "));
    assert!(!err.contains("$ dotnet tool update --global serilog --version "));
    assert!(!err.contains("$ dotnet tool update --global nuget.frameworks --version "));
    assert!(!err.contains(
        "$ dotnet tool update --global zzzz-upnow-no-such-nuget-000000000000 --version "
    ));
}

fn run_fake_nuget_server(listener: &TcpListener, fixtures_dir: &Path, stop: &AtomicBool) {
    run_fake_http_server(listener, stop, |stream| {
        handle_fake_nuget_connection(stream, fixtures_dir);
    });
}

fn handle_fake_nuget_connection(stream: &mut TcpStream, fixtures_dir: &Path) {
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
        write_http_response_bytes(
            stream,
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed",
        );
        return;
    }

    let base_url = local_base_url(stream);
    let Some((compression_kind, package_name, doc_kind)) = parse_nuget_path(path_only) else {
        write_http_response_bytes(stream, "404 Not Found", "text/plain", b"not found");
        return;
    };

    let fixture_name = format!("{package_name}.{doc_kind}.json");
    let fixture_path = fixtures_dir.join(fixture_name);

    let Ok(raw_body) = fs::read_to_string(&fixture_path) else {
        write_http_response_bytes(stream, "404 Not Found", "text/plain", b"not found");
        return;
    };

    let body = raw_body.replace("__BASE__", &base_url);

    if compression_kind == "registration5-gz-semver2" {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        if encoder.write_all(body.as_bytes()).is_err() {
            write_http_response_bytes(
                stream,
                "500 Internal Server Error",
                "text/plain",
                b"gzip error",
            );
            return;
        }

        match encoder.finish() {
            Ok(payload) => {
                write_http_response_bytes(stream, "200 OK", "application/json", &payload);
            }
            Err(_) => write_http_response_bytes(
                stream,
                "500 Internal Server Error",
                "text/plain",
                b"gzip error",
            ),
        }
    } else {
        write_http_response_bytes(stream, "200 OK", "application/json", body.as_bytes());
    }
}

fn local_base_url(stream: &TcpStream) -> String {
    let addr = stream.local_addr().expect("local addr available");
    format!("http://{addr}")
}

fn parse_nuget_path(path: &str) -> Option<(&str, &str, &str)> {
    let trimmed = path.trim_matches('/');
    let mut parts = trimmed.split('/');

    let v3 = parts.next()?;
    let kind = parts.next()?;
    let pkg = parts.next()?;
    let tail = parts.next()?;

    if v3 != "v3" {
        return None;
    }

    if kind != "registration5-gz-semver2" && kind != "registration5-semver1" {
        return None;
    }

    if tail == "index.json" && parts.next().is_none() {
        return Some((kind, pkg, "index"));
    }

    if tail == "page" {
        let page_file = parts.next()?;
        if Path::new(page_file)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
            && parts.next().is_none()
        {
            return Some((kind, pkg, "page"));
        }
    }

    None
}
