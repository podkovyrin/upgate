//! CLI-layer behavior for the `upnow` rebuild.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

pub mod config;
pub mod registry;

use std::fmt::{self, Display};
use std::fs;
use std::io::IsTerminal;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::SystemTime;

use clap::{Parser, Subcommand};
use config::{ConfigError, UpnowConfig};
use registry::{
    available_manager_ids, configured_manager, ensure_known_manager, required_executable,
};
use serde::Serialize;
use upnow_domain::{
    ManagerConfig, ManagerId, ManagerMode, ManagerScanEvidenceInput, ManagerScanInput, PlanItem,
    PlanSelection, ScanIssue, ScanItem, ScanReport, SelectedUpdate, UpdatePlan,
    UpdateSelectionPolicy, VersionPolicy,
};
use upnow_execution::progress::{
    ExecutionProgressEvent, ExecutionProgressState, ExecutionProgressSummary,
};
use upnow_execution::{
    ExecutionReport, ExecutionStatus, ResolvedExecutionPlan, execute_commands,
    resolve_selection_for_execution,
};
use upnow_infra::{
    Clock, Env, HttpClient, HttpSettings, LoggingOptions, MutationMode, ProcessRunner,
    REQUIRE_MUTATION_MODE_ENV, command_exists_in_env, init_logging, run_ordered_parallel,
    run_ordered_parallel_stoppable,
};
use upnow_managers::adapter::{ManagerAdapter, ManagerAdapterError};
use upnow_planning::{PlanningSettings, default_batch_selection, update_plan_from_inputs};
use upnow_presentation::tui::{
    InteractiveManagerSelectionDraft, InteractiveSelectionOutcome, InteractiveSelectionPlan,
    InteractiveSelectionPlanningEvent, run_interactive_progress, run_interactive_selection,
    run_interactive_selection_with_planning_events,
};
use upnow_presentation::{
    BatchRenderOptions, OutcomeTable, OutputTheme, ThemeOptions, apply_execution_report_table,
    manager_error_table, render_batch_table, scan_report_table, selection_view,
    terminal::{BatchTerminal, BatchTerminalAction, MutationNotice},
    update_plan_table,
};
use upnow_release::release_age_for_evidence;

const DEFAULT_MAX_PARALLEL_CHECKS_PER_MANAGER: usize = 6;
const APPLY_SNAPSHOT_FILE: &str = "snapshot.json";

#[derive(Clone)]
struct InteractiveCommandLog {
    enabled: bool,
    entries: Arc<Mutex<Vec<String>>>,
}

impl InteractiveCommandLog {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn disabled() -> Self {
        Self::new(false)
    }

    const fn enabled(&self) -> bool {
        self.enabled
    }

    fn snapshot(&self) -> Vec<String> {
        self.entries.lock().map_or_else(
            |poisoned| poisoned.into_inner().clone(),
            |entries| entries.clone(),
        )
    }

    fn record(&self, command: String) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(command);
    }

    fn process_for_selection(
        &self,
        process: &ProcessRunner,
        event_tx: mpsc::Sender<InteractiveSelectionPlanningEvent>,
    ) -> ProcessRunner {
        if !self.enabled {
            return process.clone();
        }

        let command_log = self.clone();
        process.clone().with_command_start_listener(move |event| {
            let command = event.command_display;
            command_log.record(command.clone());
            let _ = event_tx.send(InteractiveSelectionPlanningEvent::CommandStarted { command });
        })
    }

    fn process_for_progress(
        &self,
        process: &ProcessRunner,
        tx: mpsc::Sender<ExecutionProgressEvent>,
    ) -> ProcessRunner {
        if !self.enabled {
            return process.clone();
        }

        let command_log = self.clone();
        process.clone().with_command_start_listener(move |event| {
            let command = event.command_display;
            command_log.record(command.clone());
            let _ = tx.send(ExecutionProgressEvent::command_started(command));
        })
    }
}

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
    /// Maximum concurrent metadata checks per manager.
    #[arg(long, default_value_t = DEFAULT_MAX_PARALLEL_CHECKS_PER_MANAGER, global = true)]
    max_parallel_checks_per_manager: usize,
    /// Maximum managers to scan or plan concurrently.
    #[arg(long, global = true)]
    manager_concurrency: Option<NonZeroUsize>,
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
    /// Apply without the interactive selection UI.
    #[arg(long, visible_aliases = ["yes", "no-approval"], global = true)]
    yolo: bool,
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

