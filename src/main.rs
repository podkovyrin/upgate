use anyhow::Result;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "brew-delay-upgrade")]
#[command(about = "Upgrade Homebrew packages older than a minimum release age")]
struct Cli {
    /// Print the upgrade plan only.
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Minimum age of a formula/cask definition commit (e.g. 12h, 7d).
    #[arg(long, default_value = "12h")]
    min_release_age: String,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let _cli = Cli::parse();
    Ok(())
}
