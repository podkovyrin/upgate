use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::process::{Command, ExitStatus, Output};
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;

use crate::{Env, InfraError};

pub const SKIP_MUTATING_COMMANDS_ENV: &str = "UPNOW_SKIP_MUTATING_COMMANDS";
pub const REQUIRE_MUTATION_MODE_ENV: &str = "UPNOW_REQUIRE_MUTATION_MODE";
pub const MUTATION_SKIP_NOTICE: &str = "mutating commands are skipped (safe mode)";
pub const MUTATION_ENABLE_NOTICE: &str = "real mutating commands are ENABLED";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandCheck {
    Success,
    Allow(Vec<i32>),
    IgnoreStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationMode {
    Real,
    Skip,
}

impl MutationMode {
    #[must_use]
    pub fn from_env(env: &Env) -> Self {
        if env.truthy(SKIP_MUTATING_COMMANDS_ENV) {
            Self::Skip
        } else {
            Self::Real
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    program: OsString,
    args: Vec<OsString>,
    is_mutation: bool,
}

impl CommandSpec {
    #[must_use]
    pub fn new<P, I, A>(program: P, args: I) -> Self
    where
        P: AsRef<OsStr>,
        I: IntoIterator<Item = A>,
        A: AsRef<OsStr>,
    {
        Self {
            program: program.as_ref().to_os_string(),
            args: args
                .into_iter()
                .map(|arg| arg.as_ref().to_os_string())
                .collect(),
            is_mutation: false,
        }
    }

    #[must_use]
    pub const fn mutating(mut self) -> Self {
        self.is_mutation = true;
        self
    }

    #[must_use]
    pub fn display(&self) -> String {
        let mut display = self.program.to_string_lossy().into_owned();
        for arg in &self.args {
            display.push(' ');
            display.push_str(arg.to_string_lossy().as_ref());
        }
        display
    }
}

#[derive(Debug, Clone)]
pub enum ProcessRunner {
    Real { mutation_mode: MutationMode },
    Fake(FakeProcess),
}

impl ProcessRunner {
    #[must_use]
    pub const fn new(mutation_mode: MutationMode) -> Self {
        Self::Real { mutation_mode }
    }

    #[must_use]
    pub fn from_env(env: &Env) -> Self {
        Self::new(MutationMode::from_env(env))
    }

    #[must_use]
    pub fn fake(responses: impl IntoIterator<Item = Result<CommandOutput, InfraError>>) -> Self {
        Self::Fake(FakeProcess::new(responses))
    }

    /// Runs a command and applies the requested status policy.
    ///
    /// # Errors
    ///
    /// Returns an error when the process cannot be spawned or when its exit
    /// status is not allowed by `check`.
    pub fn run(
        &self,
        spec: &CommandSpec,
        check: &CommandCheck,
    ) -> Result<CommandOutput, InfraError> {
        match self {
            Self::Real { mutation_mode } => run_real(*mutation_mode, spec, check),
            Self::Fake(fake) => fake.run(spec, check),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FakeProcess {
    responses: Arc<Mutex<VecDeque<Result<CommandOutput, InfraError>>>>,
    calls: Arc<Mutex<Vec<CommandSpec>>>,
}

impl FakeProcess {
    #[must_use]
    pub fn new(responses: impl IntoIterator<Item = Result<CommandOutput, InfraError>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[must_use]
    pub fn calls(&self) -> Vec<CommandSpec> {
        self.calls.lock().map_or_else(
            |poisoned| poisoned.into_inner().clone(),
            |calls| calls.clone(),
        )
    }

    fn run(&self, spec: &CommandSpec, check: &CommandCheck) -> Result<CommandOutput, InfraError> {
        let display = spec.display();
        self.calls
            .lock()
            .map_err(|err| InfraError::FakeProcessState {
                detail: err.to_string(),
            })?
            .push(spec.clone());
        let mut output = self
            .responses
            .lock()
            .map_err(|err| InfraError::FakeProcessState {
                detail: err.to_string(),
            })?
            .pop_front()
            .ok_or_else(|| InfraError::ProcessSpawn {
                command: display.clone(),
                detail: "fake process response queue was empty".to_owned(),
            })??;
        output.command_display.clone_from(&display);

        if status_allowed(output.status, check) {
            Ok(output)
        } else {
            Err(InfraError::CommandFailed(CommandFailure::new(
                display,
                output.status,
                output.stderr_string_lossy().into_owned(),
            )))
        }
    }
}

fn run_real(
    mutation_mode: MutationMode,
    spec: &CommandSpec,
    check: &CommandCheck,
) -> Result<CommandOutput, InfraError> {
    let display = spec.display();
    let output = if spec.is_mutation && mutation_mode == MutationMode::Skip {
        CommandOutput::from_skipped_mutation(success_exit_status(), display.clone())
    } else {
        let mut command = Command::new(&spec.program);
        command.args(&spec.args);
        CommandOutput::from_process_output(
            command.output().map_err(|err| InfraError::ProcessSpawn {
                command: display.clone(),
                detail: err.to_string(),
            })?,
            false,
            display.clone(),
        )
    };

    if status_allowed(output.status, check) {
        Ok(output)
    } else {
        Err(InfraError::CommandFailed(CommandFailure::new(
            display,
            output.status,
            output.stderr_string_lossy().into_owned(),
        )))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    skipped_mutation: bool,
    command_display: String,
}

impl CommandOutput {
    #[must_use]
    pub fn from_parts(
        status: ExitStatus,
        stdout: impl Into<Vec<u8>>,
        stderr: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            status,
            stdout: stdout.into(),
            stderr: stderr.into(),
            skipped_mutation: false,
            command_display: "<constructed output>".to_owned(),
        }
    }

    #[must_use]
    pub const fn status(&self) -> ExitStatus {
        self.status
    }

    #[must_use]
    pub const fn skipped_mutation(&self) -> bool {
        self.skipped_mutation
    }

    /// Decodes stdout as UTF-8.
    ///
    /// # Errors
    ///
    /// Returns an error when stdout is not valid UTF-8.
    pub fn stdout(&self) -> Result<&str, InfraError> {
        let text = std::str::from_utf8(&self.stdout).map_err(|err| InfraError::OutputUtf8 {
            command: self.command_display.clone(),
            stream: "stdout",
            detail: err.to_string(),
        })?;
        Ok(text.trim())
    }

    /// Decodes stderr as UTF-8.
    ///
    /// # Errors
    ///
    /// Returns an error when stderr is not valid UTF-8.
    pub fn stderr(&self) -> Result<&str, InfraError> {
        let text = std::str::from_utf8(&self.stderr).map_err(|err| InfraError::OutputUtf8 {
            command: self.command_display.clone(),
            stream: "stderr",
            detail: err.to_string(),
        })?;
        Ok(text.trim())
    }

    /// Decodes stdout as JSON.
    ///
    /// # Errors
    ///
    /// Returns an error when stdout is not valid JSON for `T`.
    pub fn json<T>(&self) -> Result<T, InfraError>
    where
        T: DeserializeOwned,
    {
        serde_json::from_slice(&self.stdout).map_err(|err| InfraError::JsonParse {
            command: self.command_display.clone(),
            detail: err.to_string(),
        })
    }

    #[must_use]
    pub fn stdout_string_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }

    #[must_use]
    pub fn stderr_string_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.stderr)
    }

    #[must_use]
    fn from_process_output(
        output: Output,
        skipped_mutation: bool,
        command_display: String,
    ) -> Self {
        Self {
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
            skipped_mutation,
            command_display,
        }
    }

    #[must_use]
    fn from_skipped_mutation(status: ExitStatus, command_display: String) -> Self {
        Self {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
            skipped_mutation: true,
            command_display,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFailure {
    command: String,
    status: ExitStatus,
    stderr: String,
}

impl CommandFailure {
    #[must_use]
    pub fn new(command: String, status: ExitStatus, stderr: String) -> Self {
        Self {
            command,
            status,
            stderr,
        }
    }

    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    #[must_use]
    pub const fn status(&self) -> ExitStatus {
        self.status
    }

    #[must_use]
    pub fn code(&self) -> Option<i32> {
        self.status.code()
    }

    #[must_use]
    pub fn was_signaled(&self) -> bool {
        self.status.code().is_none()
    }

    #[must_use]
    pub fn is_interruption(&self) -> bool {
        self.was_signaled()
    }

    #[must_use]
    pub fn stderr(&self) -> &str {
        &self.stderr
    }
}

impl fmt::Display for CommandFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |code| code.to_string());
        let detail = self.stderr.trim();

        if detail.is_empty() {
            write!(formatter, "{} failed (exit {code})", self.command)
        } else {
            write!(formatter, "{} failed (exit {code}): {detail}", self.command)
        }
    }
}

#[must_use]
pub fn status_allowed(status: ExitStatus, check: &CommandCheck) -> bool {
    match check {
        CommandCheck::Success => status.success(),
        CommandCheck::Allow(extra_codes) => {
            status.success()
                || status
                    .code()
                    .is_some_and(|code| extra_codes.contains(&code))
        }
        CommandCheck::IgnoreStatus => true,
    }
}

#[must_use]
pub fn command_exists(command: &str) -> bool {
    command_exists_in_env(command, &Env::real())
}

#[must_use]
pub fn command_exists_in_env(command: &str, env: &Env) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }

    let command_path = std::path::Path::new(trimmed);
    if command_path.components().count() > 1 {
        return is_executable_file(command_path);
    }

    env.var("PATH").is_some_and(|paths| {
        std::env::split_paths(std::ffi::OsStr::new(&paths))
            .any(|path_dir| is_executable_file(&path_dir.join(trimmed)))
    })
}

fn is_executable_file(path: &std::path::Path) -> bool {
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

#[cfg(unix)]
fn success_exit_status() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn success_exit_status() -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    ExitStatus::from_raw(0)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        CommandCheck, CommandOutput, CommandSpec, MutationMode, ProcessRunner,
        command_exists_in_env, success_exit_status,
    };
    use crate::{Env, InfraError};

    fn unique_path(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("upnow-infra-{name}-{nanos}"))
    }

    #[test]
    fn command_success_returns_stdout() {
        let runner = ProcessRunner::new(MutationMode::Real);
        let output = runner
            .run(
                &CommandSpec::new("sh", ["-c", "printf ' ok\\n'"]),
                &CommandCheck::Success,
            )
            .expect("command should succeed");

        assert_eq!(output.stdout().expect("utf8 stdout"), "ok");
        assert!(output.status().success());
    }

    #[test]
    fn stdout_utf8_errors_include_command_context() {
        let output = CommandOutput::from_parts(success_exit_status(), vec![0xff], "");
        let runner = ProcessRunner::fake([Ok(output)]);
        let spec = CommandSpec::new("tool", ["read"]);

        let output = runner
            .run(&spec, &CommandCheck::Success)
            .expect("fake response should be returned");
        let err = output.stdout().expect_err("invalid UTF-8 should fail");

        let InfraError::OutputUtf8 {
            command, stream, ..
        } = err
        else {
            panic!("expected UTF-8 error");
        };
        assert_eq!(command, "tool read");
        assert_eq!(stream, "stdout");
    }

    #[test]
    fn command_failure_reports_status_and_stderr() {
        let runner = ProcessRunner::new(MutationMode::Real);
        let err = runner
            .run(
                &CommandSpec::new("sh", ["-c", "printf ' problem\\n' >&2; exit 7"]),
                &CommandCheck::Success,
            )
            .expect_err("command should fail");

        let InfraError::CommandFailed(failure) = err else {
            panic!("expected command failure");
        };
        assert_eq!(failure.code(), Some(7));
        assert_eq!(failure.stderr(), " problem\n");
        assert!(!failure.was_signaled());
    }

    #[test]
    fn json_decodes_stdout_with_command_context_on_failure() {
        let runner = ProcessRunner::new(MutationMode::Real);
        let output = runner
            .run(
                &CommandSpec::new("sh", ["-c", "printf '{'"]),
                &CommandCheck::Success,
            )
            .expect("command should succeed");

        let err = output
            .json::<serde_json::Value>()
            .expect_err("invalid JSON should fail");

        let InfraError::JsonParse { command, .. } = err else {
            panic!("expected JSON parse error");
        };
        assert_eq!(command, "sh -c printf '{'");
    }

    #[test]
    fn allowed_exit_code_is_successful() {
        let runner = ProcessRunner::new(MutationMode::Real);
        let output = runner
            .run(
                &CommandSpec::new("sh", ["-c", "exit 7"]),
                &CommandCheck::Allow(vec![7]),
            )
            .expect("allowed status should pass");

        assert_eq!(output.status().code(), Some(7));
    }

    #[test]
    fn ignored_exit_status_is_successful() {
        let runner = ProcessRunner::new(MutationMode::Real);
        let output = runner
            .run(
                &CommandSpec::new("sh", ["-c", "exit 9"]),
                &CommandCheck::IgnoreStatus,
            )
            .expect("ignored status should pass");

        assert_eq!(output.status().code(), Some(9));
    }

    #[test]
    fn mutation_skip_does_not_spawn_command() {
        let path = unique_path("mutation-skip");
        let script = format!("printf changed > {}", path.display());
        let runner = ProcessRunner::new(MutationMode::Skip);
        let output = runner
            .run(
                &CommandSpec::new("sh", ["-c", script.as_str()]).mutating(),
                &CommandCheck::Success,
            )
            .expect("skipped mutation should be reported as success");

        assert!(output.status().success());
        assert!(output.skipped_mutation());
        assert!(!path.exists());
    }

    #[test]
    fn mutation_real_spawns_command() {
        let path = unique_path("mutation-real");
        let script = format!("printf changed > {}", path.display());
        let runner = ProcessRunner::new(MutationMode::Real);
        let output = runner
            .run(
                &CommandSpec::new("sh", ["-c", script.as_str()]).mutating(),
                &CommandCheck::Success,
            )
            .expect("real mutation should run");

        assert!(output.status().success());
        assert!(!output.skipped_mutation());
        assert_eq!(
            fs::read_to_string(&path).expect("mutation output file should exist"),
            "changed"
        );
        let _ = fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn signal_exit_is_classified_as_interruption() {
        let runner = ProcessRunner::new(MutationMode::Real);
        let err = runner
            .run(
                &CommandSpec::new("sh", ["-c", "kill -TERM $$"]),
                &CommandCheck::Success,
            )
            .expect_err("signal should fail");

        let InfraError::CommandFailed(failure) = err else {
            panic!("expected command failure");
        };
        assert_eq!(failure.code(), None);
        assert!(failure.is_interruption());
    }

    #[cfg(unix)]
    #[test]
    fn command_discovery_uses_supplied_env_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_path("command-path");
        fs::create_dir(&dir).expect("test command dir should be created");
        let command_path = dir.join("tool");
        fs::write(&command_path, "#!/bin/sh\n").expect("test command should be written");
        fs::set_permissions(&command_path, fs::Permissions::from_mode(0o755))
            .expect("test command should be executable");

        let env = Env::fixed([("PATH".to_owned(), dir.display().to_string())]);

        assert!(command_exists_in_env("tool", &env));
        assert!(!command_exists_in_env("missing-tool", &env));

        let _ = fs::remove_file(command_path);
        let _ = fs::remove_dir(dir);
    }
}
