use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use flate2::Compression;
use flate2::write::GzEncoder;
use upnow_cli::config::UpnowConfig;
use upnow_cli::{BatchCommand, run_batch, run_batch_with_sources};
use upnow_domain::{PackageName, UpdateSelectionMode, UpdateSelectionPolicy, VersionPolicy};
use upnow_infra::{
    Clock, CommandOutput, Env, HttpBytesResponse, HttpClient, HttpResponse, ProcessRunner,
};

fn fixtures_dir(manager: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/managers")
        .join(manager)
}

fn text(manager: &str, path: &str) -> String {
    std::fs::read_to_string(fixtures_dir(manager).join(path)).expect("fixture should be readable")
}

fn include_except<const N: usize>(packages: [&str; N]) -> UpdateSelectionPolicy {
    UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Include,
        except: packages
            .into_iter()
            .map(|package| PackageName::new(package).expect("valid package"))
            .collect(),
    }
}

#[test]
fn selected_npm_scan_routes_through_batch_core() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(
        success_status(),
        text("npm", "deterministic/installed.json"),
        "",
    ))]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Scan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["npm".to_owned()],
        &[],
    )
    .expect("npm scan should render");

    assert!(output.contains("scan npm"));
    assert!(output.contains("installed fresh-tool 2.0.0"));
    let calls = fake_calls(&process);
    assert_eq!(calls, ["npm ls -g --depth=0 --json"]);
}

#[test]
fn selected_npm_verbose_scan_renders_release_age() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{
                "dependencies": {
                    "alpha-ready": {"version": "1.0.0"}
                }
            }"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("npm", "deterministic/time/alpha-ready.json"),
            "",
        )),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Scan,
        config.clone(),
        &process,
        fixed_clock(),
        true,
        &["npm".to_owned()],
        &[],
    )
    .expect("npm verbose scan should render");

    assert!(output.contains("scan npm"));
    assert!(output.contains("installed alpha-ready 1.0.0 age"));
}

#[test]
fn selected_npm_plan_routes_through_batch_core() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            exit_status(1),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("npm", "deterministic/time/alpha-ready.json"),
            "",
        )),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["npm".to_owned()],
        &[],
    )
    .expect("npm plan should render");

    assert!(output.contains("plan npm"));
    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
    let calls = fake_calls(&process);
    assert_eq!(calls[0], "npm outdated -g --json");
    assert_eq!(calls[1], "npm view alpha-ready time --json");
}

#[test]
fn selected_npm_apply_uses_native_command_when_selection_allows_it() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            exit_status(1),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("npm", "deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Apply,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["npm".to_owned()],
        &[],
    )
    .expect("npm apply should render");

    assert!(output.contains("apply npm"));
    assert!(output.contains("applied alpha-ready 1.0.0 -> 1.2.0"));
    let calls = fake_calls(&process);
    assert_eq!(calls[2], "npm -g update alpha-ready --min-release-age 7");
}

#[test]
fn selected_npm_apply_honors_pins() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            exit_status(1),
            r#"{
                "alpha-ready": {"current": "1.0.0"},
                "pinned-pkg": {"current": "3.0.0"}
            }"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("npm", "deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("npm", "deterministic/time/pinned-pkg.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let mut config = UpnowConfig::default();
    config
        .set_manager_selection_policy("npm", include_except(["pinned-pkg"]))
        .expect("npm selection policy can be set");

    let output = run_batch(
        BatchCommand::Apply,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["npm".to_owned()],
        &[],
    )
    .expect("npm apply should render");

    assert!(output.contains("applied alpha-ready 1.0.0 -> 1.2.0"));
    assert!(!output.contains("applied pinned-pkg"));
    let calls = fake_calls(&process);
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[3], "npm -g update alpha-ready --min-release-age 7");
}

#[test]
fn selected_npm_apply_command_failure_renders_report_and_returns_failure_status() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            exit_status(1),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("npm", "deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            exit_status(1),
            "",
            "install failed",
        )),
    ]);
    let config = UpnowConfig::default();

    let err = run_batch(
        BatchCommand::Apply,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["npm".to_owned()],
        &[],
    )
    .expect_err("ordinary apply command failure should return failure status");
    let output = err.to_string();

    assert!(output.contains("apply npm"));
    assert!(output.contains(
        "failed alpha-ready 1.0.0 -> 1.2.0 (npm -g update alpha-ready --min-release-age 7):"
    ));
    assert!(output.contains("install failed"));
}

#[test]
fn multi_manager_apply_keeps_rendered_reports_when_one_command_fails() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("pnpm", "deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
        Ok(CommandOutput::from_parts(
            exit_status(1),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("npm", "deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            exit_status(1),
            "",
            "install failed",
        )),
    ]);
    let config = UpnowConfig::default();

    let err = run_batch(
        BatchCommand::Apply,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["pnpm".to_owned(), "npm".to_owned()],
        &[],
    )
    .expect_err("one failed apply command should fail the batch");
    let output = err.to_string();

    assert!(output.contains("apply pnpm"));
    assert!(output.contains("applied alpha-ready 1.0.0 -> 1.2.0"));
    assert!(output.contains("apply npm"));
    assert!(output.contains("failed alpha-ready 1.0.0 -> 1.2.0"));
    assert!(output.contains("install failed"));
    assert!(!output.contains("apply npm failed:"));
}

#[test]
fn multi_manager_plan_keeps_successful_output_when_later_manager_fails() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("pnpm", "deterministic/time/alpha-ready.json"),
            "",
        )),
        Err(upnow_infra::InfraError::HttpRequest {
            url: "npm".to_owned(),
            detail: "npm failed".to_owned(),
        }),
    ]);
    let config = UpnowConfig::default();

    let err = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["pnpm".to_owned(), "npm".to_owned()],
        &[],
    )
    .expect_err("ordinary manager failure should return failure status");
    let output = err.to_string();

    assert!(output.contains("plan pnpm"));
    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
    assert!(output.contains("plan npm failed:"));
    assert!(output.contains("npm failed"));
}

#[test]
fn selected_single_manager_failure_returns_failure_status() {
    let process = ProcessRunner::fake([Err(upnow_infra::InfraError::HttpRequest {
        url: "npm".to_owned(),
        detail: "npm failed".to_owned(),
    })]);
    let config = UpnowConfig::default();

    let err = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["npm".to_owned()],
        &[],
    )
    .expect_err("ordinary selected manager failure should return failure status");

    assert!(err.to_string().contains("plan npm failed:"));
    assert!(err.to_string().contains("npm failed"));
}

#[cfg(unix)]
#[test]
fn multi_manager_plan_interruption_remains_fatal() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("pnpm", "deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(signal_status(), "", "")),
    ]);
    let config = UpnowConfig::default();

    let err = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["pnpm".to_owned(), "npm".to_owned()],
        &[],
    )
    .expect_err("interruption should remain fatal");

    assert!(err.is_interruption());
}

