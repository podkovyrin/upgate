use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;

use crate::{Env, InfraError, logging};

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
    pub const fn from_dry_run(dry_run: bool) -> Self {
        if dry_run { Self::Skip } else { Self::Real }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    program: OsString,
    args: Vec<OsString>,
    is_mutation: bool,
}

impl CommandSpec {
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
    pub const fn mutating(mut self) -> Self {
        self.is_mutation = true;
        self
    }
}

impl fmt::Display for CommandSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut display = self.program.to_string_lossy().into_owned();
        for arg in &self.args {
            display.push(' ');
            display.push_str(arg.to_string_lossy().as_ref());
        }
        formatter.write_str(&display)
    }
}

#[derive(Clone)]
pub struct ProcessRunner {
    kind: ProcessRunnerKind,
    command_start: Option<CommandStartListener>,
    interrupt_requested: Option<Arc<AtomicBool>>,
}

#[derive(Clone)]
enum ProcessRunnerKind {
    Real { mutation_mode: MutationMode },
    Fake(FakeProcess),
}

impl fmt::Debug for ProcessRunner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessRunner")
            .field("kind", &self.kind)
            .field("command_start", &self.command_start.is_some())
            .field("interrupt_requested", &self.interrupt_requested.is_some())
            .finish()
    }
}

impl fmt::Debug for ProcessRunnerKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Real { mutation_mode } => formatter
                .debug_struct("Real")
                .field("mutation_mode", mutation_mode)
                .finish(),
            Self::Fake(fake) => formatter.debug_tuple("Fake").field(fake).finish(),
        }
    }
}

#[derive(Clone)]
pub struct CommandStartEvent {
    pub command_display: String,
    pub is_mutation: bool,
}

type CommandStartListener = Arc<dyn Fn(CommandStartEvent) + Send + Sync + 'static>;

