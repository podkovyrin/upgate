mod brew;
mod bun;
mod cargo;
mod manager;
mod mise;
mod npm;
mod outcome;
mod pipx;
mod pnpm;
mod process;
mod timefmt;
mod timeparse;
mod uv;
mod yarn;

use anyhow::Result;
use clap::{Parser, Subcommand};
use manager::Manager;

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
pub(crate) struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Print the upgrade plan only.
    ///
    /// Mainly meaningful for `apply`; on `plan` this is always effectively true.
    #[arg(short = 'n', long, global = true)]
    pub(crate) dry_run: bool,

    /// Minimum age for brew formula/cask updates (e.g. 12h, 7d).
    #[arg(long, default_value = "12h", global = true)]
    pub(crate) min_release_age: String,

    /// Maximum concurrent checks.
    #[arg(long, default_value_t = 6, global = true)]
    pub(crate) max_parallel_checks: usize,

    /// Skip metadata update step for managers that support it.
    #[arg(long, global = true)]
    pub(crate) no_update: bool,

    /// Managers to run.
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        default_values_t = [Manager::Brew],
        global = true
    )]
    pub(crate) managers: Vec<Manager>,
}

fn main() {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Plan);

    let mut effective_cli = cli.clone();
    if matches!(command, Command::Plan) {
        effective_cli.dry_run = true;
    }

    if let Err(err) = run_selected_managers(&effective_cli) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run_selected_managers(cli: &Cli) -> Result<()> {
    if cli.managers.contains(&Manager::Brew) {
        brew::run(cli)?;
    }

    if cli.managers.contains(&Manager::Bun) {
        bun::run(cli)?;
    }

    if cli.managers.contains(&Manager::Cargo) {
        cargo::run(cli)?;
    }

    if cli.managers.contains(&Manager::Npm) {
        npm::run(cli)?;
    }

    if cli.managers.contains(&Manager::Yarn) {
        yarn::run(cli)?;
    }

    if cli.managers.contains(&Manager::Mise) {
        mise::run(cli)?;
    }

    if cli.managers.contains(&Manager::Pipx) {
        pipx::run(cli)?;
    }

    if cli.managers.contains(&Manager::Pnpm) {
        pnpm::run(cli)?;
    }

    if cli.managers.contains(&Manager::Uv) {
        uv::run(cli)?;
    }

    Ok(())
}
