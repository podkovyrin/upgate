//! CLI-layer behavior for the `upnow` rebuild.

pub mod config;

use std::fmt::{self, Display};
use std::time::SystemTime;

use clap::{Parser, Subcommand};
use config::{ConfigError, ManagerConfig, UpnowConfig};
use upnow_domain::{
    ExecutionEligibility, InstalledTool, ManagerId, ManagerScanInput, PlanIssue,
    ReleaseLookupResult, ScanIssue, ScanItem, ScanReport, UpdatePlan,
};
use upnow_execution::{
    ExecutionCapabilities, ExecutionCommand, ExecutionCommandItem, ExecutionReport,
    ExecutionSelectionError, ExecutionStatus, execute_commands, resolve_selection_for_execution,
};
use upnow_infra::{Clock, Env, HttpClient, HttpSettings, MutationMode, ProcessRunner};
use upnow_managers::adapter::{
    CommandBuildSettings, ManagerAdapter, ManagerAdapterError, ManagerExecutionCommand,
    ReleaseLookupSubject,
};
use upnow_managers::registry::{available_managers, manager_by_id};
use upnow_planning::{PlanningSettings, default_batch_selection, update_plan_from_inputs};
use upnow_presentation::{
    render_execution_report, render_manager_error, render_scan_report, render_update_plan,
};
use upnow_release::release_age_for_version;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchCommand {
    Scan,
    Plan,
    Apply,
}

#[derive(Debug, Parser)]
#[command(name = "upnow")]
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
}

#[derive(Debug, Clone, Copy, Subcommand)]
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
    #[must_use]
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

impl From<ConfigError> for AppError {
    fn from(value: ConfigError) -> Self {
        Self::Config(value.to_string())
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
    run_batch_with_sources(
        command,
        config,
        process,
        &http,
        &env,
        clock,
        verbose,
        selected_managers,
        overrides,
    )
}

/// Runs a batch command with explicit release metadata sources.
///
/// # Errors
///
/// Returns an error for config, discovery, planning, or execution failures.
pub fn run_batch_with_sources(
    command: BatchCommand,
    mut config: UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    verbose: bool,
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
        verbose,
        &manager_ids,
    )
}

fn run_batch_for_managers(
    command: BatchCommand,
    config: &UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    verbose: bool,
    manager_ids: &[ManagerId],
) -> Result<String, AppError> {
    let mut output = String::new();
    let mut had_error = false;
    for manager_id in manager_ids {
        match run_manager_batch(
            command, config, process, http, env, clock, verbose, manager_id,
        ) {
            Ok(manager_output) => {
                had_error |= manager_output.failed;
                output.push_str(&manager_output.rendered);
            }
            Err(err) if err.is_interruption() => return Err(err),
            Err(err) => {
                had_error = true;
                output.push_str(&render_manager_error(
                    manager_id,
                    command.as_str(),
                    &err.to_string(),
                ));
            }
        }
    }
    if had_error {
        Err(AppError::Manager(output))
    } else {
        Ok(output)
    }
}

struct ManagerBatchOutput {
    rendered: String,
    failed: bool,
}

