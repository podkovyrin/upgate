//! CLI-layer behavior for the `upnow` rebuild.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

pub mod config;
pub mod registry;

use std::fmt::{self, Display};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::SystemTime;

use clap::{Parser, Subcommand};
use config::{ConfigError, UpnowConfig};
use registry::{available_manager_ids, configured_manager, ensure_known_manager};
use upnow_domain::{
    ManagerConfig, ManagerId, ManagerMode, ManagerScanEvidenceInput, ManagerScanInput, PlanIssue,
    PlanSelection, ScanIssue, ScanItem, ScanReport, UpdatePlan, UpdateSelectionPolicy,
};
use upnow_execution::progress::{
    ExecutionProgressEvent, ExecutionProgressState, ExecutionProgressSummary,
};
use upnow_execution::{
    ExecutionReport, ExecutionSelectionError, ExecutionStatus, ResolvedExecutionPlan,
    execute_commands, resolve_selection_for_execution,
};
use upnow_infra::{
    Clock, Env, HttpClient, HttpSettings, LoggingOptions, MutationMode, ProcessRunner,
    REQUIRE_MUTATION_MODE_ENV, init_logging,
};
use upnow_managers::adapter::{ManagerAdapter, ManagerAdapterError};
use upnow_planning::{PlanningSettings, default_batch_selection, update_plan_from_inputs};
use upnow_presentation::tui::{
    InteractiveManagerSelectionDraft, InteractiveSelectionOutcome, InteractiveSelectionPlan,
    InteractiveSelectionPlanningEvent, run_interactive_progress, run_interactive_selection,
    run_interactive_selection_with_planning_events,
};
use upnow_presentation::{
    BatchRenderOptions, OutcomeTable, OutputTheme, ThemeOptions, execution_report_table,
    manager_error_table, render_batch_table, scan_report_table, selection_view,
    terminal::{BatchTerminal, BatchTerminalAction, MutationNotice},
    update_plan_table,
};
use upnow_release::release_age_for_evidence;

const DEFAULT_MAX_PARALLEL_CHECKS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchCommand {
    Scan,
    Plan,
    Apply,
}

#[derive(Debug, Parser)]
#[command(name = "upnow")]
#[expect(clippy::struct_excessive_bools)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
    #[arg(
        long = "managers",
        alias = "manager",
        value_delimiter = ',',
        global = true
    )]
    managers: Vec<String>,
    #[arg(long = "set", short = 'S', global = true)]
    overrides: Vec<String>,
    #[arg(long, global = true)]
    verbose: bool,
    /// Maximum concurrent metadata checks.
    #[arg(long, default_value_t = DEFAULT_MAX_PARALLEL_CHECKS, global = true)]
    max_parallel_checks: usize,
    #[arg(long, global = true)]
    no_color: bool,
    #[arg(long, global = true)]
    plain: bool,
    /// Persist full command debug logs (stdout/stderr + timing) under the legacy log location.
    #[arg(long, global = true)]
    debug_commands: bool,
    /// Print each command to stderr before execution.
    #[arg(long, visible_alias = "print-commands", global = true)]
    show_commands: bool,
    #[arg(long, global = true)]
    interactive: bool,
    /// Debug-only: force non-mutating behavior for mutating commands.
    #[cfg(debug_assertions)]
    #[arg(long, global = true)]
    debug_no_mutate: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
enum CliCommand {
    Scan,
    Plan,
    Apply,
}

impl From<CliCommand> for BatchCommand {
    fn from(value: CliCommand) -> Self {
        match value {
            CliCommand::Scan => Self::Scan,
            CliCommand::Plan => Self::Plan,
            CliCommand::Apply => Self::Apply,
        }
    }
}

impl BatchCommand {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Plan => "plan",
            Self::Apply => "apply",
        }
    }

    const fn terminal_action(self) -> BatchTerminalAction {
        match self {
            Self::Scan => BatchTerminalAction::Scan,
            Self::Plan => BatchTerminalAction::Plan,
            Self::Apply => BatchTerminalAction::Apply,
        }
    }
}