impl Display for BatchCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Scan => "scan",
            Self::Plan => "plan",
            Self::Apply => "apply",
        })
    }
}

impl BatchCommand {
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

#[derive(Debug, Serialize)]
struct ApplySnapshotRow<'a> {
    manager: &'a str,
    tool_name: &'a str,
    current: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    action: &'static str,
}

fn write_apply_snapshot_for_selections<'a>(
    selections: impl IntoIterator<Item = (&'a UpdatePlan, &'a PlanSelection)>,
    log_dir: &Path,
) -> Result<(), AppError> {
    let rows = selections
        .into_iter()
        .flat_map(|(plan, selection)| snapshot_rows(plan, selection))
        .collect::<Vec<_>>();
    let path = log_dir.join(APPLY_SNAPSHOT_FILE);
    let bytes = serde_json::to_vec_pretty(&rows).map_err(|err| {
        AppError::Execution(format!(
            "failed to serialize apply snapshot {}: {err}",
            path.display()
        ))
    })?;
    fs::write(&path, bytes).map_err(|err| {
        AppError::Execution(format!(
            "failed to write apply snapshot {}: {err}",
            path.display()
        ))
    })
}

fn snapshot_rows<'a>(
    plan: &'a UpdatePlan,
    selection: &'a PlanSelection,
) -> Vec<ApplySnapshotRow<'a>> {
    plan.items
        .iter()
        .map(|item| {
            let selected_update = selection
                .selected_items
                .iter()
                .find(|selected| selected.plan_item_id == *item.id())
                .map(|selected| &selected.selected_update);
            snapshot_row(plan, item, selected_update)
        })
        .collect()
}

fn snapshot_row<'a>(
    plan: &'a UpdatePlan,
    item: &'a PlanItem,
    selected_update: Option<&'a SelectedUpdate>,
) -> ApplySnapshotRow<'a> {
    ApplySnapshotRow {
        manager: plan.manager_id.as_str(),
        tool_name: snapshot_tool_name(item),
        current: snapshot_current_version(item),
        target: snapshot_target(item, selected_update),
        action: snapshot_action(item, selected_update),
    }
}

const fn snapshot_action(
    item: &PlanItem,
    selected_update: Option<&SelectedUpdate>,
) -> &'static str {
    if selected_update.is_some() {
        return "update";
    }

    match item {
        PlanItem::Update { .. } | PlanItem::Skipped { .. } => "skipped",
        PlanItem::Current { .. } => "current",
        PlanItem::Delayed { .. } => "delayed",
        PlanItem::Blocked { .. } => "blocked",
        PlanItem::ResolverError { .. } => "error",
    }
}

fn snapshot_tool_name(item: &PlanItem) -> &str {
    match item {
        PlanItem::Update { candidate, .. } | PlanItem::Delayed { candidate, .. } => {
            candidate.package_name.as_str()
        }
        PlanItem::Current { installed, .. }
        | PlanItem::Skipped { installed, .. }
        | PlanItem::ResolverError { installed, .. } => installed.tool_name.as_str(),
        PlanItem::Blocked { seed, .. } => seed.installed.tool_name.as_str(),
    }
}

fn snapshot_current_version(item: &PlanItem) -> &str {
    match item {
        PlanItem::Update { candidate, .. } | PlanItem::Delayed { candidate, .. } => {
            candidate.installed_version.as_str()
        }
        PlanItem::Current { installed, .. }
        | PlanItem::Skipped { installed, .. }
        | PlanItem::ResolverError { installed, .. } => installed.installed_version.as_str(),
        PlanItem::Blocked { seed, .. } => seed.installed.installed_version.as_str(),
    }
}

fn snapshot_target(item: &PlanItem, selected_update: Option<&SelectedUpdate>) -> Option<String> {
    if let Some(selected_update) = selected_update {
        return match selected_update {
            SelectedUpdate::Exact { target_version } => Some(target_version.to_string()),
            SelectedUpdate::ManagerResolved => None,
            SelectedUpdate::Recommended | SelectedUpdate::ForcePlannedCandidate => {
                snapshot_plan_target(item)
            }
        };
    }

    snapshot_plan_target(item)
}

