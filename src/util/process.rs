use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::Path;
use std::process::{Command, ExitStatus, Output};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;

use crate::ui::with_spinner_suspended;
use crate::util::env::non_empty_var;
use crate::util::logging;

const SKIP_MUTATING_COMMANDS_ENV: &str = "UPNOW_SKIP_MUTATING_COMMANDS";
const REQUIRE_MUTATION_MODE_ENV: &str = "UPNOW_REQUIRE_MUTATION_MODE";
pub const MUTATION_SKIP_NOTICE: &str = "mutating commands are skipped (safe mode)";
pub const MUTATION_ENABLE_NOTICE: &str = "real mutating commands are ENABLED";

static SKIP_MUTATING_COMMANDS: OnceLock<bool> = OnceLock::new();
static FORCE_SKIP_MUTATING_COMMANDS: AtomicBool = AtomicBool::new(false);
static MUTATION_MODE_NOTICE_EMITTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug)]
pub enum CmdStatus<'a> {
    Success,
    Allow(&'a [i32]),
    IgnoreStatus,
}

pub struct Cmd<'a> {
    program: OsString,
    args: Vec<OsString>,
    check: CmdStatus<'a>,
    is_mutation: bool,
}

impl Cmd<'_> {
    pub const fn mutating(mut self) -> Self {
        self.is_mutation = true;
        self
    }

    pub fn output(self) -> Result<CmdOutput> {
        execute_cmd(&self.program, &self.args, self.check, self.is_mutation)
    }
}

#[derive(Debug)]
pub struct CmdOutput {
    output: Output,
    display: String,
}

impl CmdOutput {
    pub fn stdout(&self) -> Result<&str> {
        let s = std::str::from_utf8(&self.output.stdout)
            .with_context(|| format!("{} stdout not UTF-8", self.display))?;
        Ok(s.trim())
    }

    pub fn stderr(&self) -> Result<&str> {
        let s = std::str::from_utf8(&self.output.stderr)
            .with_context(|| format!("{} stderr not UTF-8", self.display))?;
        Ok(s.trim())
    }

    pub fn json<T>(&self) -> Result<T>
    where
        T: DeserializeOwned,
    {
        serde_json::from_slice(&self.output.stdout)
            .with_context(|| format!("failed to parse JSON output from {}", self.display))
    }

    pub fn success(&self) -> bool {
        self.output.status.success()
    }

    pub fn code(&self) -> Option<i32> {
        self.output.status.code()
    }
}

pub fn run_cmd<P, I, A>(program: P, args: I, check: CmdStatus<'_>) -> Cmd<'_>
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

pub fn command_exists(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }

    let command_path = Path::new(trimmed);
    if command_path.components().count() > 1 {
        return is_executable_file(command_path);
    }

    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|path_dir| is_executable_file(&path_dir.join(trimmed)))
    })
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };

    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
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
    logging::on_command_start(&display, is_mutation);

    let started_at = Instant::now();

    let output = if is_mutation && skip_mutating_commands() {
        maybe_emit_mutation_mode_notice(true);
        Output {
            status: success_exit_status(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    } else {
        if is_mutation {
            maybe_emit_mutation_mode_notice(false);
        }

        match command.output() {
            Ok(output) => output,
            Err(err) => {
                logging::on_command_spawn_error(&display, is_mutation, &err);
                return Err(err).with_context(|| format!("failed to run {display}"));
            }
        }
    };
    let status_allowed = status_allowed(output.status, check);
    logging::on_command_finish(
        &display,
        &output,
        is_mutation,
        status_allowed,
        started_at.elapsed(),
    );

    let output = ensure_status(output, &display, check)?;
    Ok(CmdOutput { output, display })
}

fn skip_mutating_commands() -> bool {
    FORCE_SKIP_MUTATING_COMMANDS.load(Ordering::Relaxed)
        || *SKIP_MUTATING_COMMANDS.get_or_init(|| env_truthy(SKIP_MUTATING_COMMANDS_ENV))
}

pub fn mutating_commands_are_skipped() -> bool {
    skip_mutating_commands()
}

#[cfg(debug_assertions)]
pub fn set_debug_force_skip_mutating_commands(force: bool) {
    FORCE_SKIP_MUTATING_COMMANDS.store(force, Ordering::Relaxed);
}

#[cfg(not(debug_assertions))]
pub fn set_debug_force_skip_mutating_commands(_force: bool) {}

pub fn mutation_mode_notice_enabled() -> bool {
    cfg!(debug_assertions) || non_empty_var(REQUIRE_MUTATION_MODE_ENV).is_some()
}

pub fn validate_required_mutation_mode() -> Result<()> {
    let Some(raw) = non_empty_var(REQUIRE_MUTATION_MODE_ENV) else {
        return Ok(());
    };

    let required = raw.to_ascii_lowercase();
    let skipping = skip_mutating_commands();

    match required.as_str() {
        "skip" if skipping => Ok(()),
        "real" if !skipping => Ok(()),
        "skip" => anyhow::bail!(
            "{REQUIRE_MUTATION_MODE_ENV}=skip requires effective skip mode (set {SKIP_MUTATING_COMMANDS_ENV}=1 or --debug-no-mutate in debug builds)"
        ),
        "real" => anyhow::bail!(
            "{REQUIRE_MUTATION_MODE_ENV}=real requires effective real mode (set {SKIP_MUTATING_COMMANDS_ENV}=0 and disable --debug-no-mutate)"
        ),
        _ => anyhow::bail!("{REQUIRE_MUTATION_MODE_ENV} must be one of: skip, real (got '{raw}')"),
    }
}

fn maybe_emit_mutation_mode_notice(skipping: bool) {
    if !mutation_mode_notice_enabled() {
        return;
    }

    if MUTATION_MODE_NOTICE_EMITTED.swap(true, Ordering::Relaxed) {
        return;
    }

    with_spinner_suspended(|| {
        if skipping {
            eprintln!(
                "note: {MUTATION_SKIP_NOTICE}. set {SKIP_MUTATING_COMMANDS_ENV}=0 to execute real mutations (DANGEROUS)"
            );
        } else {
            eprintln!(
                "warning: {MUTATION_ENABLE_NOTICE} (set {SKIP_MUTATING_COMMANDS_ENV}=1 for safe mode)"
            );
        }
    });
}

fn env_truthy(name: &str) -> bool {
    non_empty_var(name).is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[cfg(unix)]
fn success_exit_status() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    ExitStatus::from_raw(0)
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
    let mut display = command.get_program().to_string_lossy().into_owned();
    for arg in command.get_args() {
        display.push(' ');
        display.push_str(arg.to_string_lossy().as_ref());
    }

    display
}

#[derive(Debug)]
pub struct CommandFailedError {
    command_display: String,
    status: ExitStatus,
    stderr: String,
}

impl CommandFailedError {
    pub fn was_signaled(&self) -> bool {
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
