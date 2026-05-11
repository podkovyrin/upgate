use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use upnow_cli::build_interactive_apply_selection_plans_with_sources;
use upnow_cli::config::UpnowConfig;
use upnow_domain::{PlanSelection, SelectedTarget};
use upnow_infra::{Clock, CommandOutput, Env, HttpClient, ProcessRunner};
use upnow_planning::selection_view;
use upnow_presentation::tui::{InteractiveSelectionPlan, InteractiveSelectionScreen};

fn fixtures_dir(manager: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/managers")
        .join(manager)
}

fn text(manager: &str, path: &str) -> String {
    std::fs::read_to_string(fixtures_dir(manager).join(path)).expect("fixture should be readable")
}

#[test]
fn interactive_selection_builds_typed_plan_without_executing_updates() {
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
    let http = HttpClient::fake([]);
    let env = Env::fixed([]);

    let plans = build_interactive_apply_selection_plans_with_sources(
        UpnowConfig::default(),
        &process,
        &http,
        &env,
        fixed_clock(),
        &["npm".to_owned()],
        &[],
    )
    .expect("interactive selection planning should succeed");
    let selection_plans = plans
        .iter()
        .map(|(plan, policy)| {
            InteractiveSelectionPlan::new(
                selection_view(plan, policy),
                plan.issues.clone(),
                policy.clone(),
            )
        })
        .collect();
    let drafts = InteractiveSelectionScreen::new(selection_plans).selection_drafts();
    let selection = PlanSelection::new(
        &plans[0].0,
        drafts[0].selected_items.clone(),
        drafts[0].selection_policy.clone(),
    )
    .expect("app boundary should validate selection");

    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].manager_id.as_str(), "npm");
    assert_eq!(selection.selected_items.len(), 1);
    assert_eq!(
        selection.selected_items[0].target,
        SelectedTarget::Recommended
    );
    assert_eq!(
        fake_calls(&process),
        ["npm outdated -g --json", "npm view alpha-ready time --json"]
    );
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