fn run_manager_batch(
    command: BatchCommand,
    config: &UpnowConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    clock: Clock,
    verbose: bool,
    manager_id: &ManagerId,
) -> Result<ManagerBatchOutput, AppError> {
    let manager = manager_by_id(manager_id).map_err(map_manager_error)?;
    let manager_config = config.resolve_manager(manager.id())?;
    if !manager_config
        .mode
        .allows_run(command == BatchCommand::Apply)
    {
        return Ok(ManagerBatchOutput {
            rendered: String::new(),
            failed: false,
        });
    }

    match command {
        BatchCommand::Scan if verbose => {
            let old_age_threshold = config.scan_old_age_threshold()?;
            Ok(ManagerBatchOutput {
                rendered: render_scan_report(
                    &build_verbose_scan_report(manager, process, http, env, clock.now())?,
                    Some(old_age_threshold),
                ),
                failed: false,
            })
        }
        BatchCommand::Scan => Ok(ManagerBatchOutput {
            rendered: render_scan_report(&build_scan_report(manager, process, env)?, None),
            failed: false,
        }),
        BatchCommand::Plan => {
            let plan = build_manager_plan(manager, process, http, env, clock, &manager_config)?;
            Ok(ManagerBatchOutput {
                rendered: render_update_plan(&plan),
                failed: false,
            })
        }
        BatchCommand::Apply => {
            let plan = build_manager_plan(manager, process, http, env, clock, &manager_config)?;
            let selection = default_batch_selection(&plan, &manager_config.pinned)
                .map_err(|err| AppError::Planning(err.to_string()))?;
            let execution_plan = resolve_selection_for_execution(
                &plan,
                &selection,
                execution_capabilities(manager),
                manager_config.version_policy,
            )
            .map_err(map_execution_selection_error)?;
            let commands = manager
                .commands_for_execution_plan(
                    process,
                    env,
                    &execution_plan,
                    CommandBuildSettings {
                        min_release_age: manager_config.min_release_age,
                    },
                )
                .map_err(map_manager_error)?
                .into_iter()
                .map(execution_command_from_manager)
                .collect();
            let report =
                execute_commands(plan.manager_id.clone(), commands, process).map_err(|err| {
                    if err.is_interruption() {
                        AppError::Interrupted(err.to_string())
                    } else {
                        AppError::Execution(err.to_string())
                    }
                })?;
            let output = render_execution_report(&report, &plan.issues);
            Ok(ManagerBatchOutput {
                rendered: output,
                failed: execution_report_has_failures(&report),
            })
        }
    }
}

