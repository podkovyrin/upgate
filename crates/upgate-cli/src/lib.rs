//! CLI-layer behavior for the `upgate` rebuild.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

mod batch;
pub mod config;
mod interactive;
mod registry;
mod snapshot;

use std::fmt::{self, Display};
use std::io::IsTerminal;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::SystemTime;

use batch::run_batch;
use clap::{Parser, Subcommand};
use config::{ConfigError, ConfigFile};
use interactive::{InteractiveCommandLog, run_interactive_apply};
use registry::{available_manager_ids, ensure_known_manager, required_executable};
use upgate_audit::AuditService;
use upgate_domain::{ManagerConfig, ManagerId, ManagerMode, UpdatePlan};
use upgate_execution::{ExecutionReport, ExecutionStatus};
use upgate_infra::{
    Env, HttpClient, HttpSettings, LoggingOptions, MutationMode, ProcessRunner,
    command_exists_in_env, init_logging,
};
use upgate_managers::adapter::{ManagerAdapter, ManagerAdapterError};
use upgate_planning::{PlanningSettings, derive_audit_queries, finalize_plan_from_inputs};
use upgate_presentation::{
    OutputTheme, ThemeOptions,
    terminal::{BatchTerminal, BatchTerminalAction, MutationNotice},
};

pub use interactive::ConfirmedInteractiveManagerApply;

const DEFAULT_MAX_PARALLEL_CHECKS_PER_MANAGER: usize = 6;

#[derive(Debug, Parser)]
#[command(
    name = "upgate",
    about = "Keep globally installed developer tools up to date."
)]
#[expect(clippy::struct_excessive_bools)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
    /// Run only the selected managers, separated by commas.
    #[arg(
        long = "managers",
        alias = "manager",
        value_delimiter = ',',
        global = true
    )]
    managers: Vec<String>,
    /// Override config for this run, such as npm.mode=plan.
    #[arg(long = "set", short = 'S', global = true)]
    overrides: Vec<String>,
    /// Show more detail about decisions and skipped work.
    #[arg(long, global = true)]
    verbose: bool,
    /// Limit concurrent metadata checks within each manager.
    #[arg(long, default_value_t = DEFAULT_MAX_PARALLEL_CHECKS_PER_MANAGER, global = true)]
    max_parallel_checks_per_manager: usize,
    /// Limit how many managers run at the same time.
    #[arg(long, global = true)]
    manager_concurrency: Option<NonZeroUsize>,
    /// Disable colored output.
    #[arg(long, global = true)]
    no_color: bool,
    /// Use plain output without terminal styling.
    #[arg(long, global = true)]
    plain: bool,
    /// Save command output and timing details to a log file.
    #[arg(long, global = true)]
    log_commands: bool,
    /// Print each external command before it runs.
    #[arg(
        long,
        visible_aliases = ["print-commands"],
        global = true
    )]
    trace_commands: bool,
    /// Apply selected updates without opening the interactive picker.
    #[arg(long, visible_aliases = ["dangerously-skip-confirmation", "no-approval"], global = true)]
    yolo: bool,
    /// Preview apply without running install or upgrade commands.
    #[arg(long, global = true)]
    dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
enum CliCommand {
    /// List installed tools and versions.
    Scan,
    /// Show available updates without applying them.
    Plan,
    /// Update selected tools.
    Apply,
}

impl Display for CliCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Scan => "scan",
            Self::Plan => "plan",
            Self::Apply => "apply",
        })
    }
}