impl ProcessRunner {
    pub fn new(mutation_mode: MutationMode) -> Self {
        Self {
            kind: ProcessRunnerKind::Real { mutation_mode },
            command_start: None,
            interrupt_requested: None,
        }
    }
    pub fn fake(responses: impl IntoIterator<Item = Result<CommandOutput, InfraError>>) -> Self {
        Self {
            kind: ProcessRunnerKind::Fake(FakeProcess::new(responses)),
            command_start: None,
            interrupt_requested: None,
        }
    }
    pub fn with_command_start_listener(
        mut self,
        listener: impl Fn(CommandStartEvent) + Send + Sync + 'static,
    ) -> Self {
        self.command_start = Some(Arc::new(listener));
        self
    }
    /// Returns a runner that interrupts running real commands when the flag is set.
    ///
    /// On Unix, interruptible commands are started in their own process group and
    /// the owned process group is signaled. On non-Unix platforms, interruption is
    /// limited to the direct child process.
    pub fn with_interrupt_flag(mut self, interrupt_requested: Arc<AtomicBool>) -> Self {
        self.interrupt_requested = Some(interrupt_requested);
        self
    }
    pub const fn mutation_mode(&self) -> Option<MutationMode> {
        match &self.kind {
            ProcessRunnerKind::Real { mutation_mode } => Some(*mutation_mode),
            ProcessRunnerKind::Fake(_) => None,
        }
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
        let display = spec.to_string();
        if let Some(listener) = &self.command_start {
            listener(CommandStartEvent {
                command_display: display.clone(),
                is_mutation: spec.is_mutation,
            });
        }
        match &self.kind {
            ProcessRunnerKind::Real { mutation_mode } => run_real(
                *mutation_mode,
                spec,
                check,
                display,
                self.interrupt_requested.as_deref(),
            ),
            ProcessRunnerKind::Fake(fake) => fake.run(check, display),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FakeProcess {
    responses: Arc<Mutex<VecDeque<Result<CommandOutput, InfraError>>>>,
}

impl FakeProcess {
    pub fn new(responses: impl IntoIterator<Item = Result<CommandOutput, InfraError>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
        }
    }

    fn run(&self, check: &CommandCheck, display: String) -> Result<CommandOutput, InfraError> {
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
    display: String,
    interrupt_requested: Option<&AtomicBool>,
) -> Result<CommandOutput, InfraError> {
    logging::on_command_start(&display, spec.is_mutation);
    let started_at = Instant::now();
    let output = if spec.is_mutation && mutation_mode == MutationMode::Skip {
        CommandOutput::from_skipped_mutation(success_exit_status(), display.clone())
    } else {
        run_real_command(spec, &display, interrupt_requested).map_err(|err| {
            if matches!(err, InfraError::ProcessSpawn { .. }) {
                logging::on_command_spawn_error(
                    &display,
                    spec.is_mutation,
                    &io::Error::other(err.to_string()),
                );
            }
            err
        })?
    };

    let status_allowed = status_allowed(output.status, check);
    logging::on_command_finish(
        &display,
        output.status,
        &output.stdout,
        &output.stderr,
        spec.is_mutation,
        status_allowed,
        started_at.elapsed(),
    );

    if status_allowed {
        Ok(output)
    } else {
        Err(InfraError::CommandFailed(CommandFailure::new(
            display,
            output.status,
            output.stderr_string_lossy().into_owned(),
        )))
    }
}

fn run_real_command(
    spec: &CommandSpec,
    display: &str,
    interrupt_requested: Option<&AtomicBool>,
) -> Result<CommandOutput, InfraError> {
    if interrupt_requested.is_some_and(|interrupt| interrupt.load(Ordering::Relaxed)) {
        return Err(interrupted_command_error(display.to_owned()));
    }

    let mut command = Command::new(&spec.program);
    command.args(&spec.args);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    if interrupt_requested.is_some() {
        use std::os::unix::process::CommandExt;

        command.process_group(0);
    }

    let mut child = command.spawn().map_err(|err| InfraError::ProcessSpawn {
        command: display.to_owned(),
        detail: err.to_string(),
    })?;
    let stdout = read_child_stream(child.stdout.take(), display, "stdout");
    let stderr = read_child_stream(child.stderr.take(), display, "stderr");

    let status = loop {
        if let Some(status) = child.try_wait().map_err(|err| InfraError::ProcessSpawn {
            command: display.to_owned(),
            detail: format!("failed to wait for process: {err}"),
        })? {
            break status;
        }
        if interrupt_requested.is_some_and(|interrupt| interrupt.load(Ordering::Relaxed)) {
            interrupt_child(&mut child, display)?;
            break child.wait().map_err(|err| InfraError::ProcessSpawn {
                command: display.to_owned(),
                detail: format!("failed to wait for interrupted process: {err}"),
            })?;
        }
        thread::sleep(Duration::from_millis(25));
    };

    Ok(CommandOutput::from_parts_with_display(
        status,
        join_reader(stdout, display, "stdout")?,
        join_reader(stderr, display, "stderr")?,
        false,
        display.to_owned(),
    ))
}

fn read_child_stream(
    stream: Option<impl Read + Send + 'static>,
    display: &str,
    stream_name: &'static str,
) -> thread::JoinHandle<Result<Vec<u8>, InfraError>> {
    let display = display.to_owned();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let Some(mut stream) = stream else {
            return Ok(bytes);
        };
        stream
            .read_to_end(&mut bytes)
            .map_err(|err| InfraError::OutputUtf8 {
                command: display,
                stream: stream_name,
                detail: err.to_string(),
            })?;
        Ok(bytes)
    })
}

fn join_reader(
    reader: thread::JoinHandle<Result<Vec<u8>, InfraError>>,
    display: &str,
    stream: &'static str,
) -> Result<Vec<u8>, InfraError> {
    reader.join().map_err(|_| InfraError::ProcessSpawn {
        command: display.to_owned(),
        detail: format!("{stream} reader thread panicked"),
    })?
}

#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "Unix hard interrupt requires signaling an owned process group"
)]
fn interrupt_child(child: &mut std::process::Child, display: &str) -> Result<(), InfraError> {
    let pid = i32::try_from(child.id()).map_err(|err| InfraError::ProcessSpawn {
        command: display.to_owned(),
        detail: format!("child pid was not representable: {err}"),
    })?;
    let process_group = -pid;
    // SAFETY: `kill` is called with a process-group id derived from the child
    // process id returned by `std::process::Child`. No pointers are passed.
    let result = unsafe { libc::kill(process_group, libc::SIGTERM) };
    if result == 0 {
        return Ok(());
    }
    child.kill().map_err(|err| InfraError::ProcessSpawn {
        command: display.to_owned(),
        detail: format!("failed to interrupt process: {err}"),
    })
}

#[cfg(not(unix))]
fn interrupt_child(child: &mut std::process::Child, display: &str) -> Result<(), InfraError> {
    child.kill().map_err(|err| InfraError::ProcessSpawn {
        command: display.to_owned(),
        detail: format!(
            "failed to interrupt process: {err}; descendant cleanup is only implemented on Unix"
        ),
    })
}

