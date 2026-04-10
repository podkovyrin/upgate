use anyhow::{Context, Result, bail};
use std::process::{Command, ExitStatus, Output};

pub(crate) fn run_command(mut command: Command) -> Result<Output> {
    let display = command_display(&command);
    command
        .output()
        .with_context(|| format!("failed to run {display}"))
}

pub(crate) fn run_command_checked(mut command: Command) -> Result<Output> {
    let display = command_display(&command);
    let output = command
        .output()
        .with_context(|| format!("failed to run {display}"))?;
    expect_success(output, &display)
}

pub(crate) fn run_command_checked_stdout(command: Command) -> Result<Vec<u8>> {
    Ok(run_command_checked(command)?.stdout)
}

pub(crate) fn expect_success(output: Output, command_display: &str) -> Result<Output> {
    if output.status.success() {
        return Ok(output);
    }

    let code = exit_code_label(output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();

    if detail.is_empty() {
        bail!("{command_display} failed (exit {code})");
    }

    bail!("{command_display} failed (exit {code}): {detail}")
}

pub(crate) fn command_display(command: &Command) -> String {
    let program = command.get_program().to_string_lossy().to_string();
    let args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect();

    if args.is_empty() {
        program
    } else {
        format!("{program} {}", args.join(" "))
    }
}

fn exit_code_label(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => code.to_string(),
        None => "signal".to_string(),
    }
}