impl Cli {
    const fn debug_no_mutate(&self) -> bool {
        #[cfg(debug_assertions)]
        {
            self.debug_no_mutate
        }

        #[cfg(not(debug_assertions))]
        {
            false
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    Config(String),
    InvalidArgs(String),
    Manager(String),
    Planning(String),
    Execution(String),
    Interrupted(String),
}

impl Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(detail)
            | Self::InvalidArgs(detail)
            | Self::Manager(detail)
            | Self::Planning(detail)
            | Self::Execution(detail)
            | Self::Interrupted(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for AppError {}

impl AppError {
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

impl From<ConfigError> for AppError {
    fn from(value: ConfigError) -> Self {
        Self::Config(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedInteractiveManagerApply {
    pub plan: UpdatePlan,
    pub manager_config: ManagerConfig,
    pub selection: PlanSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveApplyReport {
    pub progress: ExecutionProgressState,
    pub summary: ExecutionProgressSummary,
}

#[derive(Debug, Clone)]
struct PreparedInteractiveApply {
    config: UpnowConfig,
    managers: Vec<PreparedInteractiveManagerApply>,
    planning_failures: Vec<(ManagerId, String)>,
}

#[derive(Debug, Clone)]
struct PreparedInteractiveManagerApply {
    plan: UpdatePlan,
    manager_config: ManagerConfig,
}

/// Runs a batch command for the migrated managers selected by config and args.
///
/// # Errors
///
/// Returns an error for config, discovery, planning, or execution failures.
pub fn run_batch(
    command: BatchCommand,
    config: UpnowConfig,
    process: &ProcessRunner,
    clock: Clock,
    verbose: bool,
    selected_managers: &[String],
    overrides: &[String],
) -> Result<String, AppError> {
    let env = Env::real();
    let http = HttpClient::real(&HttpSettings::default_client_settings())
        .map_err(|err| AppError::Manager(err.to_string()))?;
    run_batch_with_theme_and_sources(
        command,
        config,
        process,
        &http,
        &env,
        clock,
        OutputTheme::plain(verbose),
        DEFAULT_MAX_PARALLEL_CHECKS,
        selected_managers,
        overrides,
    )
}

/// Runs interactive apply selection with real metadata sources.
///
/// This phase intentionally stops before config persistence or execution.
///
/// # Errors
///
/// Returns an error for config, discovery, planning, terminal, or selection failures.
pub fn run_interactive_apply_selection(
    config: UpnowConfig,
    process: &ProcessRunner,
    clock: Clock,
    selected_managers: &[String],
    overrides: &[String],
) -> Result<Option<Vec<(ManagerId, PlanSelection)>>, AppError> {
    let env = Env::real();
    let http = HttpClient::real(&HttpSettings::default_client_settings())
        .map_err(|err| AppError::Manager(err.to_string()))?;
    run_interactive_apply_selection_with_sources(
        config,
        process,
        &http,
        &env,
        clock,
        selected_managers,
        overrides,
    )
}

/// Runs interactive apply through selection, config persistence, execution, and progress output.
///
/// # Errors
///
/// Returns an error for config, discovery, planning, terminal, selection, persistence, or
/// interrupted execution failures.
pub fn run_interactive_apply(
    config: UpnowConfig,
    process: &ProcessRunner,
    clock: Clock,
    selected_managers: &[String],
    overrides: &[String],
) -> Result<String, AppError> {
    let env = Env::real();
    let http = HttpClient::real(&HttpSettings::default_client_settings())
        .map_err(|err| AppError::Manager(err.to_string()))?;
    run_interactive_apply_with_sources(
        config,
        process,
        &http,
        &env,
        clock,
        selected_managers,
        overrides,
    )
}

/// Runs interactive apply with explicit metadata sources.
///
/// # Errors
///
/// Returns an error for config, discovery, planning, terminal, selection, persistence, or
/// interrupted execution failures.
pub fn run_interactive_apply_with_sources(
    config: UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    selected_managers: &[String],
    overrides: &[String],
) -> Result<String, AppError> {
    let (config, manager_configs) =
        prepare_interactive_manager_configs(config, selected_managers, overrides)?;
    match run_live_confirmed_selection(config, manager_configs, process, http, env, clock)? {
        Some((config, confirmed)) => {
            execute_confirmed_interactive_apply_live(config, process, env, confirmed)?;
            Ok(String::new())
        }
        None => Ok("interactive selection cancelled\n".to_owned()),
    }
}

/// Runs a batch command with explicit release metadata sources.
///
/// # Errors
///
/// Returns an error for config, discovery, planning, or execution failures.
#[expect(clippy::too_many_arguments)]
pub fn run_batch_with_sources(
    command: BatchCommand,
    config: UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    verbose: bool,
    selected_managers: &[String],
    overrides: &[String],
) -> Result<String, AppError> {
    run_batch_with_theme_and_sources(
        command,
        config,
        process,
        http,
        env,
        clock,
        OutputTheme::plain(verbose),
        DEFAULT_MAX_PARALLEL_CHECKS,
        selected_managers,
        overrides,
    )
}

#[expect(clippy::too_many_arguments)]
fn run_batch_with_theme_and_sources(
    command: BatchCommand,
    config: UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    theme: OutputTheme,
    max_parallel_checks: usize,
    selected_managers: &[String],
    overrides: &[String],
) -> Result<String, AppError> {
    let terminal = BatchTerminal::disabled(theme);
    run_batch_with_terminal_and_sources(
        command,
        config,
        process,
        http,
        env,
        clock,
        theme,
        terminal,
        max_parallel_checks,
        selected_managers,
        overrides,
    )
}

#[expect(clippy::too_many_arguments)]
fn run_batch_with_terminal_and_sources(
    command: BatchCommand,
    mut config: UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    theme: OutputTheme,
    terminal: BatchTerminal,
    max_parallel_checks: usize,
    selected_managers: &[String],
    overrides: &[String],
) -> Result<String, AppError> {
    if !selected_managers.is_empty() {
        config.apply_selected_managers_cli_override(selected_managers)?;
    }
    for override_value in overrides {
        config.apply_cli_override(override_value)?;
    }
    let manager_ids = selected_manager_ids(selected_managers)?;
    run_batch_for_managers(
        command,
        &config,
        process,
        http,
        env,
        clock,
        theme,
        terminal,
        max_parallel_checks,
        &manager_ids,
    )
}

/// Builds interactive apply plans without executing selected updates.
///
/// # Errors
///
/// Returns an error for config, discovery, or planning failures.
pub fn build_interactive_apply_selection_plans_with_sources(
    config: UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    selected_managers: &[String],
    overrides: &[String],
) -> Result<Vec<(UpdatePlan, UpdateSelectionPolicy)>, AppError> {
    let prepared = prepare_interactive_apply_with_sources(
        config,
        process,
        http,
        env,
        clock,
        selected_managers,
        overrides,
    )?;
    Ok(prepared
        .managers
        .into_iter()
        .map(|manager| {
            let selection = manager.manager_config.selection.clone();
            (manager.plan, selection)
        })
        .collect())
}

fn prepare_interactive_apply_with_sources(
    config: UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    selected_managers: &[String],
    overrides: &[String],
) -> Result<PreparedInteractiveApply, AppError> {
    let (config, manager_configs) =
        prepare_interactive_manager_configs(config, selected_managers, overrides)?;
    let mut managers = Vec::new();
    for manager_config in manager_configs {
        let manager = configured_manager(manager_config.clone()).map_err(map_manager_error)?;
        let plan =
            build_manager_plan(manager.as_ref(), process, http, env, clock, &manager_config)?;
        managers.push(PreparedInteractiveManagerApply {
            plan,
            manager_config,
        });
    }
    Ok(PreparedInteractiveApply {
        config,
        managers,
        planning_failures: Vec::new(),
    })
}

fn prepare_interactive_manager_configs(
    mut config: UpnowConfig,
    selected_managers: &[String],
    overrides: &[String],
) -> Result<(UpnowConfig, Vec<ManagerConfig>), AppError> {
    if !selected_managers.is_empty() {
        config.apply_selected_managers_cli_override(selected_managers)?;
    }
    for override_value in overrides {
        config.apply_cli_override(override_value)?;
    }
    let manager_ids = selected_manager_ids(selected_managers)?;
    let mut manager_configs = Vec::new();
    for manager_id in manager_ids {
        ensure_known_manager(manager_id.as_str()).map_err(map_manager_error)?;
        let manager_config = config.resolve_manager(manager_id.as_str())?;
        if !manager_mode_allows_run(manager_config.mode, true) {
            continue;
        }
        manager_configs.push(manager_config);
    }
    Ok((config, manager_configs))
}

/// Runs interactive apply selection and returns the confirmed typed selection.
///
/// This phase intentionally stops before config persistence or execution.
///
/// # Errors
///
/// Returns an error for config, discovery, planning, terminal, or selection failures.
pub fn run_interactive_apply_selection_with_sources(
    config: UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    selected_managers: &[String],
    overrides: &[String],
) -> Result<Option<Vec<(ManagerId, PlanSelection)>>, AppError> {
    let plans = build_interactive_apply_selection_plans_with_sources(
        config,
        process,
        http,
        env,
        clock,
        selected_managers,
        overrides,
    )?;
    let selection_plans = plans
        .iter()
        .map(|(plan, selection_policy)| {
            InteractiveSelectionPlan::new(
                selection_view(plan, selection_policy),
                plan.issues.clone(),
                selection_policy.clone(),
            )
        })
        .collect();
    match run_interactive_selection(selection_plans)
        .map_err(|err| AppError::Planning(err.to_string()))?
    {
        InteractiveSelectionOutcome::Cancelled => Ok(None),
        InteractiveSelectionOutcome::Confirmed(drafts) => {
            if drafts.len() != plans.len() {
                return Err(AppError::Planning(format!(
                    "interactive selection count mismatch: expected {}, got {}",
                    plans.len(),
                    drafts.len()
                )));
            }
            let mut selections = Vec::new();
            for ((plan, _), draft) in plans.iter().zip(drafts) {
                if plan.manager_id != draft.manager_id {
                    return Err(AppError::Planning(format!(
                        "interactive selection manager mismatch: expected {}, got {}",
                        plan.manager_id.as_str(),
                        draft.manager_id.as_str()
                    )));
                }
                let selection =
                    PlanSelection::new(plan, draft.selected_items, draft.selection_policy)
                        .map_err(|err| AppError::Planning(err.to_string()))?;
                selections.push((plan.manager_id.clone(), selection));
            }
            Ok(Some(selections))
        }
    }
}

fn run_live_confirmed_selection(
    config: UpnowConfig,
    manager_configs: Vec<ManagerConfig>,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
) -> Result<Option<(UpnowConfig, Vec<ConfirmedInteractiveManagerApply>)>, AppError> {
    let manager_ids = manager_configs
        .iter()
        .map(|manager_config| manager_config.manager_id.clone())
        .collect::<Vec<_>>();
    let (event_tx, event_rx) = mpsc::channel();
    let (prepared_tx, prepared_rx) = mpsc::channel();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop_requested);
    let process = process.clone();
    let http = http.clone();
    let env = env.clone();
    let worker = thread::spawn(move || {
        let prepared = prepare_interactive_apply_with_events(
            config,
            manager_configs,
            &process,
            &http,
            &env,
            clock,
            &event_tx,
            &worker_stop,
        );
        if let Err(err) = &prepared {
            let _ = event_tx.send(InteractiveSelectionPlanningEvent::PlanningFailed {
                detail: err.to_string(),
            });
        }
        let _ = prepared_tx.send(prepared);
    });

    let outcome = match run_interactive_selection_with_planning_events(manager_ids, event_rx) {
        Ok(outcome) => outcome,
        Err(err) => {
            stop_requested.store(true, Ordering::Relaxed);
            return Err(AppError::Planning(err.to_string()));
        }
    };

    match outcome {
        InteractiveSelectionOutcome::Cancelled => {
            stop_requested.store(true, Ordering::Relaxed);
            Ok(None)
        }
        InteractiveSelectionOutcome::Confirmed(drafts) => {
            let prepared = prepared_rx
                .recv()
                .map_err(|err| AppError::Planning(err.to_string()))??;
            worker.join().map_err(|_| {
                AppError::Planning("interactive planning worker panicked".to_owned())
            })?;
            confirmed_from_drafts(prepared, &drafts).map(Some)
        }
    }
}

#[expect(clippy::too_many_arguments)]
fn prepare_interactive_apply_with_events(
    config: UpnowConfig,
    manager_configs: Vec<ManagerConfig>,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    event_tx: &mpsc::Sender<InteractiveSelectionPlanningEvent>,
    stop_requested: &AtomicBool,
) -> Result<PreparedInteractiveApply, AppError> {
    let mut managers = Vec::new();
    let mut planning_failures = Vec::new();
    for manager_config in manager_configs {
        if stop_requested.load(Ordering::Relaxed) {
            break;
        }
        let _ = event_tx.send(InteractiveSelectionPlanningEvent::ManagerStarted {
            manager_id: manager_config.manager_id.clone(),
        });
        let manager = match configured_manager(manager_config.clone()).map_err(map_manager_error) {
            Ok(manager) => manager,
            Err(err @ AppError::Interrupted(_)) => return Err(err),
            Err(err) => {
                let detail = err.to_string();
                let manager_id = manager_config.manager_id;
                let _ = event_tx.send(InteractiveSelectionPlanningEvent::ManagerError {
                    manager_id: manager_id.clone(),
                    detail: detail.clone(),
                });
                planning_failures.push((manager_id, detail));
                continue;
            }
        };
        match build_manager_plan(manager.as_ref(), process, http, env, clock, &manager_config) {
            Ok(plan) => {
                let selection_policy = manager_config.selection.clone();
                let view = selection_view(&plan, &selection_policy);
                let _ = event_tx.send(InteractiveSelectionPlanningEvent::ManagerReady {
                    view,
                    issues: plan.issues.clone(),
                    selection_policy,
                });
                managers.push(PreparedInteractiveManagerApply {
                    plan,
                    manager_config,
                });
            }
            Err(err @ AppError::Interrupted(_)) => return Err(err),
            Err(err) => {
                let detail = err.to_string();
                let manager_id = manager_config.manager_id;
                let _ = event_tx.send(InteractiveSelectionPlanningEvent::ManagerError {
                    manager_id: manager_id.clone(),
                    detail: detail.clone(),
                });
                planning_failures.push((manager_id, detail));
            }
        }
    }
    if !stop_requested.load(Ordering::Relaxed) {
        let _ = event_tx.send(InteractiveSelectionPlanningEvent::Finished);
    }
    Ok(PreparedInteractiveApply {
        config,
        managers,
        planning_failures,
    })
}

fn confirmed_from_drafts(
    prepared: PreparedInteractiveApply,
    drafts: &[InteractiveManagerSelectionDraft],
) -> Result<(UpnowConfig, Vec<ConfirmedInteractiveManagerApply>), AppError> {
    if !prepared.planning_failures.is_empty() {
        let details = prepared
            .planning_failures
            .iter()
            .map(|(manager_id, detail)| format!("{}: {detail}", manager_id.as_str()))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AppError::Planning(details));
    }

    let mut confirmed = Vec::new();
    for manager in prepared.managers {
        let Some(draft) = drafts
            .iter()
            .find(|draft| draft.manager_id == manager.plan.manager_id)
        else {
            return Err(AppError::Planning(format!(
                "missing interactive selection for {}",
                manager.plan.manager_id.as_str()
            )));
        };
        let selection = PlanSelection::new(
            &manager.plan,
            draft.selected_items.clone(),
            draft.selection_policy.clone(),
        )
        .map_err(|err| AppError::Planning(err.to_string()))?;
        confirmed.push(ConfirmedInteractiveManagerApply {
            plan: manager.plan,
            manager_config: manager.manager_config,
            selection,
        });
    }

    Ok((prepared.config, confirmed))
}

/// Executes confirmed interactive selections and persists selection policy to the default config.
///
/// # Errors
///
/// Returns an error for config persistence, selection resolution, or interrupted execution.
/// Manager command construction and execution failures are reported in the progress report.
pub fn execute_confirmed_interactive_apply_with_sources(
    config: UpnowConfig,
    process: &ProcessRunner,
    env: &Env,
    confirmed: Vec<ConfirmedInteractiveManagerApply>,
) -> Result<InteractiveApplyReport, AppError> {
    execute_confirmed_interactive_apply(config, process, env, confirmed, None)
}

/// Executes confirmed interactive selections and persists selection policy to a specific config.
///
/// # Errors
///
/// Returns an error for config persistence, selection resolution, or interrupted execution.
/// Manager command construction and execution failures are reported in the progress report.
pub fn execute_confirmed_interactive_apply_with_config_path(
    config: UpnowConfig,
    process: &ProcessRunner,
    env: &Env,
    confirmed: Vec<ConfirmedInteractiveManagerApply>,
    config_path: &Path,
) -> Result<InteractiveApplyReport, AppError> {
    execute_confirmed_interactive_apply(config, process, env, confirmed, Some(config_path))
}

fn execute_confirmed_interactive_apply(
    mut config: UpnowConfig,
    process: &ProcessRunner,
    env: &Env,
    confirmed: Vec<ConfirmedInteractiveManagerApply>,
    config_path: Option<&Path>,
) -> Result<InteractiveApplyReport, AppError> {
    let resolved = resolve_confirmed_execution_plans(&confirmed)?;
    let mut progress = ExecutionProgressState::from_execution_plans(resolved.clone());
    execute_confirmed_interactive_apply_resolved(
        &mut config,
        process,
        env,
        confirmed,
        &resolved,
        config_path,
        None,
        &mut |event| {
            progress.apply_event(event);
            Ok(())
        },
    )?;
    let summary = progress.summary();
    Ok(InteractiveApplyReport { progress, summary })
}

fn execute_confirmed_interactive_apply_live(
    config: UpnowConfig,
    process: &ProcessRunner,
    env: &Env,
    confirmed: Vec<ConfirmedInteractiveManagerApply>,
) -> Result<ExecutionProgressSummary, AppError> {
    let resolved = resolve_confirmed_execution_plans(&confirmed)?;
    let initial_progress = ExecutionProgressState::from_execution_plans(resolved.clone());
    let (tx, rx) = mpsc::channel();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop_requested);

    thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let mut config = config;
            let result = execute_confirmed_interactive_apply_resolved(
                &mut config,
                process,
                env,
                confirmed,
                &resolved,
                None,
                Some(&worker_stop),
                &mut |event| {
                    tx.send(event).map_err(|err| {
                        AppError::Execution(format!("progress event stream closed: {err}"))
                    })
                },
            );
            if let Err(err) = &result {
                let _ = tx.send(ExecutionProgressEvent::fatal(err.to_string()));
                let _ = tx.send(ExecutionProgressEvent::Finished);
            }
            result
        });

        let tui_result = run_interactive_progress(initial_progress, &rx, stop_requested)
            .map_err(|err| AppError::Planning(err.to_string()));
        let worker_result = worker
            .join()
            .map_err(|_| AppError::Execution("interactive apply worker panicked".to_owned()))?;

        worker_result?;
        tui_result
    })
}

fn resolve_confirmed_execution_plans(
    confirmed: &[ConfirmedInteractiveManagerApply],
) -> Result<Vec<(ManagerId, ResolvedExecutionPlan)>, AppError> {
    let mut resolved = Vec::new();
    for manager in confirmed {
        let execution_plan = resolve_selection_for_execution(
            &manager.plan,
            &manager.selection,
            configured_manager(manager.manager_config.clone())
                .map_err(map_manager_error)?
                .capabilities(),
            manager.manager_config.version_policy,
        )
        .map_err(map_execution_selection_error)?;
        resolved.push((manager.plan.manager_id.clone(), execution_plan));
    }
    Ok(resolved)
}

#[expect(clippy::too_many_arguments)]
fn execute_confirmed_interactive_apply_resolved(
    config: &mut UpnowConfig,
    process: &ProcessRunner,
    env: &Env,
    confirmed: Vec<ConfirmedInteractiveManagerApply>,
    resolved: &[(ManagerId, ResolvedExecutionPlan)],
    config_path: Option<&Path>,
    stop_requested: Option<&AtomicBool>,
    emit: &mut impl FnMut(ExecutionProgressEvent) -> Result<(), AppError>,
) -> Result<(), AppError> {
    for manager in &confirmed {
        config
            .set_manager_selection_policy(
                manager.plan.manager_id.as_str(),
                manager.selection.selection_policy.clone(),
            )
            .map_err(AppError::from)?;
        if let Some(path) = config_path {
            config
                .persist_manager_selection_policy_to_path(manager.plan.manager_id.as_str(), path)
                .map_err(AppError::from)?;
        } else {
            config
                .persist_manager_selection_policy(manager.plan.manager_id.as_str())
                .map_err(AppError::from)?;
        }
    }

    for manager in confirmed {
        if stop_requested.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
            break;
        }

        emit(ExecutionProgressEvent::manager_started(
            manager.plan.manager_id.clone(),
        ))?;

        let manager_adapter =
            configured_manager(manager.manager_config.clone()).map_err(map_manager_error)?;
        let Some((_, execution_plan)) = resolved
            .iter()
            .find(|(manager_id, _)| manager_id == &manager.plan.manager_id)
        else {
            return Err(AppError::Execution(format!(
                "missing execution plan for {}",
                manager.plan.manager_id.as_str()
            )));
        };
        let commands =
            match manager_adapter.commands_for_execution_plan(process, env, execution_plan) {
                Ok(commands) => commands,
                Err(err) if err.is_interruption() => return Err(map_manager_error(err)),
                Err(err) => {
                    emit(ExecutionProgressEvent::manager_failed(
                        manager.plan.manager_id,
                        err.to_string(),
                    ))?;
                    continue;
                }
            };
        let report = execute_commands(manager.plan.manager_id.clone(), commands, process).map_err(
            |err| {
                if err.is_interruption() {
                    AppError::Interrupted(err.to_string())
                } else {
                    AppError::Execution(err.to_string())
                }
            },
        )?;
        emit(ExecutionProgressEvent::manager_finished(report))?;

        if stop_requested.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
            break;
        }
    }

    emit(ExecutionProgressEvent::Finished)?;
    Ok(())
}