#[test]
fn default_manager_selection_skips_off_managers() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("pnpm", "deterministic/time/alpha-ready.json"),
            "",
        )),
    ]);
    let mut config = UpnowConfig::default();
    config
        .apply_cli_override("brew.mode=off")
        .expect("override should apply");
    config
        .apply_cli_override("npm.mode=off")
        .expect("override should apply");
    config
        .apply_cli_override("yarn.mode=off")
        .expect("override should apply");
    config
        .apply_cli_override("bun.mode=off")
        .expect("override should apply");
    config
        .apply_cli_override("cargo.mode=off")
        .expect("override should apply");
    config
        .apply_cli_override("pipx.mode=off")
        .expect("override should apply");
    config
        .apply_cli_override("go.mode=off")
        .expect("override should apply");
    config
        .apply_cli_override("mise.mode=off")
        .expect("override should apply");
    config
        .apply_cli_override("uv.mode=off")
        .expect("override should apply");

    let output = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &[],
        &[],
    )
    .expect("default plan should render");

    assert!(output.contains("plan pnpm"));
    assert!(!output.contains("plan npm"));
    assert_eq!(fake_calls(&process).len(), 2);
}

#[test]
fn selected_manager_override_runs_manager_that_config_turns_off() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            exit_status(1),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("npm", "deterministic/time/alpha-ready.json"),
            "",
        )),
    ]);
    let mut config = UpnowConfig::default();
    config
        .apply_cli_override("npm.mode=off")
        .expect("override should apply");

    let output = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["npm".to_owned()],
        &[],
    )
    .expect("selected manager should override off mode");

    assert!(output.contains("plan npm"));
}

#[test]
fn explicit_mode_override_wins_after_selected_manager_override() {
    let process = ProcessRunner::fake([]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["npm".to_owned()],
        &["npm.mode=off".to_owned()],
    )
    .expect("explicit mode override should skip selected manager");

    assert!(output.is_empty());
}

#[test]
fn selected_managers_are_deduplicated_in_first_seen_order() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            exit_status(1),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("npm", "deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("pnpm", "deterministic/time/alpha-ready.json"),
            "",
        )),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["npm".to_owned(), "npm".to_owned(), "pnpm".to_owned()],
        &[],
    )
    .expect("deduplicated selected plan should render");

    assert!(
        output.find("plan npm").expect("npm output")
            < output.find("plan pnpm").expect("pnpm output")
    );
    assert_eq!(output.matches("plan npm").count(), 1);
    assert_eq!(output.matches("plan pnpm").count(), 1);
    assert_eq!(fake_calls(&process).len(), 4);
}

#[test]
#[allow(clippy::too_many_lines)]
fn default_manager_selection_runs_all_migrated_managers_in_registry_order() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
    let cwd = format!("{home}/.bun/install/global");
    let (http, env) = fake_release_sources([
        (
            "https://crates.test/api/v1/crates/alpha-ready",
            text("cargo", "deterministic/crates/alpha-ready.json"),
        ),
        (
            "https://pypi.test/pypi/alpha-ready/json",
            text("pipx", "deterministic/pypi/alpha-ready.json"),
        ),
    ]);
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(success_status(), "", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"formulae":[],"casks":[]}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("pnpm", "deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            exit_status(1),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("npm", "deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "1.22.22", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"type":"tree","data":{"trees":[{"name":"alpha-ready@1.0.0"}]}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("yarn", "deterministic/time/alpha-ready.jsonl"),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "/fake/bun", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"dependencies":{"alpha-ready":{"version":"1.0.0"}}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("bun", "deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0:\n    alpha-ready\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("cargo", "deterministic/search/alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"venvs":{"alpha-ready":{"metadata":{"main_package":{"package":"alpha-ready","package_version":"1.0.0"}}}}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            "/tmp/uv-tools",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0 [required: ==1.0.0]\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("uv", "deterministic/pip-plan/alpha-ready.txt"),
            "",
        )),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        config.clone(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &[],
        &[],
    )
    .expect("default batch plan should render");

    assert!(output.contains("plan brew"));
    assert!(output.contains("plan pnpm"));
    assert!(output.contains("plan npm"));
    assert!(output.contains("plan yarn"));
    assert!(output.contains("plan bun"));
    assert!(output.contains("plan cargo"));
    assert!(output.contains("plan pipx"));
    assert!(output.contains("plan go"));
    assert!(output.contains("plan mise"));
    assert!(output.contains("plan uv"));
    assert!(
        output.find("plan brew").expect("brew output")
            < output.find("plan pnpm").expect("pnpm output")
    );
    assert!(
        output.find("plan pnpm").expect("pnpm output")
            < output.find("plan npm").expect("npm output")
    );
    assert!(
        output.find("plan npm").expect("npm output")
            < output.find("plan yarn").expect("yarn output")
    );
    assert!(
        output.find("plan yarn").expect("yarn output")
            < output.find("plan bun").expect("bun output")
    );
    assert!(
        output.find("plan bun").expect("bun output")
            < output.find("plan cargo").expect("cargo output")
    );
    assert!(
        output.find("plan cargo").expect("cargo output")
            < output.find("plan pipx").expect("pipx output")
    );
    assert!(
        output.find("plan pipx").expect("pipx output") < output.find("plan go").expect("go output")
    );
    assert!(
        output.find("plan go").expect("go output") < output.find("plan mise").expect("mise output")
    );
    assert!(
        output.find("plan mise").expect("mise output") < output.find("plan uv").expect("uv output")
    );
    let calls = fake_calls(&process);
    let expected_bun_lookup = format!("/fake/bun pm view alpha-ready time --json --cwd {cwd}");
    assert!(calls.iter().any(|call| call == &expected_bun_lookup));
}

#[test]
fn selected_unknown_manager_is_rejected() {
    let process = ProcessRunner::fake([]);
    let config = UpnowConfig::default();

    let err = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["not-a-manager".to_owned()],
        &[],
    )
    .expect_err("unknown manager should be rejected");

    assert_eq!(err.to_string(), "unknown manager `not-a-manager`");
}

#[test]
fn selected_cargo_plan_routes_through_batch_core() {
    let (http, env) = fake_release_sources([(
        "https://crates.test/api/v1/crates/alpha-ready",
        text("cargo", "deterministic/crates/alpha-ready.json"),
    )]);
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0:\n    alpha-ready\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("cargo", "deterministic/search/alpha-ready.txt"),
            "",
        )),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        config.clone(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["cargo".to_owned()],
        &[],
    )
    .expect("cargo plan should render");

    assert!(output.contains("plan cargo"));
    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
    assert_eq!(
        fake_calls(&process),
        ["cargo install --list", "cargo search alpha-ready --limit 1"]
    );
}

