mod config;
mod manager;
mod managers;
mod outcome;
mod util;

use clap::{Parser, Subcommand};
use config::UpnowConfig;
use manager::{RunMode, all_plugins, build_ctx_for_plugin, resolve_selected_plugins};

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
}

fn main() {
    let cli = Cli::parse();
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
        let manager_ctx = match build_ctx_for_plugin(plugin, run_mode, cli.max_parallel_checks, &config)
        {
            Ok(ctx) => ctx,
            Err(err) => {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        };

        if let Err(err) = plugin.run(&manager_ctx) {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }
}