#[expect(clippy::too_many_arguments)]
fn run_batch_for_managers(
    command: BatchCommand,
    config: &UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    theme: OutputTheme,
    terminal: BatchTerminal,
    max_parallel_checks: usize,
    manager_ids: &[ManagerId],
) -> Result<String, AppError> {
    let mut table = OutcomeTable::default();
    let mut had_error = false;
    for manager_id in manager_ids {
        match run_manager_batch(
            command,
            config,
            process,
            http,
            env,
            clock,
            theme,
            terminal,
            manager_id,
            max_parallel_checks,
        ) {
            Ok(manager_output) => {
                had_error |= manager_output.failed;
                table.rows.extend(manager_output.table.rows);
            }
            Err(err) if err.is_interruption() => return Err(err),
            Err(err) => {
                had_error = true;
                table.rows.extend(
                    manager_error_table(manager_id, command.as_str(), &err.to_string()).rows,
                );
            }
        }
    }
    let output = render_batch_table(&table, theme);
    if had_error {
        Err(AppError::Manager(output))
    } else {
        Ok(output)
    }
}

struct ManagerBatchOutput {
    table: OutcomeTable,
    failed: bool,
}

#[expect(clippy::too_many_arguments)]
fn run_manager_batch(
    command: BatchCommand,
    config: &UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    theme: OutputTheme,
    terminal: BatchTerminal,
    manager_id: &ManagerId,
    max_parallel_checks: usize,
) -> Result<ManagerBatchOutput, AppError> {
    ensure_known_manager(manager_id.as_str()).map_err(map_manager_error)?;
    let manager_config = config.resolve_manager(manager_id.as_str())?;
    if !manager_mode_allows_run(manager_config.mode, command == BatchCommand::Apply) {
        return Ok(ManagerBatchOutput {
            table: OutcomeTable::default(),
            failed: false,
        });
    }

    let _spinner = terminal.start_manager_spinner(command.terminal_action(), manager_id.as_str());

    match command {
        BatchCommand::Scan if theme.verbose => {
            let manager = configured_manager(manager_config).map_err(map_manager_error)?;
            let old_age_threshold = config.scan_old_age_threshold()?;
            let options = BatchRenderOptions::new(theme).with_old_age_threshold(old_age_threshold);
            Ok(ManagerBatchOutput {
                table: scan_report_table(
                    &build_verbose_scan_report(
                        manager_id.clone(),
                        manager.as_ref(),
                        process,
                        http,
                        env,
                        clock.now(),
                        max_parallel_checks,
                    )?,
                    options,
                ),
                failed: false,
            })
        }
        BatchCommand::Scan => {
            let manager = configured_manager(manager_config).map_err(map_manager_error)?;
            Ok(ManagerBatchOutput {
                table: scan_report_table(
                    &build_scan_report(manager_id.clone(), manager.as_ref(), process, env)?,
                    BatchRenderOptions::new(theme),
                ),
                failed: false,
            })
        }
        BatchCommand::Plan => {
            let manager = configured_manager(manager_config.clone()).map_err(map_manager_error)?;
            let plan =
                build_manager_plan(manager.as_ref(), process, http, env, clock, &manager_config)?;
            Ok(ManagerBatchOutput {
                table: update_plan_table(
                    &plan,
                    BatchRenderOptions::new(theme)
                        .with_version_policy(manager_config.version_policy),
                ),
                failed: false,
            })
        }
        BatchCommand::Apply => {
            let manager = configured_manager(manager_config.clone()).map_err(map_manager_error)?;
            let plan =
                build_manager_plan(manager.as_ref(), process, http, env, clock, &manager_config)?;
            let selection = default_batch_selection(&plan, &manager_config.selection)
                .map_err(|err| AppError::Planning(err.to_string()))?;
            let execution_plan = resolve_selection_for_execution(
                &plan,
                &selection,
                manager.capabilities(),
                manager_config.version_policy,
            )
            .map_err(map_execution_selection_error)?;
            let commands = manager
                .commands_for_execution_plan(process, env, &execution_plan)
                .map_err(map_manager_error)?;
            let report =
                execute_commands(plan.manager_id.clone(), commands, process).map_err(|err| {
                    if err.is_interruption() {
                        AppError::Interrupted(err.to_string())
                    } else {
                        AppError::Execution(err.to_string())
                    }
                })?;
            Ok(ManagerBatchOutput {
                table: execution_report_table(&report, &plan.issues),
                failed: execution_report_has_failures(&report),
            })
        }
    }
}