#[test]
fn selected_cargo_plan_uses_crates_io_timeline_after_search_validation() {
    let (http, env) = fake_release_sources([(
        "https://crates.test/api/v1/crates/alpha-ready",
        text("cargo", "deterministic/crates/alpha-ready.json"),
    )]);
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0:\n    alpha-ready\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready = \"9.9.9\"    # intentionally newer than fixture metadata\n",
            "",
        )),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        config.clone(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["cargo".to_owned()],
        &[],
    )
    .expect("cargo plan should render");

    assert!(output.contains("plan cargo"));
    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
    assert!(!output.contains("9.9.9"));
}

#[test]
fn selected_cargo_plan_keeps_other_items_when_search_fails() {
    let (http, env) = fake_release_sources([(
        "https://crates.test/api/v1/crates/alpha-ready",
        text("cargo", "deterministic/crates/alpha-ready.json"),
    )]);
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0:\n    alpha-ready\nomega-error v1.0.0:\n    omega-error\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("cargo", "deterministic/search/alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            exit_status(1),
            "",
            "registry search failed",
        )),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        config.clone(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["cargo".to_owned()],
        &[],
    )
    .expect("cargo plan should keep item-level search failures in the plan");

    assert!(output.contains("plan cargo"));
    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
    assert!(output.contains("error omega-error"));
    assert!(!output.contains("plan cargo failed:"));
    assert_eq!(
        fake_calls(&process),
        [
            "cargo install --list",
            "cargo search alpha-ready --limit 1",
            "cargo search omega-error --limit 1",
        ]
    );
}

#[test]
fn selected_cargo_plan_keeps_other_items_when_crates_io_metadata_is_malformed() {
    let (http, env) = fake_release_sources([
        (
            "https://crates.test/api/v1/crates/alpha-ready",
            text("cargo", "deterministic/crates/alpha-ready.json"),
        ),
        (
            "https://crates.test/api/v1/crates/omega-error",
            "{".to_owned(),
        ),
    ]);
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0:\n    alpha-ready\nomega-error v1.0.0:\n    omega-error\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("cargo", "deterministic/search/alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("cargo", "deterministic/search/omega-error.txt"),
            "",
        )),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        config.clone(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["cargo".to_owned()],
        &[],
    )
    .expect("cargo plan should keep item-level metadata failures in the plan");

    assert!(output.contains("plan cargo"));
    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
    assert!(output.contains("blocked omega-error release lookup failed"));
    assert!(!output.contains("plan cargo failed:"));
    assert_eq!(
        fake_calls(&process),
        [
            "cargo install --list",
            "cargo search alpha-ready --limit 1",
            "cargo search omega-error --limit 1",
        ]
    );
}

#[test]
fn selected_pipx_apply_routes_through_batch_core() {
    let (http, env) = fake_release_sources([(
        "https://pypi.test/pypi/alpha-ready/json",
        text("pipx", "deterministic/pypi/alpha-ready.json"),
    )]);
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"venvs":{"alpha-ready":{"metadata":{"main_package":{"package":"alpha-ready","package_version":"1.0.0"}}}}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch_with_sources(
        BatchCommand::Apply,
        config.clone(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["pipx".to_owned()],
        &[],
    )
    .expect("pipx apply should render");

    assert!(output.contains("apply pipx"));
    assert!(output.contains("applied alpha-ready 1.0.0 -> 1.2.0"));
    assert_eq!(
        fake_calls(&process),
        ["pipx list --json", "pipx upgrade alpha-ready==1.2.0"]
    );
}

#[test]
fn selected_yarn_plan_routes_through_batch_core() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(success_status(), "1.22.22", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"type":"tree","data":{"trees":[{"name":"alpha-ready@1.0.0"}]}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("yarn", "deterministic/time/alpha-ready.jsonl"),
            "",
        )),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["yarn".to_owned()],
        &[],
    )
    .expect("yarn plan should render");

    assert!(output.contains("plan yarn"));
    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
    assert_eq!(
        fake_calls(&process),
        [
            "yarn --version",
            "yarn global list --depth=0 --json",
            "yarn info alpha-ready time --json",
        ]
    );
}

#[test]
fn selected_yarn_apply_runs_exact_global_add() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(success_status(), "1.22.22", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"type":"tree","data":{"trees":[{"name":"alpha-ready@1.0.0"}]}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("yarn", "deterministic/time/alpha-ready.jsonl"),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Apply,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["yarn".to_owned()],
        &[],
    )
    .expect("yarn apply should render");

    assert!(output.contains("apply yarn"));
    assert!(output.contains("applied alpha-ready 1.0.0 -> 1.2.0"));
    assert_eq!(fake_calls(&process)[3], "yarn global add alpha-ready@1.2.0");
}

#[test]
fn selected_yarn_scan_probe_failure_reports_discovery_issue() {
    let process = ProcessRunner::fake([Err(upnow_infra::InfraError::HttpRequest {
        url: "yarn".to_owned(),
        detail: "yarn unavailable".to_owned(),
    })]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Scan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["yarn".to_owned()],
        &[],
    )
    .expect("yarn scan should render discovery issue");

    assert!(output.contains("scan yarn"));
    assert!(output.contains("issue"));
    assert!(output.contains("yarn unavailable"));
    assert_eq!(fake_calls(&process), ["yarn --version"]);
}

#[test]
fn selected_yarn_plan_probe_failure_reports_plan_issue() {
    let process = ProcessRunner::fake([Err(upnow_infra::InfraError::HttpRequest {
        url: "yarn".to_owned(),
        detail: "yarn unavailable".to_owned(),
    })]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["yarn".to_owned()],
        &[],
    )
    .expect("yarn plan should render discovery issue");

    assert!(output.contains("plan yarn"));
    assert!(output.contains("issue"));
    assert!(output.contains("yarn unavailable"));
    assert_eq!(fake_calls(&process), ["yarn --version"]);
}

#[test]
fn selected_yarn_two_plus_scan_reports_unsupported_version() {
    let process =
        ProcessRunner::fake([Ok(CommandOutput::from_parts(success_status(), "4.3.1", ""))]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Scan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["yarn".to_owned()],
        &[],
    )
    .expect("yarn scan should render unsupported version");

    assert!(output.contains("scan yarn"));
    assert!(output.contains("unsupported manager version 4.3.1"));
    assert!(output.contains("global upgrades are not supported for Yarn 2+"));
    assert_eq!(fake_calls(&process), ["yarn --version"]);
}

#[test]
fn selected_yarn_two_plus_plan_reports_unsupported_version() {
    let process =
        ProcessRunner::fake([Ok(CommandOutput::from_parts(success_status(), "4.3.1", ""))]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["yarn".to_owned()],
        &[],
    )
    .expect("yarn plan should render unsupported version");

    assert!(output.contains("plan yarn"));
    assert!(output.contains("unsupported manager version 4.3.1"));
    assert!(!output.contains("yarn global list"));
    assert_eq!(fake_calls(&process), ["yarn --version"]);
}