impl CliCommand {
    const fn terminal_action(self) -> BatchTerminalAction {
        match self {
            Self::Scan => BatchTerminalAction::Scan,
            Self::Plan => BatchTerminalAction::Plan,
            Self::Apply => BatchTerminalAction::Apply,
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

pub struct CliRunOutput {
    pub stdout: String,
    pub command_log_dir: Option<PathBuf>,
}

pub enum CliRunResult {
    Completed(CliRunOutput),
    Cancelled(CliRunOutput),
    Failed {
        error: AppError,
        command_log_dir: Option<PathBuf>,
    },
}

pub fn run_from_env_with_report() -> CliRunResult {
    let cli = Cli::parse();
    run_cli(&cli)
}

fn run_cli(cli: &Cli) -> CliRunResult {
    let config = match ConfigFile::load() {
        Ok(config) => config,
        Err(error) => {
            return CliRunResult::Failed {
                error: error.into(),
                command_log_dir: None,
            };
        }
    };
    let env = Env::real();
    let command = cli.command.unwrap_or(CliCommand::Apply);
    let interactive_apply = command == CliCommand::Apply && !cli.yolo;
    let log_dir = match init_command_logging(cli, &env, command, interactive_apply) {
        Ok(log_dir) => log_dir,
        Err(error) => {
            return CliRunResult::Failed {
                error,
                command_log_dir: None,
            };
        }
    };
    let process = ProcessRunner::new(MutationMode::from_dry_run(cli.dry_run));
    let result = (|| {
        if cli.yolo && command != CliCommand::Apply {
            return Err(AppError::InvalidArgs(
                "--yolo is only supported with apply".to_owned(),
            ));
        }
        let theme = OutputTheme::from_environment(ThemeOptions {
            plain: cli.plain,
            no_color: cli.no_color,
            verbose: cli.verbose,
        });
        if interactive_apply {
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                return Err(AppError::InvalidArgs(
                    "interactive apply requires a TTY; use --yolo for non-interactive apply"
                        .to_owned(),
                ));
            }
            let command_log = InteractiveCommandLog::new(cli.trace_commands);
            return run_interactive_apply(
                config,
                &process,
                &HttpClient::real(&HttpSettings::default())
                    .map_err(|err| AppError::Manager(err.to_string()))?,
                &env,
                &cli.managers,
                &cli.overrides,
                cli.max_parallel_checks_per_manager,
                cli.manager_concurrency.map(NonZeroUsize::get),
                &command_log,
                log_dir.as_deref(),
                theme,
            );
        }
        let terminal = BatchTerminal::from_environment(theme);
        maybe_emit_apply_mutation_mode_notice(command, &process, terminal, cli.dry_run);
        let terminal = if cli.trace_commands {
            terminal.suppress_spinner()
        } else {
            terminal
        };
        run_batch(
            command,
            config,
            &process,
            &HttpClient::real(&HttpSettings::default())
                .map_err(|err| AppError::Manager(err.to_string()))?,
            &env,
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
        .map(Some)
    })();

    match result {
        Ok(None) => CliRunResult::Cancelled(CliRunOutput {
            stdout: String::new(),
            command_log_dir: reported_command_log_dir(cli, log_dir.as_ref()),
        }),
        Ok(Some(stdout)) => CliRunResult::Completed(CliRunOutput {
            stdout,
            command_log_dir: reported_command_log_dir(cli, log_dir.as_ref()),
        }),
        Err(error) => CliRunResult::Failed {
            error,
            command_log_dir: reported_command_log_dir(cli, log_dir.as_ref()),
        },
    }
}

fn reported_command_log_dir(cli: &Cli, log_dir: Option<&PathBuf>) -> Option<PathBuf> {
    if cli.log_commands {
        log_dir.cloned()
    } else {
        None
    }
}

fn init_command_logging(
    cli: &Cli,
    env: &Env,
    command: CliCommand,
    interactive_apply: bool,
) -> Result<Option<PathBuf>, AppError> {
    let options = LoggingOptions {
        log_commands: cli.log_commands,
        trace_commands: cli.trace_commands && !interactive_apply,
        trace_command_colors: cli.trace_commands
            && !interactive_apply
            && command_prefix_color_enabled(cli),
    };

    let path = match init_logging(options, env) {
        Ok(path) => Some(path),
        Err(err) if options.log_commands || command == CliCommand::Apply => {
            return Err(AppError::Execution(err.to_string()));
        }
        Err(_) => None,
    };

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
    command: CliCommand,
    process: &ProcessRunner,
    terminal: BatchTerminal,
    dry_run: bool,
) {
    if command != CliCommand::Apply {
        return;
    }

    let Some(mutation_mode) = process.mutation_mode() else {
        return;
    };

    if !(dry_run || cfg!(debug_assertions)) || !terminal.notice_enabled() {
        return;
    }

    let notice = match mutation_mode {
        MutationMode::Skip => MutationNotice::Skip,
        MutationMode::Real => MutationNotice::Real,
    };
    eprintln!("{notice}");
}

fn build_manager_plan(
    manager: &dyn ManagerAdapter,
    process: &ProcessRunner,
    http: &HttpClient,
    audit_service: &AuditService,
    env: &Env,
    manager_config: &ManagerConfig,
    max_parallel_checks_per_manager: usize,
) -> Result<UpdatePlan, AppError> {
    let now = SystemTime::now();
    let inputs = manager
        .update_inputs(process, http, env, max_parallel_checks_per_manager)
        .map_err(map_manager_error)?;
    let settings = PlanningSettings {
        policy: manager_config.version_policy,
        now,
        min_release_age: manager_config.min_release_age,
    };
    let audit_results = audit_service
        .query(derive_audit_queries(&inputs))
        .map_err(|err| AppError::Planning(err.to_string()))?;
    finalize_plan_from_inputs(
        manager_config.manager_id.clone(),
        inputs,
        settings,
        &audit_results,
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
