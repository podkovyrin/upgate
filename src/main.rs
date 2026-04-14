mod config;
mod interactive;
mod manager;
mod managers;
mod outcome;
mod ui;
mod util;

use anyhow::Error;
use clap::{Parser, Subcommand};
use config::UpnowConfig;
use manager::{RunMode, all_plugins, build_ctx_for_plugin, resolve_selected_plugins};
use ui::{finish_manager_spinner, init_output_theme, start_manager_spinner};
use util::logging::{LoggingOptions, init_logging, set_current_manager};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
enum Command {
    /// Compute and print intended updates (non-mutating).
    Plan,
    /// Apply updates using manager-native upgrade commands.
    Apply,
    /// List installed package/tool versions across managers.
    Scan,
}

#[derive(Debug, Clone, Parser)]
#[command(name = "upnow")]
#[command(about = "Delay-aware global package upgrades")]
#[command(version)]
#[allow(clippy::struct_excessive_bools)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Maximum concurrent checks.
    #[arg(long, default_value_t = 6, global = true)]
    max_parallel_checks: usize,

    /// Managers to run (comma-separated manager IDs).
    #[arg(long, value_delimiter = ',', global = true)]
    managers: Vec<String>,

    /// Override config values (repeatable), format: <manager>.<key>=<value>
    #[arg(long = "set", short = 'S', global = true)]
    set: Vec<String>,

    /// Disable ANSI color output.
    #[arg(long, global = true)]
    no_color: bool,

    /// Force plain output (no color, no Unicode symbols).
    #[arg(long, global = true)]
    plain: bool,

    /// Show additional metadata in outcome lines.
    #[arg(long, global = true)]
    verbose: bool,

    /// Persist full command debug logs (stdout/stderr + timing) under XDG state.
    #[arg(long, global = true)]
    debug_commands: bool,

    /// Print each command to stderr before execution.
    #[arg(long, visible_alias = "print-commands", global = true)]
    show_commands: bool,

    /// Prompt per manager to select which updates to apply.
    #[arg(long, global = true)]
    interactive: bool,
}

fn main() {
    let cli = Cli::parse();
    init_output_theme(cli.plain, cli.no_color, cli.verbose);

    if let Err(err) = init_logging(LoggingOptions {
        debug_commands: cli.debug_commands,
        show_commands: cli.show_commands,
    }) {
        eprintln!("error: failed to initialize logging: {err}");
        std::process::exit(1);
    }

    if cli.debug_commands
        && let Some(path) = util::logging::session_dir()
    {
        eprintln!("debug logs: {}", path.display());
    }
    let run_mode = match cli.command.unwrap_or(Command::Plan) {
        Command::Plan => RunMode::Plan,
        Command::Apply => RunMode::Apply,
        Command::Scan => RunMode::Scan,
    };

    if cli.interactive {
        if !matches!(run_mode, RunMode::Apply) {
            eprintln!("error: --interactive is only supported with 'apply'");
            std::process::exit(1);
        }

        if let Err(err) = interactive::ensure_tty_available() {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }

    let mut config = match UpnowConfig::load() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    };

    let known_manager_ids: Vec<&str> = all_plugins().iter().map(|p| p.id()).collect();
    for override_arg in &cli.set {
        if let Err(err) = config.apply_cli_override(override_arg, &known_manager_ids) {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }

    let selected_plugins = match resolve_selected_plugins(&cli.managers) {
        Ok(plugins) => plugins,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    };

    let mut had_manager_failure = false;

    for plugin in selected_plugins {
        set_current_manager(Some(plugin.id()));

        let manager_ctx = match build_ctx_for_plugin(
            plugin,
            run_mode,
            cli.max_parallel_checks,
            &config,
            cli.interactive,
        ) {
            Ok(ctx) => ctx,
            Err(err) => {
                eprintln!("error: manager '{}' setup failed: {err}", plugin.id());
                had_manager_failure = true;
                continue;
            }
        };

        if !manager_ctx
            .policy
            .mode
            .allows_run(matches!(run_mode, RunMode::Apply))
        {
            continue;
        }

        let spinner = start_manager_spinner(plugin.id(), run_mode);
        let run_result = plugin.run(&manager_ctx);
        finish_manager_spinner(spinner);

        if cli.interactive
            && matches!(run_mode, RunMode::Apply)
            && let Some(new_pins) = manager_ctx.take_pending_pins()
        {
            config.set_manager_pins(plugin.id(), new_pins);
            if let Err(err) = config.persist_manager_pins(plugin.id()) {
                eprintln!(
                    "error: failed to persist interactive pin updates after manager '{}': {err}",
                    plugin.id()
                );
                had_manager_failure = true;
            }
        }

        if let Err(err) = run_result {
            eprintln!("error: manager '{}' failed: {err}", plugin.id());
            if is_signal_termination(&err) {
                std::process::exit(130);
            }
            had_manager_failure = true;
        }
    }

    set_current_manager(None);

    if had_manager_failure {
        std::process::exit(1);
    }
}

fn is_signal_termination(err: &Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<util::process::CommandFailedError>()
            .is_some_and(util::process::CommandFailedError::was_signaled)
            || cause
                .downcast_ref::<interactive::InteractiveCancelled>()
                .is_some()
    })
}