#[test]
fn selected_yarn_two_plus_apply_reports_unsupported_version() {
    let process =
        ProcessRunner::fake([Ok(CommandOutput::from_parts(success_status(), "4.3.1", ""))]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Apply,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["yarn".to_owned()],
        &[],
    )
    .expect("yarn apply should render unsupported version");

    assert!(output.contains("apply yarn"));
    assert!(output.contains("unsupported manager version 4.3.1"));
    assert!(output.contains("global upgrades are not supported for Yarn 2+"));
    assert_eq!(fake_calls(&process), ["yarn --version"]);
}

#[test]
fn selected_bun_plan_routes_through_batch_core() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
    let cwd = format!("{home}/.bun/install/global");
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(success_status(), "/fake/bun", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"dependencies":{"alpha-ready":{"version":"1.0.0"}}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("bun", "deterministic/time/alpha-ready.json"),
            "",
        )),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["bun".to_owned()],
        &[],
    )
    .expect("bun plan should render");

    assert!(output.contains("plan bun"));
    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
    assert_eq!(
        fake_calls(&process),
        [
            "mise which bun",
            "/fake/bun pm ls -g --json",
            &format!("/fake/bun pm view alpha-ready time --json --cwd {cwd}"),
        ]
    );
}

#[test]
fn selected_bun_verbose_scan_missing_metadata_renders_installed_without_age() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(success_status(), "/fake/bun", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"dependencies":{"alpha-ready":{"version":"1.0.0"}}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "/fake/bun", "")),
        Ok(CommandOutput::from_parts(success_status(), "{}", "")),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Scan,
        config.clone(),
        &process,
        fixed_clock(),
        true,
        &["bun".to_owned()],
        &[],
    )
    .expect("bun verbose scan should render");

    assert!(output.contains("scan bun"));
    assert!(output.contains("installed alpha-ready 1.0.0"));
    assert!(!output.contains("skipped alpha-ready"));
    assert!(!output.contains("installed alpha-ready 1.0.0 age"));
}

#[test]
fn selected_bun_apply_uses_native_global_update_for_complete_default_selection() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(success_status(), "/fake/bun", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{
                "dependencies": {
                    "alpha-ready": {"version": "1.0.0"},
                    "beta-ready": {"version": "1.0.0"}
                }
            }"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("bun", "deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("bun", "deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "/fake/bun", "")),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Apply,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["bun".to_owned()],
        &[],
    )
    .expect("bun apply should render");

    assert!(output.contains("applied alpha-ready 1.0.0 -> 1.2.0"));
    assert!(output.contains("applied beta-ready 1.0.0 -> 1.2.0"));
    assert_eq!(
        fake_calls(&process).last().map(String::as_str),
        Some("/fake/bun update -g --minimum-release-age 604800")
    );
}

#[test]
fn selected_bun_apply_runs_exact_update_and_honors_pins() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(success_status(), "/fake/bun", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{
                "dependencies": {
                    "alpha-ready": {"version": "1.0.0"},
                    "pinned-pkg": {"version": "3.0.0"}
                }
            }"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("bun", "deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("bun", "deterministic/time/pinned-pkg.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "/fake/bun", "")),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let mut config = UpnowConfig::default();
    config
        .set_manager_selection_policy("bun", include_except(["pinned-pkg"]))
        .expect("bun selection policy can be set");

    let output = run_batch(
        BatchCommand::Apply,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["bun".to_owned()],
        &[],
    )
    .expect("bun apply should render");

    assert!(output.contains("applied alpha-ready 1.0.0 -> 1.2.0"));
    assert!(!output.contains("applied pinned-pkg"));
    assert_eq!(
        fake_calls(&process).last().map(String::as_str),
        Some("/fake/bun update -g alpha-ready@1.2.0 --minimum-release-age 604800")
    );
}

#[test]
fn set_override_affects_batch_planning_settings() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{
                "dependencies": {
                    "alpha-ready": {"version": "1.0.0"}
                }
            }"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("npm", "deterministic/time/alpha-ready.json"),
            "",
        )),
    ]);
    let config = UpnowConfig::default();

    let output = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["npm".to_owned()],
        &[format!("npm.version_policy={}", VersionPolicy::Stable)],
    )
    .expect("npm plan should render");

    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
    let calls = fake_calls(&process);
    assert_eq!(calls[0], "npm ls -g --depth=0 --json");
}

#[test]
fn selected_go_apply_routes_through_batch_core() {
    let go_bin = temp_go_bin("go-batch-apply");
    touch(go_bin.join("alpha-ready"));
    let version_metadata = text("go", "deterministic/version-m-alpha-ready.txt");
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            version_metadata.clone(),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"Versions":["v1.0.0","v1.2.0"]}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"Time":"2020-01-01T00:00:00Z"}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"Time":"2021-01-01T00:00:00Z"}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            version_metadata,
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let http = HttpClient::fake([]);
    let env = Env::fixed([("GOBIN".to_owned(), go_bin.to_string_lossy().into_owned())]);

    let output = run_batch_with_sources(
        BatchCommand::Apply,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["go".to_owned()],
        &[],
    )
    .expect("go apply should render");

    assert!(output.contains("apply go"));
    assert!(output.contains(
        "applied alpha-ready v1.0.0 -> v1.2.0 (go install example.com/alpha/cmd/alpha-ready@v1.2.0)"
    ));
    assert_eq!(
        fake_calls(&process).last().map(String::as_str),
        Some("go install example.com/alpha/cmd/alpha-ready@v1.2.0")
    );
    let _ = std::fs::remove_dir_all(go_bin);
}

#[test]
fn selected_go_verbose_scan_looks_up_only_installed_version_age() {
    let go_bin = temp_go_bin("go-verbose-scan-age");
    touch(go_bin.join("alpha-ready"));
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            text("go", "deterministic/version-m-alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("go", "deterministic/version-m-alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"Time":"2020-01-01T00:00:00Z"}"#,
            "",
        )),
    ]);
    let http = HttpClient::fake([]);
    let env = Env::fixed([("GOBIN".to_owned(), go_bin.to_string_lossy().into_owned())]);

    let output = run_batch_with_sources(
        BatchCommand::Scan,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        true,
        &["go".to_owned()],
        &[],
    )
    .expect("go verbose scan should render");

    assert!(output.contains("installed alpha-ready v1.0.0 age"));
    assert_eq!(
        fake_calls(&process),
        [
            format!("go version -m {}", go_bin.join("alpha-ready").display()),
            format!("go version -m {}", go_bin.join("alpha-ready").display()),
            "go list -m -json example.com/alpha@v1.0.0".to_owned(),
        ]
    );
    let _ = std::fs::remove_dir_all(go_bin);
}

