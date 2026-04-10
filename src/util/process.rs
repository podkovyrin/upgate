use anyhow::{Context, Result};
use std::fmt;
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

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Err(anyhow::Error::new(CommandFailedError {
        command_display: command_display.to_string(),
        status: output.status,
        stderr,
    }))
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

#[derive(Debug)]
pub(crate) struct CommandFailedError {
    command_display: String,
    status: ExitStatus,
    stderr: String,
}

impl CommandFailedError {
    pub(crate) fn was_signaled(&self) -> bool {
        self.status.code().is_none()
    }
}

impl fmt::Display for CommandFailedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = exit_code_label(self.status);
        let detail = self.stderr.trim();

        if detail.is_empty() {
            write!(f, "{} failed (exit {code})", self.command_display)
        } else {
            write!(f, "{} failed (exit {code}): {detail}", self.command_display)
        }
    }
}

impl std::error::Error for CommandFailedError {}

fn exit_code_label(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => code.to_string(),
        None => "signal".to_string(),
    }
}