fn build_scan_report(
    manager_id: ManagerId,
    manager: &dyn ManagerAdapter,
    process: &ProcessRunner,
    env: &Env,
) -> Result<ScanReport, AppError> {
    match manager.unsupported_manager_version(process) {
        Ok(Some(unsupported)) => {
            return Ok(ScanReport::new(
                manager_id,
                Vec::new(),
                vec![ScanIssue::UnsupportedManagerVersion {
                    installed_version: unsupported.installed_version,
                    reason: unsupported.reason,
                }],
            ));
        }
        Ok(None) => {}
        Err(err) if err.is_interruption() => return Err(map_manager_error(err)),
        Err(err) => {
            return Ok(ScanReport::new(
                manager_id,
                Vec::new(),
                vec![ScanIssue::DiscoveryFailed {
                    detail: err.to_string(),
                }],
            ));
        }
    }
    match manager.scan_inputs(process, env) {
        Ok(inputs) => Ok(ScanReport::new(
            manager_id,
            inputs.into_iter().map(scan_item_from_input).collect(),
            Vec::new(),
        )),
        Err(err) if err.is_interruption() => Err(map_manager_error(err)),
        Err(err) => Ok(ScanReport::new(
            manager_id,
            Vec::new(),
            vec![ScanIssue::DiscoveryFailed {
                detail: err.to_string(),
            }],
        )),
    }
}