#[test]
fn selected_go_verbose_scan_renders_installed_without_age_when_current_time_is_missing() {
    let go_bin = temp_go_bin("go-verbose-scan-noage");
    touch(go_bin.join("alpha-ready"));
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            text("go", "deterministic/version-m-alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("go", "deterministic/version-m-alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "{}", "")),
    ]);
    let http = HttpClient::fake([]);
    let env = Env::fixed([("GOBIN".to_owned(), go_bin.to_string_lossy().into_owned())]);

    let output = run_batch_with_sources(
        BatchCommand::Scan,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        true,
        &["go".to_owned()],
        &[],
    )
    .expect("go verbose scan should render");

    assert!(output.contains("installed alpha-ready v1.0.0"));
    assert!(!output.contains("skipped alpha-ready"));
    assert!(!output.contains("go list -m -json -versions"));
    let _ = std::fs::remove_dir_all(go_bin);
}

#[test]
fn selected_go_verbose_scan_reports_current_time_lookup_failure() {
    let go_bin = temp_go_bin("go-verbose-scan-failed-age");
    touch(go_bin.join("alpha-ready"));
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            text("go", "deterministic/version-m-alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("go", "deterministic/version-m-alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            exit_status(1),
            "",
            "module unavailable",
        )),
    ]);
    let http = HttpClient::fake([]);
    let env = Env::fixed([("GOBIN".to_owned(), go_bin.to_string_lossy().into_owned())]);

    let output = run_batch_with_sources(
        BatchCommand::Scan,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        true,
        &["go".to_owned()],
        &[],
    )
    .expect("go verbose scan should render");

    assert!(output.contains("skipped alpha-ready"));
    assert!(output.contains("module unavailable"));
    assert!(!output.contains("go list -m -json -versions"));
    let _ = std::fs::remove_dir_all(go_bin);
}

#[test]
fn selected_gem_plan_routes_through_batch_core() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready (1.0.0)\ndefault-skip (default: 9.9.9)",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready (1.0.0 < 1.2.0)\ndefault-skip (default: 9.9.9 < 10.0.0)",
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "3.0.0", "")),
    ]);
    let http = HttpClient::fake([(
        "https://rubygems.test/api/v1/versions/alpha-ready.json".to_owned(),
        HttpResponse {
            status: 200,
            body: text("gem", "deterministic/rubygems/alpha-ready.json"),
        },
    )]);
    let env = Env::fixed([(
        "UPNOW_GEM_RUBYGEMS_BASE_URL".to_owned(),
        "https://rubygems.test".to_owned(),
    )]);

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["gem".to_owned()],
        &[],
    )
    .expect("gem plan should render");

    assert!(output.contains("plan gem"));
    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
    assert!(!output.contains("default-skip"));
}

#[test]
fn selected_gem_plan_preserves_legacy_comparable_target_text() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready (1.0.0)",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready (1.0.0 < 1.2)",
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "3.0.0", "")),
    ]);
    let http = HttpClient::fake([(
        "https://rubygems.test/api/v1/versions/alpha-ready.json".to_owned(),
        HttpResponse {
            status: 200,
            body: r#"[
                {"number":"1.3.0","created_at":"2022-01-01T00:00:00Z"},
                {"number":"1.1.0","created_at":"2020-01-01T00:00:00Z"},
                {"number":"1.2","created_at":"2021-01-01T00:00:00Z"},
                {"number":"1.0.0","created_at":"2019-01-01T00:00:00Z"}
            ]"#
            .to_owned(),
        },
    )]);
    let env = Env::fixed([(
        "UPNOW_GEM_RUBYGEMS_BASE_URL".to_owned(),
        "https://rubygems.test".to_owned(),
    )]);

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["gem".to_owned()],
        &[],
    )
    .expect("gem plan should render");

    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2"));
    assert!(!output.contains("1.3.0"));
}

#[test]
fn selected_gem_verbose_scan_keeps_installed_row_when_age_lookup_fails() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "scan-noage (5.0.0)",
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "3.0.0", "")),
    ]);
    let http = HttpClient::fake([(
        "https://rubygems.test/api/v1/versions/scan-noage.json".to_owned(),
        HttpResponse {
            status: 200,
            body: text("gem", "deterministic/rubygems/scan-noage.json"),
        },
    )]);
    let env = Env::fixed([(
        "UPNOW_GEM_RUBYGEMS_BASE_URL".to_owned(),
        "https://rubygems.test".to_owned(),
    )]);

    let output = run_batch_with_sources(
        BatchCommand::Scan,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        true,
        &["gem".to_owned()],
        &[],
    )
    .expect("gem verbose scan should render");

    assert!(output.contains("scan gem"));
    assert!(output.contains("installed scan-noage 5.0.0"));
    assert!(!output.contains("skipped scan-noage"));
}

#[test]
fn selected_gem_apply_builds_exact_install_command() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready (1.0.0)",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready (1.0.0 < 1.2.0)",
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "3.0.0", "")),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let http = HttpClient::fake([(
        "https://rubygems.test/api/v1/versions/alpha-ready.json".to_owned(),
        HttpResponse {
            status: 200,
            body: text("gem", "deterministic/rubygems/alpha-ready.json"),
        },
    )]);
    let env = Env::fixed([(
        "UPNOW_GEM_RUBYGEMS_BASE_URL".to_owned(),
        "https://rubygems.test".to_owned(),
    )]);

    let output = run_batch_with_sources(
        BatchCommand::Apply,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["gem".to_owned()],
        &[],
    )
    .expect("gem apply should render");

    assert!(output.contains("apply gem"));
    assert!(output.contains("applied alpha-ready 1.0.0 -> 1.2.0"));
    assert_eq!(fake_calls(&process)[3], "gem install alpha-ready -v 1.2.0");
}

#[test]
fn selected_dotnet_plan_routes_through_batch_core() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(
        success_status(),
        r#"{"version":1,"data":[{"packageId":"alpha-ready","version":"1.0.0"}]}"#,
        "",
    ))]);
    let base = "https://nuget.test";
    let http = HttpClient::fake_bytes([
        (
            format!("{base}/v3/registration5-gz-semver2/alpha-ready/index.json"),
            gzipped_http_body(
                &text("dotnet", "deterministic/nuget/alpha-ready.index.json")
                    .replace("__BASE__", base),
            ),
        ),
        (
            format!("{base}/v3/registration5-gz-semver2/alpha-ready/page/1.json"),
            gzipped_http_body(&text("dotnet", "deterministic/nuget/alpha-ready.page.json")),
        ),
    ]);
    let env = Env::fixed([("UPNOW_DOTNET_NUGET_BASE_URL".to_owned(), base.to_owned())]);

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["dotnet".to_owned()],
        &[],
    )
    .expect("dotnet plan should render");

    assert!(output.contains("plan dotnet"));
    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
}

