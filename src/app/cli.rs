use clap::{Parser, Subcommand};

use crate::managers::RunMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Compute and print intended updates (non-mutating).
    Plan,
    /// Apply updates using manager-native upgrade commands.
    ///
    /// Note: Set `UPNOW_SKIP_MUTATING_COMMANDS=1` to force safe non-mutating mode.
    Apply,
    /// List installed package/tool versions across managers.
    Scan,
}

impl Command {
    pub const fn run_mode(self) -> RunMode {
        match self {
            Self::Plan => RunMode::Plan,
            Self::Apply => RunMode::Apply,
            Self::Scan => RunMode::Scan,
        }
    }
}

#[derive(Debug, Clone, Parser)]
#[command(name = "upnow")]
#[command(about = "Delay-aware global package upgrades")]
#[command(version)]
#[allow(clippy::struct_excessive_bools)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Maximum concurrent checks.
    #[arg(long, default_value_t = 6, global = true)]
    pub max_parallel_checks: usize,

    /// Managers to run (comma-separated manager IDs).
    #[arg(long, value_delimiter = ',', global = true)]
    pub managers: Vec<String>,

    /// Override config values (repeatable), format: <manager>.<key>=<value>
    #[arg(long, short = 'S', global = true)]
    pub set: Vec<String>,

    /// Disable ANSI color output.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Force plain output (no color, no Unicode symbols).
    #[arg(long, global = true)]
    pub plain: bool,

    /// Show additional metadata in outcome lines.
    #[arg(long, global = true)]
    pub verbose: bool,

    /// Persist full command debug logs (stdout/stderr + timing) under XDG state.
    #[arg(long, global = true)]
    pub debug_commands: bool,

    /// Print each command to stderr before execution.
    #[arg(long, visible_alias = "print-commands", global = true)]
    pub show_commands: bool,

    /// Prompt per manager to select which updates to apply.
    #[arg(long, global = true)]
    pub interactive: bool,

    /// Debug-only: force non-mutating behavior for mutating commands.
    #[cfg(debug_assertions)]
    #[arg(long, global = true)]
    pub debug_no_mutate: bool,
}

impl Cli {
    pub fn run_mode(&self) -> RunMode {
        self.command.unwrap_or(Command::Plan).run_mode()
    }

    pub const fn debug_no_mutate(&self) -> bool {
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