fn build_verbose_scan_report(
    manager_id: ManagerId,
    manager: &dyn ManagerAdapter,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    now: SystemTime,
    max_parallel_checks: usize,
) -> Result<ScanReport, AppError> {
    match manager.unsupported_manager_version(process) {
        Ok(Some(unsupported)) => {
            return Ok(ScanReport::new(
                manager_id,
                Vec::new(),
                vec![ScanIssue::UnsupportedManagerVersion {
                    installed_version: unsupported.installed_version,
                    reason: unsupported.reason,
                }],
            ));
        }
        Ok(None) => {}
        Err(err) if err.is_interruption() => return Err(map_manager_error(err)),
        Err(err) => {
            return Ok(ScanReport::new(
                manager_id,
                Vec::new(),
                vec![ScanIssue::DiscoveryFailed {
                    detail: err.to_string(),
                }],
            ));
        }
    }
    match manager.scan_inputs_with_release_evidence(process, http, env, max_parallel_checks) {
        Ok(inputs) => Ok(ScanReport::new(
            manager_id,
            inputs
                .into_iter()
                .map(|input| scan_item_from_evidence_input(input, now))
                .collect(),
            Vec::new(),
        )),
        Err(err) if err.is_interruption() => Err(map_manager_error(err)),
        Err(err) => Ok(ScanReport::new(
            manager_id,
            Vec::new(),
            vec![ScanIssue::DiscoveryFailed {
                detail: err.to_string(),
            }],
        )),
    }
}