fn build_scan_report(
    manager: &dyn ManagerAdapter,
    process: &ProcessRunner,
    env: &Env,
) -> Result<ScanReport, AppError> {
    let manager_id = manager.manager_id();
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
    manager: &dyn ManagerAdapter,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    now: SystemTime,
) -> Result<ScanReport, AppError> {
    let manager_id = manager.manager_id();
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
    let inputs = match manager.scan_inputs(process, env) {
        Ok(inputs) => inputs,
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
    };

    let mut items = Vec::new();
    for input in inputs {
        match input {
            ManagerScanInput::Installed(tool) => {
                items.push(verbose_scan_item(manager, process, http, env, now, tool)?);
            }
            ManagerScanInput::Skipped { installed, reason } => {
                items.push(ScanItem::Skipped {
                    tool: installed,
                    reason,
                });
            }
        }
    }

    Ok(ScanReport::new(manager_id, items, Vec::new()))
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

fn verbose_scan_item(
    manager: &dyn ManagerAdapter,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    now: SystemTime,
    tool: InstalledTool,
) -> Result<ScanItem, AppError> {
    match manager
        .release_lookup(process, http, env, ReleaseLookupSubject::Installed(&tool))
        .map_err(map_manager_error)?
    {
        ReleaseLookupResult::Known(timeline) => {
            match release_age_for_version(&timeline, &tool.installed_version, now) {
                Some(age) => Ok(ScanItem::InstalledWithReleaseAge { tool, age }),
                None => Ok(ScanItem::Installed(tool)),
            }
        }
        ReleaseLookupResult::MissingMetadata => Ok(ScanItem::Installed(tool)),
        ReleaseLookupResult::LookupFailed(err) => Ok(ScanItem::Skipped {
            tool,
            reason: ScanIssue::ReleaseLookupFailed { detail: err.detail },
        }),
    }
}

/// Runs from process environment and command-line arguments.
///
/// # Errors
///
/// Returns an error for invalid arguments or command execution failures.
pub fn run_from_env() -> Result<String, AppError> {
    let cli = Cli::parse();
    run_cli(cli)
}

fn run_cli(cli: Cli) -> Result<String, AppError> {
    let config = UpnowConfig::load()?;
    let env = Env::real();
    let process = ProcessRunner::new(MutationMode::from_env(&env));
    run_batch(
        cli.command.unwrap_or(CliCommand::Plan).into(),
        config,
        &process,
        Clock::system(),
        cli.verbose,
        &cli.managers,
        &cli.overrides,
    )
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
        .update_inputs(
            process,
            http,
            env,
            manager_config.version_policy,
            manager_config.min_release_age,
            now,
        )
        .map_err(map_manager_error)?;
    update_plan_from_inputs(
        manager_config.manager_id.clone(),
        inputs,
        PlanningSettings {
            policy: manager_config.version_policy,
            now,
            min_release_age: manager_config.min_release_age,
            execution_eligibility: execution_eligibility(manager),
        },
    )
    .map_err(|err| AppError::Planning(err.to_string()))
}

fn execution_eligibility(manager: &dyn ManagerAdapter) -> ExecutionEligibility {
    let capabilities = manager.capabilities();
    if capabilities.native_update && capabilities.exact_target {
        ExecutionEligibility::NativeOrExact
    } else if capabilities.native_update {
        ExecutionEligibility::NativeOnly
    } else {
        ExecutionEligibility::ExactOnly
    }
}

fn execution_capabilities(manager: &dyn ManagerAdapter) -> ExecutionCapabilities {
    let capabilities = manager.capabilities();
    ExecutionCapabilities {
        exact_target: capabilities.exact_target,
        native_update: capabilities.native_update,
        native_global_update: capabilities.native_global_update,
    }
}

fn execution_command_from_manager(command: ManagerExecutionCommand) -> ExecutionCommand {
    ExecutionCommand {
        items: command
            .items
            .into_iter()
            .map(|item| ExecutionCommandItem {
                plan_item_id: item.plan_item_id,
                package_name: item.package_name,
                installed_version: item.installed_version,
                target_version: item.target_version,
            })
            .collect(),
        command: command.command,
    }
}

fn execution_report_has_failures(report: &ExecutionReport) -> bool {
    report
        .items
        .iter()
        .any(|item| matches!(item.status, ExecutionStatus::Failed { .. }))
}

fn map_manager_error(err: ManagerAdapterError) -> AppError {
    if err.is_interruption() {
        AppError::Interrupted(err.to_string())
    } else {
        AppError::Manager(err.to_string())
    }
}

fn map_execution_selection_error(err: ExecutionSelectionError) -> AppError {
    AppError::Manager(err.to_string())
}

fn selected_manager_ids(selected_managers: &[String]) -> Result<Vec<ManagerId>, AppError> {
    if selected_managers.is_empty() {
        return Ok(available_managers()
            .into_iter()
            .map(ManagerAdapter::manager_id)
            .collect());
    }

    let mut manager_ids = Vec::new();
    for manager_id in selected_managers {
        let manager_id = ManagerId::new(manager_id.clone())
            .map_err(|err| AppError::InvalidArgs(err.to_string()))?;
        manager_by_id(&manager_id).map_err(map_manager_error)?;
        if !manager_ids.contains(&manager_id) {
            manager_ids.push(manager_id);
        }
    }
    Ok(manager_ids)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, CliCommand};

    #[test]
    fn parses_legacy_comma_delimited_managers_flag() {
        let cli = Cli::try_parse_from(["upnow", "--managers", "pnpm,npm", "scan"])
            .expect("CLI should parse legacy manager list shape");

        assert!(matches!(cli.command, Some(CliCommand::Scan)));
        assert_eq!(cli.managers, ["pnpm", "npm"]);
    }

    #[test]
    fn parses_singular_manager_alias() {
        let cli = Cli::try_parse_from(["upnow", "--manager", "pnpm", "plan"])
            .expect("CLI should parse singular manager alias");

        assert!(matches!(cli.command, Some(CliCommand::Plan)));
        assert_eq!(cli.managers, ["pnpm"]);
    }

    #[test]
    fn missing_command_is_available_for_default_plan_dispatch() {
        let cli = Cli::try_parse_from(["upnow", "--manager", "npm"])
            .expect("CLI should parse without a subcommand");

        assert!(cli.command.is_none());
        assert_eq!(cli.managers, ["npm"]);
    }

    #[test]
    fn parses_set_overrides() {
        let cli = Cli::try_parse_from([
            "upnow",
            "--manager",
            "npm",
            "--set",
            "npm.version_policy=stable",
            "plan",
        ])
        .expect("CLI should parse overrides");

        assert_eq!(cli.overrides, ["npm.version_policy=stable"]);
    }
}
