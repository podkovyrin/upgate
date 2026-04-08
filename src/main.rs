mod brew;
mod mise;
mod npm;
mod pipx;

use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Manager {
    Brew,
    Npm,
    Mise,
    Pipx,
}

#[derive(Debug, Parser)]
#[command(name = "brew-delay-upgrade")]
#[command(about = "Delay-aware global package upgrades")]
pub(crate) struct Cli {
    /// Print the upgrade plan only.
    #[arg(short = 'n', long)]
    pub(crate) dry_run: bool,

    /// Minimum age for brew formula/cask updates (e.g. 12h, 7d).
    #[arg(long, default_value = "12h")]
    pub(crate) min_release_age: String,

    /// Maximum concurrent checks.
    #[arg(long, default_value_t = 6)]
    pub(crate) max_parallel_checks: usize,

    /// Skip metadata update step for managers that support it.
    #[arg(long)]
    pub(crate) no_update: bool,

    /// Managers to run.
    #[arg(long, value_enum, value_delimiter = ',', default_values_t = [Manager::Brew])]
    pub(crate) managers: Vec<Manager>,
}

fn main() {
    let cli = Cli::parse();

    let result = (|| -> anyhow::Result<()> {
        if cli.managers.contains(&Manager::Brew) {
            brew::run(&cli)?;
        }

        if cli.managers.contains(&Manager::Npm) {
            npm::run(&cli)?;
        }

        if cli.managers.contains(&Manager::Mise) {
            mise::run(&cli)?;
        }

        if cli.managers.contains(&Manager::Pipx) {
            pipx::run(&cli)?;
        }

        Ok(())
    })();

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