fn scan_item_from_evidence_input(input: ManagerScanEvidenceInput, now: SystemTime) -> ScanItem {
    match input {
        ManagerScanEvidenceInput::Installed {
            tool,
            release_evidence: Some(evidence),
        } => ScanItem::InstalledWithReleaseAge {
            tool,
            age: release_age_for_evidence(&evidence, now),
        },
        ManagerScanEvidenceInput::Installed {
            tool,
            release_evidence: None,
        } => ScanItem::Installed(tool),
        ManagerScanEvidenceInput::Skipped { installed, reason } => ScanItem::Skipped {
            tool: installed,
            reason,
        },
    }
}

fn scan_item_from_input(input: ManagerScanInput) -> ScanItem {
    match input {
        ManagerScanInput::Installed(tool) => ScanItem::Installed(tool),
        ManagerScanInput::Skipped { installed, reason } => ScanItem::Skipped {
            tool: installed,
            reason,
        },
    }
}

/// Runs from process environment and command-line arguments.
///
/// # Errors
///
/// Returns an error for invalid arguments or command execution failures.
pub fn run_from_env() -> Result<String, AppError> {
    let cli = Cli::parse();
    run_cli(&cli)
}

fn run_cli(cli: &Cli) -> Result<String, AppError> {
    let config = UpnowConfig::load()?;
    let env = Env::real();
    init_command_logging(cli, &env)?;
    let process = ProcessRunner::new(MutationMode::from_env_and_debug_no_mutate(
        &env,
        cli.debug_no_mutate(),
    ));
    let command = cli.command.unwrap_or(CliCommand::Plan);
    if cli.interactive {
        if command != CliCommand::Apply {
            return Err(AppError::InvalidArgs(
                "--interactive is only supported with apply".to_owned(),
            ));
        }
        return run_interactive_apply(
            config,
            &process,
            Clock::system(),
            &cli.managers,
            &cli.overrides,
        );
    }
    let theme = OutputTheme::from_environment(ThemeOptions {
        plain: cli.plain,
        no_color: cli.no_color,
        verbose: cli.verbose,
    });
    let terminal = BatchTerminal::from_environment(theme);
    maybe_emit_apply_mutation_mode_notice(command.into(), &process, &env, terminal)?;
    run_batch_with_terminal_and_sources(
        command.into(),
        config,
        &process,
        &HttpClient::real(&HttpSettings::default_client_settings())
            .map_err(|err| AppError::Manager(err.to_string()))?,
        &env,
        Clock::system(),
        theme,
        terminal,
        cli.max_parallel_checks,
        &cli.managers,
        &cli.overrides,
    )
}

