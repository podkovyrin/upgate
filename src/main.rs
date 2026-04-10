mod config;
mod manager;
mod managers;
mod outcome;
mod ui;
mod util;

use clap::{Parser, Subcommand};
use config::UpnowConfig;
use manager::{RunMode, all_plugins, build_ctx_for_plugin, resolve_selected_plugins};
use ui::{finish_manager_spinner, init_output_theme, start_manager_spinner};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
enum Command {
    /// Compute and print intended updates (non-mutating).
    Plan,
    /// Apply updates using manager-native upgrade commands.
    Apply,
}

#[derive(Debug, Clone, Parser)]
#[command(name = "upnow")]
#[command(about = "Delay-aware global package upgrades")]
#[command(version)]
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
}

fn main() {
    let cli = Cli::parse();
    init_output_theme(cli.plain, cli.no_color, cli.verbose);
    let run_mode = match cli.command.unwrap_or(Command::Plan) {
        Command::Plan => RunMode::Plan,
        Command::Apply => RunMode::Apply,
    };

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

    for plugin in selected_plugins {
        let manager_ctx =
            match build_ctx_for_plugin(plugin, run_mode, cli.max_parallel_checks, &config) {
                Ok(ctx) => ctx,
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            };

        let spinner = start_manager_spinner(plugin.id(), run_mode);
        let run_result = plugin.run(&manager_ctx);
        finish_manager_spinner(spinner);

        if let Err(err) = run_result {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }
}