#[test]
fn selected_dotnet_apply_builds_exact_update_command() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"version":1,"data":[{"packageId":"alpha-ready","version":"1.0.0"}]}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let base = "https://nuget.test";
    let http = HttpClient::fake_bytes([
        (
            format!("{base}/v3/registration5-gz-semver2/alpha-ready/index.json"),
            gzipped_http_body(
                &text("dotnet", "deterministic/nuget/alpha-ready.index.json")
                    .replace("__BASE__", base),
            ),
        ),
        (
            format!("{base}/v3/registration5-gz-semver2/alpha-ready/page/1.json"),
            gzipped_http_body(&text("dotnet", "deterministic/nuget/alpha-ready.page.json")),
        ),
    ]);
    let env = Env::fixed([("UPNOW_DOTNET_NUGET_BASE_URL".to_owned(), base.to_owned())]);

    let output = run_batch_with_sources(
        BatchCommand::Apply,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["dotnet".to_owned()],
        &[],
    )
    .expect("dotnet apply should render");

    assert!(output.contains("apply dotnet"));
    assert!(output.contains("applied alpha-ready 1.0.0 -> 1.2.0"));
    assert_eq!(
        fake_calls(&process)[1],
        "dotnet tool update --global alpha-ready --version 1.2.0 --allow-downgrade"
    );
}

#[test]
fn selected_uv_plan_routes_through_batch_core() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "/tmp/uv-tools",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0 [required: ==1.0.0]\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("uv", "deterministic/pip-plan/alpha-ready.txt"),
            "",
        )),
    ]);
    let (http, env) = uv_release_sources([(
        "https://pypi.test/pypi/alpha-ready/json",
        text("pipx", "deterministic/pypi/alpha-ready.json"),
    )]);

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["uv".to_owned()],
        &[],
    )
    .expect("uv plan should render");

    assert!(output.contains("plan uv"));
    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
}

#[test]
fn selected_uv_apply_builds_native_selected_install_command() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "/tmp/uv-tools",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0 [required: ==1.0.0]\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("uv", "deterministic/pip-plan/alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let (http, env) = uv_release_sources([(
        "https://pypi.test/pypi/alpha-ready/json",
        text("pipx", "deterministic/pypi/alpha-ready.json"),
    )]);

    let output = run_batch_with_sources(
        BatchCommand::Apply,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["uv".to_owned()],
        &[],
    )
    .expect("uv apply should render");

    assert!(output.contains("apply uv"));
    assert!(output.contains("applied alpha-ready 1.0.0 -> 1.2.0"));
    assert_eq!(
        fake_calls(&process).last().map(String::as_str),
        Some("uv tool install --upgrade --exclude-newer 7d alpha-ready")
    );
}

#[test]
fn selected_uv_plan_keeps_dry_run_target_when_latest_is_newer() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "/tmp/uv-tools",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0 [required: ==1.0.0]\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "Would install 1 packages\n + alpha-ready==1.0.5\n",
            "",
        )),
    ]);
    let (http, env) = uv_release_sources([(
        "https://pypi.test/pypi/alpha-ready/json",
        pypi_releases_json(&[
            ("1.0.0", "2020-01-01T00:00:00Z"),
            ("1.0.5", "2020-06-01T00:00:00Z"),
            ("1.2.0", "2021-01-01T00:00:00Z"),
        ]),
    )]);

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["uv".to_owned()],
        &[],
    )
    .expect("uv plan should render");

    assert!(output.contains("update alpha-ready 1.0.0 -> 1.0.5"));
    assert!(!output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
}

#[test]
fn selected_uv_plan_does_not_invent_target_when_dry_run_selects_current() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "/tmp/uv-tools",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0 [required: ==1.0.0]\n",
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            "Would install 0 packages\n",
            "",
        )),
    ]);
    let (http, env) = uv_release_sources([(
        "https://pypi.test/pypi/alpha-ready/json",
        pypi_releases_json(&[
            ("1.0.0", "2020-01-01T00:00:00Z"),
            ("1.2.0", "2021-01-01T00:00:00Z"),
        ]),
    )]);

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["uv".to_owned()],
        &[],
    )
    .expect("uv plan should render");

    assert!(output.contains("current alpha-ready 1.0.0"));
    assert!(!output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
    assert!(!output.contains("delayed alpha-ready 1.0.0 -> 1.2.0"));
}

#[test]
fn selected_uv_plan_ignores_too_fresh_advisory_when_dry_run_selects_current() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "/tmp/uv-tools",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0 [required: ==1.0.0]\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "Would install 0 packages\n",
            "",
        )),
    ]);
    let (http, env) = uv_release_sources([(
        "https://pypi.test/pypi/alpha-ready/json",
        pypi_releases_json(&[
            ("1.0.0", "2020-01-01T00:00:00Z"),
            ("1.2.0", "2021-12-31T00:00:00Z"),
        ]),
    )]);

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["uv".to_owned()],
        &[],
    )
    .expect("uv plan should render current selected target");

    assert!(output.contains("current alpha-ready 1.0.0"));
    assert!(!output.contains("delayed alpha-ready 1.0.0 -> 1.2.0 release too fresh"));
}

#[test]
fn selected_uv_plan_blocks_missing_metadata_for_one_item() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "/tmp/uv-tools",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0 [required: ==1.0.0]\nmissing-meta v1.0.0 [required: ==1.0.0]\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("uv", "deterministic/pip-plan/alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "Would install 1 packages\n + missing-meta==1.2.0\n",
            "",
        )),
    ]);
    let (http, env) = uv_release_sources([
        (
            "https://pypi.test/pypi/alpha-ready/json",
            text("pipx", "deterministic/pypi/alpha-ready.json"),
        ),
        (
            "https://pypi.test/pypi/missing-meta/json",
            r#"{"releases":{}}"#.to_owned(),
        ),
    ]);

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["uv".to_owned()],
        &[],
    )
    .expect("uv plan should keep item-level metadata failures in the plan");

    assert!(output.contains("plan uv"));
    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
    assert!(output.contains("blocked missing-meta missing release metadata"));
    assert!(!output.contains("plan uv failed:"));
}

#[test]
fn selected_uv_apply_honors_pins_without_running_them() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "/tmp/uv-tools",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready v1.0.0 [required: ==1.0.0]\npinned-pkg v3.0.0 [required: ==3.0.0]\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("uv", "deterministic/pip-plan/alpha-ready.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("uv", "deterministic/pip-plan/pinned-pkg.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let (http, env) = uv_release_sources([
        (
            "https://pypi.test/pypi/alpha-ready/json",
            text("pipx", "deterministic/pypi/alpha-ready.json"),
        ),
        (
            "https://pypi.test/pypi/pinned-pkg/json",
            text("pipx", "deterministic/pypi/pinned-pkg.json"),
        ),
    ]);
    let mut config = UpnowConfig::default();
    config
        .set_manager_selection_policy("uv", include_except(["pinned-pkg"]))
        .expect("uv selection policy can be set");

    let output = run_batch_with_sources(
        BatchCommand::Apply,
        config,
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["uv".to_owned()],
        &[],
    )
    .expect("uv apply should render");

    assert!(output.contains("applied alpha-ready 1.0.0 -> 1.2.0"));
    assert!(!output.contains("applied pinned-pkg"));
    assert!(!output.contains("skipped pinned-pkg"));
    assert_eq!(
        fake_calls(&process).last().map(String::as_str),
        Some("uv tool install --upgrade --exclude-newer 7d alpha-ready")
    );
}