fn snapshot_plan_target(item: &PlanItem) -> Option<String> {
    match item {
        PlanItem::Update { candidate, .. } | PlanItem::Delayed { candidate, .. } => {
            candidate.target_version().map(ToString::to_string)
        }
        PlanItem::Blocked { seed, .. } => seed
            .target_selection
            .target_version()
            .map(ToString::to_string),
        PlanItem::Current { .. } | PlanItem::Skipped { .. } | PlanItem::ResolverError { .. } => {
            None
        }
    }
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
        DEFAULT_MAX_PARALLEL_CHECKS_PER_MANAGER,
        selected_managers,
        overrides,
        None,
        None,
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
    run_interactive_apply_with_sources_and_manager_concurrency_override(
        config,
        process,
        http,
        env,
        clock,
        selected_managers,
        overrides,
        DEFAULT_MAX_PARALLEL_CHECKS_PER_MANAGER,
        None,
        None,
    )
}

#[expect(clippy::too_many_arguments)]
fn run_interactive_apply_with_sources_and_manager_concurrency_override(
    config: UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    selected_managers: &[String],
    overrides: &[String],
    max_parallel_checks_per_manager: usize,
    manager_concurrency_override: Option<usize>,
    snapshot_log_dir: Option<&Path>,
) -> Result<String, AppError> {
    let command_log = InteractiveCommandLog::disabled();
    run_interactive_apply_with_sources_and_options(
        config,
        process,
        http,
        env,
        clock,
        selected_managers,
        overrides,
        max_parallel_checks_per_manager,
        manager_concurrency_override,
        &command_log,
        snapshot_log_dir,
    )
}

