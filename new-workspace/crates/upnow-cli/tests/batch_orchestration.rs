use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use upnow_cli::config::UpnowConfig;
use upnow_cli::{BatchCommand, run_batch, run_batch_with_sources};
use upnow_domain::{PackageName, VersionPolicy};
use upnow_infra::{Clock, CommandOutput, Env, HttpClient, HttpResponse, ProcessRunner};

fn fixtures_dir(manager: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/managers")
        .join(manager)
}

fn text(manager: &str, path: &str) -> String {
    std::fs::read_to_string(fixtures_dir(manager).join(path)).expect("fixture should be readable")
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
        .set_manager_pins(
            "npm",
            BTreeSet::from([PackageName::new("pinned-pkg").expect("valid package")]),
        )
        .expect("npm pins can be set");

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

    assert!(output.contains("plan pnpm"));
    assert!(output.contains("plan npm"));
    assert!(output.contains("plan yarn"));
    assert!(output.contains("plan bun"));
    assert!(output.contains("plan cargo"));
    assert!(output.contains("plan pipx"));
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
    let calls = fake_calls(&process);
    let expected_bun_lookup = format!("/fake/bun pm view alpha-ready time --json --cwd {cwd}");
    assert!(calls.iter().any(|call| call == &expected_bun_lookup));
}

#[test]
fn selected_unknown_unmigrated_manager_is_rejected() {
    let process = ProcessRunner::fake([]);
    let config = UpnowConfig::default();

    let err = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["go".to_owned()],
        &[],
    )
    .expect_err("unmigrated manager should be rejected");

    assert_eq!(err.to_string(), "unknown manager `go`");
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
        .set_manager_pins(
            "bun",
            BTreeSet::from([PackageName::new("pinned-pkg").expect("valid package")]),
        )
        .expect("bun pins can be set");

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
    ]);
    (http, env)
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