fn init_command_logging(cli: &Cli, env: &Env) -> Result<(), AppError> {
    let options = LoggingOptions {
        debug_commands: cli.debug_commands,
        show_commands: cli.show_commands,
    };

    let path = match init_logging(options, env) {
        Ok(path) => Some(path),
        Err(err) if options.debug_commands => return Err(AppError::Execution(err.to_string())),
        Err(_) => None,
    };

    if options.debug_commands
        && !(cli.interactive && cli.command.unwrap_or(CliCommand::Plan) == CliCommand::Apply)
    {
        let path = path.expect("debug logging initialization succeeded");
        eprintln!("debug logs: {}", path.display());
    }

    Ok(())
}

fn maybe_emit_apply_mutation_mode_notice(
    command: BatchCommand,
    process: &ProcessRunner,
    env: &Env,
    terminal: BatchTerminal,
) -> Result<(), AppError> {
    if let Some(notice) = apply_mutation_mode_notice(command, process, env, terminal)? {
        eprintln!("{}", notice.render());
    }
    Ok(())
}

fn apply_mutation_mode_notice(
    command: BatchCommand,
    process: &ProcessRunner,
    env: &Env,
    terminal: BatchTerminal,
) -> Result<Option<MutationNotice>, AppError> {
    if command != BatchCommand::Apply {
        return Ok(None);
    }

    let ProcessRunner::Real { mutation_mode } = process else {
        return Ok(None);
    };

    validate_required_mutation_mode(env, *mutation_mode)?;

    if !mutation_mode_notice_enabled(env) || !terminal.notice_enabled() {
        return Ok(None);
    }

    Ok(Some(match mutation_mode {
        MutationMode::Skip => MutationNotice::Skip,
        MutationMode::Real => MutationNotice::Real,
    }))
}