#[test]
fn selected_mise_plan_routes_through_batch_core() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "Would uninstall npm:alpha-ready@1.0.0\nWould install npm:alpha-ready@1.2.0\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"npm:alpha-ready":{"latest":"1.2.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            npm_time_json(&[("1.2.0", "2021-01-01T00:00:00Z")]),
            "",
        )),
    ]);
    let (http, env) = fake_release_sources([]);

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["mise".to_owned()],
        &[],
    )
    .expect("mise plan should render");

    assert!(output.contains("plan mise"));
    assert!(output.contains("update npm:alpha-ready 1.0.0 -> 1.2.0"));
    assert_eq!(
        fake_calls(&process),
        [
            "mise upgrade --dry-run --before 7d".to_owned(),
            "mise outdated --json".to_owned(),
            "npm view alpha-ready@1.2.0 time --json".to_owned(),
        ]
    );
}

#[test]
fn selected_mise_apply_uses_global_resolver_command_for_complete_selection() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "Would uninstall npm:alpha-ready@1.0.0\nWould install npm:alpha-ready@1.2.0\nWould uninstall npm:beta-ready@1.0.0\nWould install npm:beta-ready@1.2.0\n",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"npm:alpha-ready":{"latest":"1.2.0"},"npm:beta-ready":{"latest":"1.2.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            npm_time_json(&[("1.2.0", "2021-01-01T00:00:00Z")]),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            npm_time_json(&[("1.2.0", "2021-01-01T00:00:00Z")]),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let (http, env) = fake_release_sources([]);

    let output = run_batch_with_sources(
        BatchCommand::Apply,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["mise".to_owned()],
        &[],
    )
    .expect("mise apply should render");

    assert!(output.contains("apply mise"));
    assert!(output.contains("applied npm:alpha-ready 1.0.0 -> 1.2.0"));
    assert!(output.contains("applied npm:beta-ready 1.0.0 -> 1.2.0"));
    assert_eq!(
        fake_calls(&process).last().map(String::as_str),
        Some("mise upgrade --before 7d")
    );
}

#[test]
fn selected_mise_apply_uses_per_item_command_when_plan_contains_blocked_item() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "Would uninstall npm:alpha-ready@1.0.0\nWould install npm:alpha-ready@1.2.0\nWould uninstall npm:missing-age@1.0.0\nWould install npm:missing-age@1.2.0\n",
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "{}", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            npm_time_json(&[("1.2.0", "2021-01-01T00:00:00Z")]),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "{}", "")),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let (http, env) = fake_release_sources([]);

    let output = run_batch_with_sources(
        BatchCommand::Apply,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["mise".to_owned()],
        &[],
    )
    .expect("mise apply should execute only eligible update items");

    assert!(output.contains("apply mise"));
    assert!(output.contains("applied npm:alpha-ready 1.0.0 -> 1.2.0"));
    assert!(!output.contains("applied npm:missing-age"));
    assert_eq!(
        fake_calls(&process).last().map(String::as_str),
        Some("mise upgrade --before 7d npm:alpha-ready")
    );
}

#[test]
fn selected_mise_plan_blocks_missing_selected_target_metadata_per_item() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "Would uninstall npm:alpha-ready@1.0.0\nWould install npm:alpha-ready@1.2.0\nWould uninstall npm:missing-age@1.0.0\nWould install npm:missing-age@1.2.0\n",
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "{}", "")),
        Ok(CommandOutput::from_parts(
            success_status(),
            npm_time_json(&[("1.2.0", "2021-01-01T00:00:00Z")]),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "{}", "")),
    ]);
    let (http, env) = fake_release_sources([]);

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["mise".to_owned()],
        &[],
    )
    .expect("mise plan should keep item-level metadata failures in the plan");

    assert!(output.contains("plan mise"));
    assert!(output.contains("update npm:alpha-ready 1.0.0 -> 1.2.0"));
    assert!(output.contains("blocked npm:missing-age missing release metadata"));
    assert!(!output.contains("plan mise failed:"));
}

#[test]
fn selected_brew_plan_routes_through_batch_core() {
    let process = brew_plan_process();
    let (http, env) = fake_release_sources([]);
    let mut config = UpnowConfig::default();
    config
        .apply_cli_override("brew.no_update=true")
        .expect("brew no_update override should apply");

    let output = run_batch_with_sources(
        BatchCommand::Plan,
        config,
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["brew".to_owned()],
        &[],
    )
    .expect("brew plan should render");

    assert!(output.contains("plan brew"));
    assert!(output.contains("update alpha-ready 1.0.0 -> 1.2.0"));
    assert!(output.contains("delayed beta-fresh-latest 1.0.0 -> 1.1.0 release too fresh"));
    assert!(output.contains("update pinned-pkg 3.0.0 -> 3.1.0"));
    assert!(output.contains("blocked omega-error release lookup failed"));
    assert_eq!(
        fake_calls(&process),
        [
            "brew outdated --json=v2".to_owned(),
            "brew info --json=v2 alpha-ready beta-fresh-latest pinned-pkg omega-error".to_owned(),
            "brew tap-info --json --installed".to_owned(),
            "git -C /tmp/local-tap log -1 --format=%ct origin/main -- Formula/alpha-ready.rb"
                .to_owned(),
            "git -C /tmp/local-tap log -1 --format=%ct origin/main -- Formula/beta-fresh-latest.rb"
                .to_owned(),
            "git -C /tmp/local-tap log -1 --format=%ct origin/main -- Formula/pinned-pkg.rb"
                .to_owned(),
            "git -C /tmp/local-tap log -1 --format=%ct origin/main -- Formula/omega-error.rb"
                .to_owned(),
            "git -C /tmp/local-tap log -1 --format=%ct origin/HEAD -- Formula/omega-error.rb"
                .to_owned(),
            "git -C /tmp/local-tap log -1 --format=%ct FETCH_HEAD -- Formula/omega-error.rb"
                .to_owned(),
            "git -C /tmp/local-tap log -1 --format=%ct HEAD -- Formula/omega-error.rb".to_owned(),
        ]
    );
}

