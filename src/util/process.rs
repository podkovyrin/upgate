use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use std::ffi::OsStr;
use std::fmt;
use std::process::{Command, ExitStatus, Output};

#[derive(Clone, Copy, Debug)]
pub(crate) enum RunCmd<'a> {
    Success,
    Allow(&'a [i32]),
    IgnoreStatus,
}

impl<'a> RunCmd<'a> {
    pub(crate) fn run<P, I, A>(self, program: P, args: I) -> Result<Output>
    where
        P: AsRef<OsStr>,
        I: IntoIterator<Item = A>,
        A: AsRef<OsStr>,
    {
        let (output, _display) = run_cmd_with_display(program, args, self)?;
        Ok(output)
    }

    pub(crate) fn text<P, I, A>(self, program: P, args: I) -> Result<String>
    where
        P: AsRef<OsStr>,
        I: IntoIterator<Item = A>,
        A: AsRef<OsStr>,
    {
        let (output, display) = run_cmd_with_display(program, args, self)?;
        String::from_utf8(output.stdout).with_context(|| format!("{display} output not UTF-8"))
    }

    pub(crate) fn json<P, I, A, T>(self, program: P, args: I) -> Result<T>
    where
        P: AsRef<OsStr>,
        I: IntoIterator<Item = A>,
        A: AsRef<OsStr>,
        T: DeserializeOwned,
    {
        let (output, display) = run_cmd_with_display(program, args, self)?;
        serde_json::from_slice(&output.stdout)
            .with_context(|| format!("failed to parse JSON output from {display}"))
    }
}

fn run_cmd_with_display<P, I, A>(program: P, args: I, check: RunCmd<'_>) -> Result<(Output, String)>
where
    P: AsRef<OsStr>,
    I: IntoIterator<Item = A>,
    A: AsRef<OsStr>,
{
    let mut command = Command::new(program);
    command.args(args);

    let display = command_display(&command);
    let output = command
        .output()
        .with_context(|| format!("failed to run {display}"))?;

    let output = ensure_status(output, &display, check)?;
    Ok((output, display))
}

fn ensure_status(output: Output, command_display: &str, check: RunCmd<'_>) -> Result<Output> {
    if status_allowed(output.status, check) {
        return Ok(output);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Err(anyhow::Error::new(CommandFailedError {
        command_display: command_display.to_string(),
        status: output.status,
        stderr,
    }))
}

fn status_allowed(status: ExitStatus, check: RunCmd<'_>) -> bool {
    match check {
        RunCmd::Success => status.success(),
        RunCmd::Allow(extra_codes) => {
            status.success()
                || status
                    .code()
                    .is_some_and(|code| extra_codes.contains(&code))
        }
        RunCmd::IgnoreStatus => true,
    }
}

fn command_display(command: &Command) -> String {
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
