use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use upnow_cli::config::UpnowConfig;
use upnow_cli::{BatchCommand, run_batch};
use upnow_domain::{PackageName, UpdateSelectionMode, UpdateSelectionPolicy, VersionPolicy};
use upnow_infra::{Clock, CommandOutput, ProcessRunner};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/managers/pnpm")
}

fn text(path: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(path)).expect("fixture should be readable")
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
fn plan_reports_update_current_delayed_and_lookup_failure_items() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"[
                {
                    "dependencies": {
                        "alpha-ready": {"version": "1.0.0"},
                        "gamma-delayed": {"version": "2.0.0"},
                        "scan-noage": {"version": "5.0.0"},
                        "omega-error": {"version": "0.1.0"}
                    }
                }
            ]"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/time/gamma-delayed.json"),
            "",
        )),
        Err(upnow_infra::InfraError::HttpRequest {
            url: "fake".to_owned(),
            detail: "lookup failed".to_owned(),
        }),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/time/scan-noage.json"),
            "",
        )),
    ]);
    let config = config_with_policy(VersionPolicy::Stable);

    let output = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["pnpm".to_owned()],
        &[],
    )
    .expect("plan should render");

    assert!(output.contains("+ Update"));
    assert!(output.contains("alpha-ready"));
    assert!(output.contains("v1.0.0"));
    assert!(output.contains("v1.2.0"));
    assert!(output.contains("~ Delayed"));
    assert!(output.contains("gamma-delayed"));
    assert!(output.contains("(no eligible release yet; latest v2.1.0 too fresh)"));
    assert!(!output.contains("scan-noage"));
    assert!(output.contains("! Error"));
    assert!(output.contains("omega-error"));
}

#[test]
fn apply_skips_pinned_packages_and_runs_exact_pnpm_command() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{
                "alpha-ready": {"current": "1.0.0"},
                "pinned-pkg": {"current": "3.0.0"}
            }"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/time/alpha-ready.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/time/pinned-pkg.json"),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "", "")),
    ]);
    let mut config = UpnowConfig::default();
    config
        .set_manager_selection_policy("pnpm", include_except(["pinned-pkg"]))
        .expect("pnpm selection policy can be set");

    let output = run_batch(
        BatchCommand::Apply,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["pnpm".to_owned()],
        &[],
    )
    .expect("apply should render");

    assert!(output.contains("+ Update"));
    assert!(output.contains("alpha-ready"));
    assert!(output.contains("v1.2.0"));
    assert!(!output.contains("pinned-pkg"));
    let calls = match &process {
        ProcessRunner::Fake(fake) => fake.calls(),
        ProcessRunner::Real { .. } => panic!("expected fake process"),
    };
    assert!(
        calls
            .iter()
            .any(|call| call.display() == "pnpm add -g alpha-ready@1.2.0")
    );
}

#[test]
fn plan_with_no_policy_uses_pnpm_outdated_instead_of_installed_list() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/time/alpha-ready.json"),
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
        &["pnpm".to_owned()],
        &[],
    )
    .expect("plan should render");

    assert!(output.contains("+ Update"));
    assert!(output.contains("alpha-ready"));
    assert!(output.contains("v1.2.0"));
    let calls = match &process {
        ProcessRunner::Fake(fake) => fake.calls(),
        ProcessRunner::Real { .. } => panic!("expected fake process"),
    };
    assert_eq!(calls[0].display(), "pnpm outdated -g --json");
}

#[test]
fn apply_honors_wildcard_pin() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/time/alpha-ready.json"),
            "",
        )),
    ]);
    let mut config = UpnowConfig::default();
    config
        .set_manager_selection_policy("pnpm", UpdateSelectionPolicy::skip_all())
        .expect("pnpm selection policy can be set");

    let output = run_batch(
        BatchCommand::Apply,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["pnpm".to_owned()],
        &[],
    )
    .expect("apply should render");

    assert!(output.contains("no selected updates"));
    assert!(!output.contains("alpha-ready"));
}

#[test]
fn verbose_scan_renders_release_age_when_available() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"[
                {
                    "dependencies": {
                        "alpha-ready": {"version": "1.0.0"}
                    }
                }
            ]"#,
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/time/alpha-ready.json"),
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
        &["pnpm".to_owned()],
        &[],
    )
    .expect("scan should render");

    assert!(output.contains("= Current"));
    assert!(output.contains("alpha-ready"));
    assert!(output.contains("v1.0.0"));
    assert!(output.contains("(released: "));
}

#[test]
fn verbose_scan_reports_release_lookup_failures_without_hiding_installed_package() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"[
                {
                    "dependencies": {
                        "alpha-ready": {"version": "1.0.0"},
                        "pinned-pkg": {"version": "3.0.0"}
                    }
                }
            ]"#,
            "",
        )),
        Err(upnow_infra::InfraError::HttpRequest {
            url: "fake".to_owned(),
            detail: "lookup failed".to_owned(),
        }),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/time/pinned-pkg.json"),
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
        &["pnpm".to_owned()],
        &[],
    )
    .expect("scan should render");

    assert!(!output.contains("issue"));
    assert!(output.contains("! Error"));
    assert!(output.contains("alpha-ready"));
    assert!(output.contains("lookup failed"));
    assert!(output.contains("pinned-pkg"));
    assert!(output.contains("v3.0.0"));
    assert!(output.contains("(released: "));
}

#[cfg(unix)]
#[test]
fn interrupted_outdated_discovery_returns_interrupted_error() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(signal_status(), "", ""))]);
    let config = UpnowConfig::default();

    let err = run_batch(
        BatchCommand::Plan,
        config.clone(),
        &process,
        fixed_clock(),
        false,
        &["pnpm".to_owned()],
        &[],
    )
    .expect_err("interrupted outdated discovery should not render a plan");

    assert!(err.is_interruption());
}

#[cfg(unix)]
#[test]
fn interrupted_release_lookup_returns_interrupted_error() {
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            r#"{"alpha-ready": {"current": "1.0.0"}}"#,
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
        &["pnpm".to_owned()],
        &[],
    )
    .expect_err("interrupted release lookup should not render a blocked item");

    assert!(err.is_interruption());
}

#[cfg(unix)]
#[test]
fn interrupted_verbose_scan_discovery_returns_interrupted_error() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(signal_status(), "", ""))]);
    let config = UpnowConfig::default();

    let err = run_batch(
        BatchCommand::Scan,
        config.clone(),
        &process,
        fixed_clock(),
        true,
        &["pnpm".to_owned()],
        &[],
    )
    .expect_err("interrupted scan discovery should not render a scan report");

    assert!(err.is_interruption());
}

fn fixed_clock() -> Clock {
    Clock::fixed(SystemTime::UNIX_EPOCH + Duration::from_secs(1_640_995_200))
}

fn config_with_policy(policy: VersionPolicy) -> UpnowConfig {
    let mut config = UpnowConfig::default();
    config
        .apply_cli_override(&format!("pnpm.version_policy={policy}"))
        .expect("policy override should apply");
    config
}

#[cfg(unix)]
fn success_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}

#[cfg(unix)]
fn signal_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(2)
}

#[cfg(windows)]
fn success_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}