#[test]
fn selected_brew_apply_groups_formula_updates_without_indices() {
    let process = brew_apply_process();
    let (http, env) = fake_release_sources([]);
    let mut config = UpnowConfig::default();
    config
        .apply_cli_override("brew.no_update=true")
        .expect("brew no_update override should apply");
    config
        .set_manager_selection_policy("brew", include_except(["pinned-pkg"]))
        .expect("brew selection policy can be set");

    let output = run_batch_with_sources(
        BatchCommand::Apply,
        config,
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["brew".to_owned()],
        &[],
    )
    .expect("brew apply should render");

    assert!(output.contains("apply brew"));
    assert!(output.contains("applied alpha-ready 1.0.0 -> 1.2.0"));
    assert!(!output.contains("applied beta-fresh-latest"));
    assert!(!output.contains("applied pinned-pkg"));
    assert_eq!(
        fake_calls(&process).last().map(String::as_str),
        Some("brew upgrade --formula alpha-ready")
    );
}

#[test]
fn selected_brew_apply_with_policy_still_uses_native_selected_update() {
    let process = brew_apply_process();
    let (http, env) = fake_release_sources([]);
    let mut config = UpnowConfig::default();
    config
        .apply_cli_override("brew.no_update=true")
        .expect("brew no_update override should apply");
    config
        .apply_cli_override("brew.version_policy=stable")
        .expect("brew policy override should apply");
    config
        .set_manager_selection_policy("brew", include_except(["pinned-pkg"]))
        .expect("brew selection policy can be set");

    let output = run_batch_with_sources(
        BatchCommand::Apply,
        config,
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["brew".to_owned()],
        &[],
    )
    .expect("brew apply should render");

    assert!(output.contains("apply brew"));
    assert!(output.contains("applied alpha-ready 1.0.0 -> 1.2.0"));
    assert!(!output.contains("applied beta-fresh-latest"));
    assert!(!output.contains("applied pinned-pkg"));
    assert_eq!(
        fake_calls(&process).last().map(String::as_str),
        Some("brew upgrade --formula alpha-ready")
    );
}

#[test]
fn selected_brew_apply_honors_config_pins() {
    let process = brew_plan_process();
    let (http, env) = fake_release_sources([]);
    let mut config = UpnowConfig::default();
    config
        .apply_cli_override("brew.no_update=true")
        .expect("brew no_update override should apply");
    config
        .set_manager_selection_policy("brew", include_except(["alpha-ready", "pinned-pkg"]))
        .expect("brew selection policy can be set");

    let output = run_batch_with_sources(
        BatchCommand::Apply,
        config,
        &process,
        &http,
        &env,
        fixed_clock(),
        false,
        &["brew".to_owned()],
        &[],
    )
    .expect("brew apply should render no selected updates");

    assert!(output.contains("apply brew"));
    assert!(output.contains("no selected updates"));
    assert!(
        !fake_calls(&process)
            .iter()
            .any(|call| call.starts_with("brew upgrade"))
    );
}

fn brew_plan_process() -> ProcessRunner {
    ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            text("brew", "deterministic/outdated.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("brew", "deterministic/info-plan.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("brew", "deterministic/tap-info.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "1000000000",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "9999999999",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "1000000000",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "not-a-timestamp",
            "",
        )),
        Ok(CommandOutput::from_parts(exit_status(1), "", "bad ref")),
        Ok(CommandOutput::from_parts(exit_status(1), "", "bad ref")),
        Ok(CommandOutput::from_parts(exit_status(1), "", "bad ref")),
    ])
}

fn brew_apply_process() -> ProcessRunner {
    ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            text("brew", "deterministic/outdated.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("brew", "deterministic/info-plan.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("brew", "deterministic/tap-info.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "1000000000",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "9999999999",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "1000000000",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "not-a-timestamp",
            "",
        )),
        Ok(CommandOutput::from_parts(exit_status(1), "", "bad ref")),
        Ok(CommandOutput::from_parts(exit_status(1), "", "bad ref")),
        Ok(CommandOutput::from_parts(exit_status(1), "", "bad ref")),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ])
}

fn fixed_clock() -> Clock {
    Clock::fixed(SystemTime::UNIX_EPOCH + Duration::from_secs(1_640_995_200))
}

fn fake_calls(process: &ProcessRunner) -> Vec<String> {
    match process {
        ProcessRunner::Fake(fake) => fake
            .calls()
            .iter()
            .map(upnow_infra::CommandSpec::display)
            .collect(),
        ProcessRunner::Real { .. } => panic!("expected fake process"),
    }
}

fn fake_release_sources(
    responses: impl IntoIterator<Item = (&'static str, String)>,
) -> (HttpClient, Env) {
    let http = HttpClient::fake(
        responses
            .into_iter()
            .map(|(url, body)| (url.to_owned(), HttpResponse { status: 200, body })),
    );
    let env = Env::fixed([
        (
            "HOME".to_owned(),
            std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned()),
        ),
        (
            "UPNOW_CARGO_CRATES_IO_BASE_URL".to_owned(),
            "https://crates.test".to_owned(),
        ),
        (
            "UPNOW_PIPX_PYPI_BASE_URL".to_owned(),
            "https://pypi.test".to_owned(),
        ),
        (
            "UPNOW_UV_PYPI_BASE_URL".to_owned(),
            "https://pypi.test".to_owned(),
        ),
    ]);
    (http, env)
}

fn uv_release_sources(
    responses: impl IntoIterator<Item = (&'static str, String)>,
) -> (HttpClient, Env) {
    let http = HttpClient::fake(
        responses
            .into_iter()
            .map(|(url, body)| (url.to_owned(), HttpResponse { status: 200, body })),
    );
    let env = Env::fixed([
        (
            "HOME".to_owned(),
            std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned()),
        ),
        (
            "UPNOW_UV_PYPI_BASE_URL".to_owned(),
            "https://pypi.test".to_owned(),
        ),
    ]);
    (http, env)
}

fn pypi_releases_json(releases: &[(&str, &str)]) -> String {
    let entries = releases
        .iter()
        .map(|(version, timestamp)| {
            format!(r#""{version}": [{{"upload_time_iso_8601": "{timestamp}"}}]"#)
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"releases": {{{entries}}}}}"#)
}

fn npm_time_json(releases: &[(&str, &str)]) -> String {
    let entries = releases
        .iter()
        .map(|(version, timestamp)| format!(r#""{version}":"{timestamp}""#))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{entries}}}")
}

fn gzipped_http_body(body: &str) -> HttpBytesResponse {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(body.as_bytes())
        .expect("fixture should gzip");
    HttpBytesResponse {
        status: 200,
        body: encoder.finish().expect("fixture should gzip"),
    }
}

fn temp_go_bin(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("temp dir should be created");
    path
}

fn touch(path: PathBuf) {
    std::fs::write(path, "").expect("fake binary should be writable");
}

#[cfg(unix)]
fn success_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}

#[cfg(unix)]
fn exit_status(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(code << 8)
}

#[cfg(windows)]
fn success_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn exit_status(code: u32) -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(code)
}

#[cfg(unix)]
fn signal_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(2)
}
