//! CLI-layer behavior for the `upnow` rebuild.

pub mod config;

use std::fmt::{self, Display};

use clap::{Parser, Subcommand};
use config::{ConfigError, ManagerConfig, UpnowConfig};
use upnow_domain::{ExecutionEligibility, UpdatePlan};
use upnow_execution::{ExecutionCommand, execute_commands};
use upnow_infra::{Clock, Env, MutationMode, ProcessRunner};
use upnow_managers::pnpm;
use upnow_planning::{PlanningSettings, default_batch_selection, update_plan_from_seeds};
use upnow_presentation::{render_execution_report, render_scan_report, render_update_plan};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    Config(String),
    InvalidArgs(String),
    Pnpm(String),
    Planning(String),
    Execution(String),
    Interrupted(String),
}

impl Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(detail)
            | Self::InvalidArgs(detail)
            | Self::Pnpm(detail)
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

/// Runs the phase-6 pnpm batch command with explicit dependencies.
///
/// # Errors
///
/// Returns an error for config, discovery, planning, or execution failures.
pub fn run_pnpm_batch(
    command: BatchCommand,
    config: &UpnowConfig,
    process: &ProcessRunner,
    clock: Clock,
) -> Result<String, AppError> {
    run_pnpm_batch_with_options(command, config, process, clock, false)
}

/// Runs the phase-6 pnpm batch command with explicit dependencies and scan options.
///
/// # Errors
///
/// Returns an error for config, discovery, planning, or execution failures.
pub fn run_pnpm_batch_with_options(
    command: BatchCommand,
    config: &UpnowConfig,
    process: &ProcessRunner,
    clock: Clock,
    verbose: bool,
) -> Result<String, AppError> {
    let manager_config = config.resolve_manager(pnpm::MANAGER_ID)?;
    if !manager_config
        .mode
        .allows_run(command == BatchCommand::Apply)
    {
        return Ok(format!("manager {} is off\n", pnpm::MANAGER_ID));
    }

    match command {
        BatchCommand::Scan if verbose => {
            let old_age_threshold = config.scan_old_age_threshold()?;
            Ok(render_scan_report(
                &pnpm::verbose_scan(process, clock.now()).map_err(map_pnpm_error)?,
                Some(old_age_threshold),
            ))
        }
        BatchCommand::Scan => Ok(render_scan_report(
            &pnpm::scan(process).map_err(map_pnpm_error)?,
            None,
        )),
        BatchCommand::Plan => {
            let plan = build_pnpm_plan(process, clock, &manager_config)?;
            Ok(render_update_plan(&plan))
        }
        BatchCommand::Apply => {
            let plan = build_pnpm_plan(process, clock, &manager_config)?;
            let selection = default_batch_selection(&plan, &manager_config.pinned)
                .map_err(|err| AppError::Planning(err.to_string()))?;
            let commands = pnpm::exact_commands_for_selection(&plan, &selection)
                .map_err(map_pnpm_error)?
                .into_iter()
                .map(|command| ExecutionCommand {
                    plan_item_id: command.plan_item_id,
                    package_name: command.package_name,
                    installed_version: command.installed_version,
                    target_version: command.target_version,
                    command: command.command,
                })
                .collect();
            let report =
                execute_commands(plan.manager_id.clone(), commands, process).map_err(|err| {
                    if err.is_interruption() {
                        AppError::Interrupted(err.to_string())
                    } else {
                        AppError::Execution(err.to_string())
                    }
                })?;
            Ok(render_execution_report(&report))
        }
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
    validate_phase_6_managers(&cli.managers)?;
    let mut config = UpnowConfig::load()?;
    if !cli.managers.is_empty() {
        config.apply_selected_managers_cli_override(&cli.managers)?;
    }
    for override_value in cli.overrides {
        config.apply_cli_override(&override_value)?;
    }
    let env = Env::real();
    let process = ProcessRunner::new(MutationMode::from_env(&env));
    run_pnpm_batch_with_options(
        cli.command.unwrap_or(CliCommand::Plan).into(),
        &config,
        &process,
        Clock::system(),
        cli.verbose,
    )
}

fn build_pnpm_plan(
    process: &ProcessRunner,
    clock: Clock,
    manager_config: &ManagerConfig,
) -> Result<UpdatePlan, AppError> {
    let seeds =
        pnpm::update_seeds(process, manager_config.version_policy).map_err(map_pnpm_error)?;
    update_plan_from_seeds(
        manager_config.manager_id.clone(),
        seeds,
        PlanningSettings {
            policy: manager_config.version_policy,
            now: clock.now(),
            min_release_age: manager_config.min_release_age,
            execution_eligibility: ExecutionEligibility::ExactOnly,
        },
    )
    .map_err(|err| AppError::Planning(err.to_string()))
}

fn map_pnpm_error(err: pnpm::PnpmError) -> AppError {
    if err.is_interruption() {
        AppError::Interrupted(err.to_string())
    } else {
        AppError::Pnpm(err.to_string())
    }
}

fn validate_phase_6_managers(managers: &[String]) -> Result<(), AppError> {
    if managers
        .iter()
        .any(|manager| manager.as_str() != pnpm::MANAGER_ID)
    {
        return Err(AppError::InvalidArgs(format!(
            "phase 6 supports only manager `{}`",
            pnpm::MANAGER_ID
        )));
    }
    Ok(())
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
}