fn interrupted_command_error(command: String) -> InfraError {
    InfraError::CommandFailed(CommandFailure::new(
        command,
        interrupted_exit_status(),
        "interrupted".to_owned(),
    ))
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
    pub const fn status(&self) -> ExitStatus {
        self.status
    }
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
    pub fn stdout_string_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }
    pub fn stderr_string_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.stderr)
    }
    const fn from_parts_with_display(
        status: ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        skipped_mutation: bool,
        command_display: String,
    ) -> Self {
        Self {
            status,
            stdout,
            stderr,
            skipped_mutation,
            command_display,
        }
    }
    const fn from_skipped_mutation(status: ExitStatus, command_display: String) -> Self {
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
    pub const fn new(command: String, status: ExitStatus, stderr: String) -> Self {
        Self {
            command,
            status,
            stderr,
        }
    }
    pub fn command(&self) -> &str {
        &self.command
    }
    pub const fn status(&self) -> ExitStatus {
        self.status
    }
    pub fn code(&self) -> Option<i32> {
        self.status.code()
    }
    pub fn was_signaled(&self) -> bool {
        self.status.code().is_none()
    }
    pub fn is_interruption(&self) -> bool {
        self.was_signaled()
    }
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

#[cfg(unix)]
fn interrupted_exit_status() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    ExitStatus::from_raw(libc::SIGTERM)
}

#[cfg(windows)]
fn interrupted_exit_status() -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    ExitStatus::from_raw(1)
}

#[cfg(not(any(unix, windows)))]
fn interrupted_exit_status() -> ExitStatus {
    success_exit_status()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn interruptible_process_runner_interrupts_running_command() {
        let interrupt = Arc::new(AtomicBool::new(false));
        let process = ProcessRunner::new(MutationMode::Real).with_interrupt_flag(interrupt.clone());
        let interrupter = thread::spawn({
            let interrupt = interrupt.clone();
            move || {
                thread::sleep(Duration::from_millis(100));
                interrupt.store(true, Ordering::Relaxed);
            }
        });

        let started_at = Instant::now();
        let err = process
            .run(
                &CommandSpec::new("/bin/sh", ["-c", "sleep 30"]),
                &CommandCheck::Success,
            )
            .expect_err("command should be interrupted");
        interrupter.join().expect("interrupter should finish");

        assert!(err.is_interruption());
        assert!(started_at.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn interruptible_process_runner_interrupts_descendant_process() {
        let pid_file = std::env::temp_dir().join(format!(
            "upgate-descendant-{}-{}.pid",
            std::process::id(),
            unique_nanos()
        ));
        let script = format!("sleep 30 & echo $! > {}; wait", pid_file.display());
        let interrupt = Arc::new(AtomicBool::new(false));
        let process = ProcessRunner::new(MutationMode::Real).with_interrupt_flag(interrupt.clone());
        let interrupter = thread::spawn({
            let interrupt = interrupt.clone();
            let pid_file = pid_file.clone();
            move || {
                let deadline = Instant::now() + Duration::from_secs(2);
                while !pid_file.exists() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
                interrupt.store(true, Ordering::Relaxed);
            }
        });

        let err = process
            .run(
                &CommandSpec::new("/bin/sh", [OsString::from("-c"), OsString::from(script)]),
                &CommandCheck::Success,
            )
            .expect_err("command should be interrupted");
        interrupter.join().expect("interrupter should finish");
        let descendant_pid = fs::read_to_string(&pid_file)
            .expect("descendant pid should be recorded")
            .trim()
            .parse::<i32>()
            .expect("descendant pid should be numeric");

        let descendant_exited =
            wait_until(Duration::from_secs(2), || !process_exists(descendant_pid));
        if !descendant_exited {
            kill_pid(descendant_pid);
        }
        let _ = fs::remove_file(pid_file);

        assert!(err.is_interruption());
        assert!(descendant_exited);
    }

    fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(25));
        }
        condition()
    }

    #[expect(
        unsafe_code,
        reason = "test verifies descendant process cleanup by pid"
    )]
    fn process_exists(pid: i32) -> bool {
        // SAFETY: `kill(pid, 0)` performs existence/error checking for the
        // numeric pid. No pointers are passed and no signal is delivered.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[expect(unsafe_code, reason = "best-effort test cleanup by pid")]
    fn kill_pid(pid: i32) {
        // SAFETY: Best-effort test cleanup for a numeric pid captured from the
        // child shell. No pointers are passed.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }

    fn unique_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    }
}