fn mutation_mode_notice_enabled(env: &Env) -> bool {
    cfg!(debug_assertions) || env.non_empty_var(REQUIRE_MUTATION_MODE_ENV).is_some()
}

fn validate_required_mutation_mode(env: &Env, mutation_mode: MutationMode) -> Result<(), AppError> {
    let Some(raw) = env.non_empty_var(REQUIRE_MUTATION_MODE_ENV) else {
        return Ok(());
    };

    match raw.to_ascii_lowercase().as_str() {
        "skip" if mutation_mode == MutationMode::Skip => Ok(()),
        "real" if mutation_mode == MutationMode::Real => Ok(()),
        "skip" => Err(AppError::InvalidArgs(format!(
            "{REQUIRE_MUTATION_MODE_ENV}=skip requires effective skip mode ({})",
            skip_mode_hint()
        ))),
        "real" => Err(AppError::InvalidArgs(format!(
            "{REQUIRE_MUTATION_MODE_ENV}=real requires effective real mode ({})",
            real_mode_hint()
        ))),
        _ => Err(AppError::InvalidArgs(format!(
            "{REQUIRE_MUTATION_MODE_ENV} must be one of: skip, real (got '{raw}')"
        ))),
    }
}

fn skip_mode_hint() -> &'static str {
    if cfg!(debug_assertions) {
        "set UPNOW_SKIP_MUTATING_COMMANDS=1 or --debug-no-mutate in debug builds"
    } else {
        "set UPNOW_SKIP_MUTATING_COMMANDS=1"
    }
}

fn real_mode_hint() -> &'static str {
    if cfg!(debug_assertions) {
        "set UPNOW_SKIP_MUTATING_COMMANDS=0 and disable --debug-no-mutate"
    } else {
        "set UPNOW_SKIP_MUTATING_COMMANDS=0"
    }
}

fn build_manager_plan(
    manager: &dyn ManagerAdapter,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    manager_config: &ManagerConfig,
) -> Result<UpdatePlan, AppError> {
    match manager.unsupported_manager_version(process) {
        Ok(Some(unsupported)) => {
            return UpdatePlan::with_issues(
                manager_config.manager_id.clone(),
                Vec::new(),
                vec![PlanIssue::UnsupportedManagerVersion {
                    installed_version: unsupported.installed_version,
                    reason: unsupported.reason,
                }],
            )
            .map_err(|err| AppError::Planning(err.to_string()));
        }
        Ok(None) => {}
        Err(err) if err.is_interruption() => return Err(map_manager_error(err)),
        Err(err) => {
            return UpdatePlan::with_issues(
                manager_config.manager_id.clone(),
                Vec::new(),
                vec![PlanIssue::DiscoveryFailed {
                    detail: err.to_string(),
                }],
            )
            .map_err(|err| AppError::Planning(err.to_string()));
        }
    }
    let now = clock.now();
    let inputs = manager
        .update_inputs(process, http, env)
        .map_err(map_manager_error)?;
    update_plan_from_inputs(
        manager_config.manager_id.clone(),
        inputs,
        PlanningSettings {
            policy: manager_config.version_policy,
            now,
            min_release_age: manager_config.min_release_age,
        },
    )
    .map_err(|err| AppError::Planning(err.to_string()))
}

fn execution_report_has_failures(report: &ExecutionReport) -> bool {
    report
        .items
        .iter()
        .any(|item| matches!(item.status, ExecutionStatus::Failed { .. }))
}

const fn manager_mode_allows_run(mode: ManagerMode, is_apply: bool) -> bool {
    match mode {
        ManagerMode::Off => false,
        ManagerMode::Plan => !is_apply,
        ManagerMode::Apply => true,
    }
}

#[expect(clippy::needless_pass_by_value)]
fn map_manager_error(err: ManagerAdapterError) -> AppError {
    let detail = err.to_string();
    if err.is_interruption() {
        AppError::Interrupted(detail)
    } else {
        AppError::Manager(detail)
    }
}

#[expect(clippy::needless_pass_by_value)]
fn map_execution_selection_error(err: ExecutionSelectionError) -> AppError {
    AppError::Manager(err.to_string())
}

fn selected_manager_ids(selected_managers: &[String]) -> Result<Vec<ManagerId>, AppError> {
    if selected_managers.is_empty() {
        return Ok(available_manager_ids().collect());
    }

    let mut manager_ids = Vec::new();
    for manager_id in selected_managers {
        let manager_id = ManagerId::new(manager_id.clone())
            .map_err(|err| AppError::InvalidArgs(err.to_string()))?;
        ensure_known_manager(manager_id.as_str()).map_err(map_manager_error)?;
        if !manager_ids.contains(&manager_id) {
            manager_ids.push(manager_id);
        }
    }
    Ok(manager_ids)
}
