use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::process::{Command, ExitStatus, Output};
use std::time::Instant;

#[derive(Clone, Copy, Debug)]
pub(crate) enum CmdStatus<'a> {
    Success,
    Allow(&'a [i32]),
    IgnoreStatus,
}

pub(crate) struct Cmd<'a> {
    program: OsString,
    args: Vec<OsString>,
    check: CmdStatus<'a>,
    is_mutation: bool,
}

impl Cmd<'_> {
    pub(crate) fn mutating(mut self) -> Self {
        self.is_mutation = true;
        self
    }

    pub(crate) fn output(self) -> Result<CmdOutput> {
        execute_cmd(&self.program, &self.args, self.check, self.is_mutation)
    }
}

#[derive(Debug)]
pub(crate) struct CmdOutput {
    output: Output,
    display: String,
}

impl CmdOutput {
    pub(crate) fn stdout(&self) -> Result<&str> {
        let s = std::str::from_utf8(&self.output.stdout)
            .with_context(|| format!("{} stdout not UTF-8", self.display))?;
        Ok(s.trim())
    }

    pub(crate) fn stderr(&self) -> Result<&str> {
        let s = std::str::from_utf8(&self.output.stderr)
            .with_context(|| format!("{} stderr not UTF-8", self.display))?;
        Ok(s.trim())
    }

    pub(crate) fn json<T>(&self) -> Result<T>
    where
        T: DeserializeOwned,
    {
        serde_json::from_slice(&self.output.stdout)
            .with_context(|| format!("failed to parse JSON output from {}", self.display))
    }

    pub(crate) fn success(&self) -> bool {
        self.output.status.success()
    }

    pub(crate) fn code(&self) -> Option<i32> {
        self.output.status.code()
    }
}

pub(crate) fn run_cmd<P, I, A>(program: P, args: I, check: CmdStatus<'_>) -> Cmd<'_>
where
    P: AsRef<OsStr>,
    I: IntoIterator<Item = A>,
    A: AsRef<OsStr>,
{
    Cmd {
        program: program.as_ref().to_os_string(),
        args: args
            .into_iter()
            .map(|arg| arg.as_ref().to_os_string())
            .collect(),
        check,
        is_mutation: false,
    }
}

fn execute_cmd(
    program: &OsStr,
    args: &[OsString],
    check: CmdStatus<'_>,
    is_mutation: bool,
) -> Result<CmdOutput> {
    let mut command = Command::new(program);
    command.args(args);

    let display = command_display(&command);
    crate::util::logging::on_command_start(&display, is_mutation);

    let started_at = Instant::now();
    let output = match command.output() {
        Ok(output) => output,
        Err(err) => {
            crate::util::logging::on_command_spawn_error(&display, is_mutation, &err);
            return Err(err).with_context(|| format!("failed to run {display}"));
        }
    };
    let status_allowed = status_allowed(output.status, check);
    crate::util::logging::on_command_finish(
        &display,
        &output,
        is_mutation,
        status_allowed,
        started_at.elapsed(),
    );

    let output = ensure_status(output, &display, check)?;
    Ok(CmdOutput { output, display })
}

fn ensure_status(output: Output, command_display: &str, check: CmdStatus<'_>) -> Result<Output> {
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

fn status_allowed(status: ExitStatus, check: CmdStatus<'_>) -> bool {
    match check {
        CmdStatus::Success => status.success(),
        CmdStatus::Allow(extra_codes) => {
            status.success()
                || status
                    .code()
                    .is_some_and(|code| extra_codes.contains(&code))
        }
        CmdStatus::IgnoreStatus => true,
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
    status
        .code()
        .map_or_else(|| "signal".to_string(), |code| code.to_string())
}
