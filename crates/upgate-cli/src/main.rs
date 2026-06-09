use std::io::Write as _;

fn main() {
    match upgate_cli::run_from_env_with_report() {
        upgate_cli::CliRunResult::Completed(output) => {
            print!("{}", output.stdout);
            let _ = std::io::stdout().flush();
            print_command_log_dir(output.command_log_dir.as_deref());
        }
        upgate_cli::CliRunResult::Cancelled(output) => {
            print_command_log_dir(output.command_log_dir.as_deref());
        }
        upgate_cli::CliRunResult::Failed {
            error,
            command_log_dir,
        } => {
            eprintln!("{error}");
            print_command_log_dir(command_log_dir.as_deref());
            std::process::exit(if error.is_interruption() { 130 } else { 1 });
        }
    }
}

fn print_command_log_dir(command_log_dir: Option<&std::path::Path>) {
    if let Some(path) = command_log_dir {
        eprintln!("command logs: {}", path.display());
    }
}