#[expect(clippy::too_many_arguments)]
fn run_interactive_apply_with_sources_and_options(
    config: UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    selected_managers: &[String],
    overrides: &[String],
    max_parallel_checks_per_manager: usize,
    manager_concurrency_override: Option<usize>,
    command_log: &InteractiveCommandLog,
    snapshot_log_dir: Option<&Path>,
) -> Result<String, AppError> {
    let (mut config, manager_configs) =
        prepare_interactive_manager_configs(config, selected_managers, overrides)?;
    let manager_configs = available_manager_configs(manager_configs, env)?;
    if manager_configs.is_empty() {
        return Ok(String::new());
    }
    if let Some(manager_concurrency) = manager_concurrency_override {
        config.set_manager_concurrency(manager_concurrency)?;
    }
    let manager_concurrency = config.manager_concurrency()?;
    match run_live_confirmed_selection(
        config,
        manager_configs,
        process,
        http,
        env,
        clock,
        max_parallel_checks_per_manager,
        manager_concurrency,
        command_log,
    )? {
        Some((config, confirmed)) => {
            if let Some(log_dir) = snapshot_log_dir {
                write_apply_snapshot_for_selections(
                    confirmed
                        .iter()
                        .map(|manager| (&manager.plan, &manager.selection)),
                    log_dir,
                )?;
            }
            execute_confirmed_interactive_apply_live(config, process, env, confirmed, command_log)?;
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
        DEFAULT_MAX_PARALLEL_CHECKS_PER_MANAGER,
        selected_managers,
        overrides,
        None,
        None,
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
    max_parallel_checks_per_manager: usize,
    selected_managers: &[String],
    overrides: &[String],
    manager_concurrency_override: Option<usize>,
    snapshot_log_dir: Option<&Path>,
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
        max_parallel_checks_per_manager,
        selected_managers,
        overrides,
        manager_concurrency_override,
        snapshot_log_dir,
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
    max_parallel_checks_per_manager: usize,
    selected_managers: &[String],
    overrides: &[String],
    manager_concurrency_override: Option<usize>,
    snapshot_log_dir: Option<&Path>,
) -> Result<String, AppError> {
    if !selected_managers.is_empty() {
        config.apply_selected_managers_cli_override(selected_managers)?;
    }
    for override_value in overrides {
        config.apply_cli_override(override_value)?;
    }
    if let Some(manager_concurrency) = manager_concurrency_override {
        config.set_manager_concurrency(manager_concurrency)?;
    }
    let manager_ids = selected_manager_ids(selected_managers)?;
    if command == BatchCommand::Apply
        && runnable_manager_ids(&config, env, &manager_ids)?.is_empty()
    {
        return Ok(String::new());
    }
    let manager_concurrency = config.manager_concurrency()?;
    run_batch_for_managers(
        command,
        &config,
        process,
        http,
        env,
        clock,
        theme,
        terminal,
        max_parallel_checks_per_manager,
        manager_concurrency,
        &manager_ids,
        snapshot_log_dir,
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
) -> Result<Vec<(UpdatePlan, UpdateSelectionPolicy, VersionPolicy)>, AppError> {
    let prepared = prepare_interactive_apply_with_sources(
        config,
        process,
        http,
        env,
        clock,
        selected_managers,
        overrides,
        DEFAULT_MAX_PARALLEL_CHECKS_PER_MANAGER,
    )?;
    Ok(prepared
        .managers
        .into_iter()
        .map(|manager| {
            let selection = manager.manager_config.selection.clone();
            let version_policy = manager.manager_config.version_policy;
            (manager.plan, selection, version_policy)
        })
        .collect())
}

#[expect(clippy::too_many_arguments)]
fn prepare_interactive_apply_with_sources(
    config: UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    selected_managers: &[String],
    overrides: &[String],
    max_parallel_checks_per_manager: usize,
) -> Result<PreparedInteractiveApply, AppError> {
    let (config, manager_configs) =
        prepare_interactive_manager_configs(config, selected_managers, overrides)?;
    let manager_configs = available_manager_configs(manager_configs, env)?;
    let manager_concurrency = config.manager_concurrency()?;
    let managers = run_ordered_parallel(
        manager_configs,
        manager_concurrency,
        "interactive planning managers",
        |manager_config| {
            let manager = configured_manager(manager_config.clone()).map_err(map_manager_error)?;
            let plan = build_manager_plan(
                manager.as_ref(),
                process,
                http,
                env,
                clock,
                &manager_config,
                max_parallel_checks_per_manager,
            )?;
            Ok(PreparedInteractiveManagerApply {
                plan,
                manager_config,
            })
        },
    )
    .map_err(|err| AppError::Execution(err.to_string()))?
    .into_iter()
    .collect::<Result<Vec<_>, AppError>>()?;
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

fn available_manager_configs(
    manager_configs: Vec<ManagerConfig>,
    env: &Env,
) -> Result<Vec<ManagerConfig>, AppError> {
    manager_configs
        .into_iter()
        .filter_map(|manager_config| {
            match manager_executable_is_available(&manager_config.manager_id, env) {
                Ok(true) => Some(Ok(manager_config)),
                Ok(false) => None,
                Err(err) => Some(Err(err)),
            }
        })
        .collect()
}

fn runnable_manager_ids(
    config: &UpnowConfig,
    env: &Env,
    manager_ids: &[ManagerId],
) -> Result<Vec<ManagerId>, AppError> {
    manager_ids
        .iter()
        .filter_map(|manager_id| {
            let manager_config = match config.resolve_manager(manager_id.as_str()) {
                Ok(manager_config) => manager_config,
                Err(err) => return Some(Err(AppError::from(err))),
            };
            if !manager_mode_allows_run(manager_config.mode, true) {
                return None;
            }
            match manager_executable_is_available(manager_id, env) {
                Ok(true) => Some(Ok(manager_id.clone())),
                Ok(false) => None,
                Err(err) => Some(Err(err)),
            }
        })
        .collect()
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
        .map(|(plan, selection_policy, version_policy)| {
            InteractiveSelectionPlan::new(
                selection_view(plan, selection_policy),
                plan.issues.clone(),
                selection_policy.clone(),
                *version_policy,
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
            for ((plan, _, _), draft) in plans.iter().zip(drafts) {
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

#[expect(clippy::too_many_arguments)]
fn run_live_confirmed_selection(
    config: UpnowConfig,
    manager_configs: Vec<ManagerConfig>,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    max_parallel_checks_per_manager: usize,
    manager_concurrency: usize,
    command_log: &InteractiveCommandLog,
) -> Result<Option<(UpnowConfig, Vec<ConfirmedInteractiveManagerApply>)>, AppError> {
    let manager_ids = manager_configs
        .iter()
        .map(|manager_config| manager_config.manager_id.clone())
        .collect::<Vec<_>>();
    let (event_tx, event_rx) = mpsc::channel();
    let (prepared_tx, prepared_rx) = mpsc::channel();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop_requested);
    let process = command_log.process_for_selection(process, event_tx.clone());
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
            max_parallel_checks_per_manager,
            manager_concurrency,
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

    let outcome = match run_interactive_selection_with_planning_events(
        manager_ids,
        event_rx,
        command_log.enabled(),
    ) {
        Ok(outcome) => outcome,
        Err(err) => {
            stop_requested.store(true, Ordering::Relaxed);
            worker.join().map_err(|_| {
                AppError::Planning("interactive planning worker panicked".to_owned())
            })?;
            return Err(AppError::Planning(err.to_string()));
        }
    };

    match outcome {
        InteractiveSelectionOutcome::Cancelled => {
            stop_requested.store(true, Ordering::Relaxed);
            worker.join().map_err(|_| {
                AppError::Planning("interactive planning worker panicked".to_owned())
            })?;
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
    max_parallel_checks_per_manager: usize,
    manager_concurrency: usize,
    event_tx: &mpsc::Sender<InteractiveSelectionPlanningEvent>,
    stop_requested: &AtomicBool,
) -> Result<PreparedInteractiveApply, AppError> {
    let worker_results = run_interactive_planning_workers(
        manager_configs,
        process,
        http,
        env,
        clock,
        max_parallel_checks_per_manager,
        manager_concurrency,
        event_tx,
        stop_requested,
    )?;

    let mut managers = Vec::new();
    let mut planning_failures = Vec::new();
    for result in worker_results {
        match result {
            InteractivePlanningWorkerResult::Ready { manager, .. } => managers.push(manager),
            InteractivePlanningWorkerResult::Failed {
                manager_id, detail, ..
            } => planning_failures.push((manager_id, detail)),
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

enum InteractivePlanningWorkerResult {
    Ready {
        manager: PreparedInteractiveManagerApply,
    },
    Failed {
        manager_id: ManagerId,
        detail: String,
    },
}

#[expect(clippy::too_many_arguments)]
fn run_interactive_planning_workers(
    manager_configs: Vec<ManagerConfig>,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    max_parallel_checks_per_manager: usize,
    manager_concurrency: usize,
    event_tx: &mpsc::Sender<InteractiveSelectionPlanningEvent>,
    stop_requested: &AtomicBool,
) -> Result<Vec<InteractivePlanningWorkerResult>, AppError> {
    run_ordered_parallel_stoppable(
        manager_configs,
        manager_concurrency,
        "interactive planning managers",
        stop_requested,
        |manager_config| {
            prepare_one_interactive_manager_with_events(
                manager_config,
                process,
                http,
                env,
                clock,
                max_parallel_checks_per_manager,
                event_tx,
            )
        },
        |result| result.as_ref().is_err_and(AppError::is_interruption),
    )
    .map_err(|err| AppError::Execution(err.to_string()))?
    .into_iter()
    .collect::<Result<Vec<_>, AppError>>()
}

fn prepare_one_interactive_manager_with_events(
    manager_config: ManagerConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    max_parallel_checks_per_manager: usize,
    event_tx: &mpsc::Sender<InteractiveSelectionPlanningEvent>,
) -> Result<InteractivePlanningWorkerResult, AppError> {
    let manager_id = manager_config.manager_id.clone();
    let _ = event_tx.send(InteractiveSelectionPlanningEvent::ManagerStarted {
        manager_id: manager_id.clone(),
    });

    let manager = match configured_manager(manager_config.clone()).map_err(map_manager_error) {
        Ok(manager) => manager,
        Err(err @ AppError::Interrupted(_)) => return Err(err),
        Err(err) => {
            let detail = err.to_string();
            let _ = event_tx.send(InteractiveSelectionPlanningEvent::ManagerError {
                manager_id: manager_id.clone(),
                detail: detail.clone(),
            });
            return Ok(InteractivePlanningWorkerResult::Failed { manager_id, detail });
        }
    };

    match build_manager_plan(
        manager.as_ref(),
        process,
        http,
        env,
        clock,
        &manager_config,
        max_parallel_checks_per_manager,
    ) {
        Ok(plan) => {
            let selection_policy = manager_config.selection.clone();
            let view = selection_view(&plan, &selection_policy);
            let _ = event_tx.send(InteractiveSelectionPlanningEvent::ManagerReady {
                view,
                issues: plan.issues.clone(),
                selection_policy,
                version_policy: manager_config.version_policy,
            });
            Ok(InteractivePlanningWorkerResult::Ready {
                manager: PreparedInteractiveManagerApply {
                    plan,
                    manager_config,
                },
            })
        }
        Err(err @ AppError::Interrupted(_)) => Err(err),
        Err(err) => {
            let detail = err.to_string();
            let _ = event_tx.send(InteractiveSelectionPlanningEvent::ManagerError {
                manager_id: manager_id.clone(),
                detail: detail.clone(),
            });
            Ok(InteractivePlanningWorkerResult::Failed { manager_id, detail })
        }
    }
}

fn confirmed_from_drafts(
    prepared: PreparedInteractiveApply,
    drafts: &[InteractiveManagerSelectionDraft],
) -> Result<(UpnowConfig, Vec<ConfirmedInteractiveManagerApply>), AppError> {
    if !prepared.planning_failures.is_empty() {
        let details = prepared
            .planning_failures
            .iter()
            .map(|(manager_id, detail)| format!("{manager_id}: {detail}"))
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
    command_log: &InteractiveCommandLog,
) -> Result<ExecutionProgressSummary, AppError> {
    let resolved = resolve_confirmed_execution_plans(&confirmed)?;
    let initial_progress = ExecutionProgressState::from_execution_plans(resolved.clone())
        .with_command_log(command_log.snapshot());
    let (tx, rx) = mpsc::channel();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop_requested);
    let worker_process = command_log.process_for_progress(process, tx.clone());

    thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let mut config = config;
            let result = execute_confirmed_interactive_apply_resolved(
                &mut config,
                &worker_process,
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

        let tui_result =
            run_interactive_progress(initial_progress, &rx, stop_requested, command_log.enabled())
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
        .map_err(|err| AppError::Manager(err.to_string()))?;
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
    max_parallel_checks_per_manager: usize,
    manager_concurrency: usize,
    manager_ids: &[ManagerId],
    snapshot_log_dir: Option<&Path>,
) -> Result<String, AppError> {
    let _spinner = terminal.start_action_spinner(command.terminal_action());
    if command == BatchCommand::Apply {
        return run_batch_apply_for_managers(
            config,
            process,
            http,
            env,
            clock,
            theme,
            max_parallel_checks_per_manager,
            manager_concurrency,
            manager_ids,
            snapshot_log_dir,
        );
    }

    let stop_requested = AtomicBool::new(false);
    let manager_outputs = run_ordered_parallel_stoppable(
        manager_ids.to_vec(),
        manager_concurrency,
        &format!("{command} managers"),
        &stop_requested,
        |manager_id| {
            let result = run_manager_scan_or_plan_batch(
                command,
                config,
                process,
                http,
                env,
                clock,
                theme,
                &manager_id,
                max_parallel_checks_per_manager,
            );
            (manager_id, result)
        },
        |(_, result)| result.as_ref().is_err_and(AppError::is_interruption),
    )
    .map_err(|err| AppError::Execution(err.to_string()))?;

    let mut table = OutcomeTable::default();
    let mut had_error = false;
    let command_label = command.to_string();
    for (manager_id, manager_output) in manager_outputs {
        match manager_output {
            Ok(manager_output) => {
                had_error |= manager_output.failed;
                table.rows.extend(manager_output.table.rows);
            }
            Err(err) if err.is_interruption() => return Err(err),
            Err(err) => {
                had_error = true;
                table.rows.extend(
                    manager_error_table(&manager_id, &command_label, &err.to_string()).rows,
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

struct PreparedBatchApply {
    plan: UpdatePlan,
    manager_config: ManagerConfig,
    selection: PlanSelection,
    execution_plan: ResolvedExecutionPlan,
}

#[expect(clippy::too_many_arguments)]
fn run_manager_scan_or_plan_batch(
    command: BatchCommand,
    config: &UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    theme: OutputTheme,
    manager_id: &ManagerId,
    max_parallel_checks_per_manager: usize,
) -> Result<ManagerBatchOutput, AppError> {
    ensure_known_manager(manager_id.as_str()).map_err(map_manager_error)?;
    let manager_config = config.resolve_manager(manager_id.as_str())?;
    if !manager_mode_allows_run(manager_config.mode, command == BatchCommand::Apply) {
        return Ok(ManagerBatchOutput {
            table: OutcomeTable::default(),
            failed: false,
        });
    }
    if !manager_executable_is_available(manager_id, env)? {
        return Ok(ManagerBatchOutput {
            table: OutcomeTable::default(),
            failed: false,
        });
    }

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
                        max_parallel_checks_per_manager,
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
            let plan = build_manager_plan(
                manager.as_ref(),
                process,
                http,
                env,
                clock,
                &manager_config,
                max_parallel_checks_per_manager,
            )?;
            Ok(ManagerBatchOutput {
                table: update_plan_table(
                    &plan,
                    BatchRenderOptions::new(theme)
                        .with_version_policy(manager_config.version_policy),
                ),
                failed: false,
            })
        }
        BatchCommand::Apply => Err(AppError::Execution(
            "batch apply must be prepared before serial execution".to_owned(),
        )),
    }
}

#[expect(clippy::too_many_arguments)]
fn run_batch_apply_for_managers(
    config: &UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    theme: OutputTheme,
    max_parallel_checks_per_manager: usize,
    manager_concurrency: usize,
    manager_ids: &[ManagerId],
    snapshot_log_dir: Option<&Path>,
) -> Result<String, AppError> {
    let stop_requested = AtomicBool::new(false);
    let prepared = run_ordered_parallel_stoppable(
        manager_ids.to_vec(),
        manager_concurrency,
        "apply planning managers",
        &stop_requested,
        |manager_id| {
            let result = prepare_manager_batch_apply(
                config,
                process,
                http,
                env,
                clock,
                &manager_id,
                max_parallel_checks_per_manager,
            );
            (manager_id, result)
        },
        |(_, result)| result.as_ref().is_err_and(AppError::is_interruption),
    )
    .map_err(|err| AppError::Execution(err.to_string()))?;

    if let Some(log_dir) = snapshot_log_dir {
        write_apply_snapshot_for_selections(
            prepared.iter().filter_map(|(_, prepared_manager)| {
                if let Ok(Some(prepared_manager)) = prepared_manager {
                    Some((&prepared_manager.plan, &prepared_manager.selection))
                } else {
                    None
                }
            }),
            log_dir,
        )?;
    }

    let mut table = OutcomeTable::default();
    let mut had_error = false;
    for (manager_id, prepared_manager) in prepared {
        match prepared_manager {
            Ok(None) => {}
            Ok(Some(prepared_manager)) => {
                match execute_prepared_batch_apply(process, env, prepared_manager) {
                    Ok(manager_output) => {
                        had_error |= manager_output.failed;
                        table.rows.extend(manager_output.table.rows);
                    }
                    Err(err) if err.is_interruption() => return Err(err),
                    Err(err) => {
                        had_error = true;
                        table.rows.extend(
                            manager_error_table(&manager_id, "apply", &err.to_string()).rows,
                        );
                    }
                }
            }
            Err(err) if err.is_interruption() => return Err(err),
            Err(err) => {
                had_error = true;
                table
                    .rows
                    .extend(manager_error_table(&manager_id, "apply", &err.to_string()).rows);
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

fn prepare_manager_batch_apply(
    config: &UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    manager_id: &ManagerId,
    max_parallel_checks_per_manager: usize,
) -> Result<Option<PreparedBatchApply>, AppError> {
    ensure_known_manager(manager_id.as_str()).map_err(map_manager_error)?;
    let manager_config = config.resolve_manager(manager_id.as_str())?;
    if !manager_mode_allows_run(manager_config.mode, true) {
        return Ok(None);
    }
    if !manager_executable_is_available(manager_id, env)? {
        return Ok(None);
    }

    let manager = configured_manager(manager_config.clone()).map_err(map_manager_error)?;
    let plan = build_manager_plan(
        manager.as_ref(),
        process,
        http,
        env,
        clock,
        &manager_config,
        max_parallel_checks_per_manager,
    )?;
    let selection = default_batch_selection(&plan, &manager_config.selection)
        .map_err(|err| AppError::Planning(err.to_string()))?;
    let execution_plan = resolve_selection_for_execution(
        &plan,
        &selection,
        manager.capabilities(),
        manager_config.version_policy,
    )
    .map_err(|err| AppError::Manager(err.to_string()))?;

    Ok(Some(PreparedBatchApply {
        plan,
        manager_config,
        selection,
        execution_plan,
    }))
}

fn execute_prepared_batch_apply(
    process: &ProcessRunner,
    env: &Env,
    prepared: PreparedBatchApply,
) -> Result<ManagerBatchOutput, AppError> {
    let manager = configured_manager(prepared.manager_config).map_err(map_manager_error)?;
    let commands = manager
        .commands_for_execution_plan(process, env, &prepared.execution_plan)
        .map_err(map_manager_error)?;
    let report =
        execute_commands(prepared.plan.manager_id.clone(), commands, process).map_err(|err| {
            if err.is_interruption() {
                AppError::Interrupted(err.to_string())
            } else {
                AppError::Execution(err.to_string())
            }
        })?;
    Ok(ManagerBatchOutput {
        table: apply_execution_report_table(&report, &prepared.plan, &prepared.selection),
        failed: execution_report_has_failures(&report),
    })
}

fn build_scan_report(
    manager_id: ManagerId,
    manager: &dyn ManagerAdapter,
    process: &ProcessRunner,
    env: &Env,
) -> Result<ScanReport, AppError> {
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
    max_parallel_checks_per_manager: usize,
) -> Result<ScanReport, AppError> {
    match manager.scan_inputs_with_release_evidence(
        process,
        http,
        env,
        max_parallel_checks_per_manager,
    ) {
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
    let command = cli.command.unwrap_or(CliCommand::Plan);
    let interactive_apply = command == CliCommand::Apply && !cli.yolo;
    let log_dir = init_command_logging(cli, &env, command, interactive_apply)?;
    let process = ProcessRunner::new(MutationMode::from_env_and_debug_no_mutate(
        &env,
        cli.debug_no_mutate(),
    ));
    if cli.yolo && command != CliCommand::Apply {
        return Err(AppError::InvalidArgs(
            "--yolo is only supported with apply".to_owned(),
        ));
    }
    if interactive_apply {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return Err(AppError::InvalidArgs(
                "interactive apply requires a TTY; use --yolo for non-interactive apply".to_owned(),
            ));
        }
        let command_log = InteractiveCommandLog::new(cli.show_commands);
        return run_interactive_apply_with_sources_and_options(
            config,
            &process,
            &HttpClient::real(&HttpSettings::default_client_settings())
                .map_err(|err| AppError::Manager(err.to_string()))?,
            &env,
            Clock::system(),
            &cli.managers,
            &cli.overrides,
            cli.max_parallel_checks_per_manager,
            cli.manager_concurrency.map(NonZeroUsize::get),
            &command_log,
            log_dir.as_deref(),
        );
    }
    let theme = OutputTheme::from_environment(ThemeOptions {
        plain: cli.plain,
        no_color: cli.no_color,
        verbose: cli.verbose,
    });
    let terminal = BatchTerminal::from_environment(theme);
    maybe_emit_apply_mutation_mode_notice(command.into(), &process, &env, terminal)?;
    let terminal = if cli.show_commands {
        terminal.suppress_spinner()
    } else {
        terminal
    };
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
        cli.max_parallel_checks_per_manager,
        &cli.managers,
        &cli.overrides,
        cli.manager_concurrency.map(NonZeroUsize::get),
        if command == CliCommand::Apply {
            log_dir.as_deref()
        } else {
            None
        },
    )
}

fn init_command_logging(
    cli: &Cli,
    env: &Env,
    command: CliCommand,
    interactive_apply: bool,
) -> Result<Option<PathBuf>, AppError> {
    let options = LoggingOptions {
        debug_commands: cli.debug_commands,
        show_commands: cli.show_commands && !interactive_apply,
        show_command_colors: cli.show_commands
            && !interactive_apply
            && command_prefix_color_enabled(cli),
    };

    let path = match init_logging(options, env) {
        Ok(path) => Some(path),
        Err(err) if options.debug_commands || command == CliCommand::Apply => {
            return Err(AppError::Execution(err.to_string()));
        }
        Err(_) => None,
    };

    if options.debug_commands && (cli.yolo || command != CliCommand::Apply) {
        let path = path
            .as_ref()
            .expect("debug logging initialization succeeded");
        eprintln!("debug logs: {}", path.display());
    }

    Ok(path)
}

fn command_prefix_color_enabled(cli: &Cli) -> bool {
    !cli.plain
        && !cli.no_color
        && std::io::stderr().is_terminal()
        && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
        && std::env::var("TERM").map_or(true, |value| value != "dumb")
}

fn maybe_emit_apply_mutation_mode_notice(
    command: BatchCommand,
    process: &ProcessRunner,
    env: &Env,
    terminal: BatchTerminal,
) -> Result<(), AppError> {
    if let Some(notice) = apply_mutation_mode_notice(command, process, env, terminal)? {
        eprintln!("{notice}");
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

    let Some(mutation_mode) = process.mutation_mode() else {
        return Ok(None);
    };

    validate_required_mutation_mode(env, mutation_mode)?;

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

const fn skip_mode_hint() -> &'static str {
    if cfg!(debug_assertions) {
        "set UPNOW_SKIP_MUTATING_COMMANDS=1 or --debug-no-mutate in debug builds"
    } else {
        "set UPNOW_SKIP_MUTATING_COMMANDS=1"
    }
}

const fn real_mode_hint() -> &'static str {
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
    max_parallel_checks_per_manager: usize,
) -> Result<UpdatePlan, AppError> {
    let now = clock.now();
    let inputs = manager
        .update_inputs(process, http, env, max_parallel_checks_per_manager)
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

fn manager_executable_is_available(manager_id: &ManagerId, env: &Env) -> Result<bool, AppError> {
    let executable = required_executable(manager_id.as_str()).map_err(map_manager_error)?;
    Ok(command_exists_in_env(executable, env))
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
