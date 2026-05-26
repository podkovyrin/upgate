use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::Local;
use owo_colors::OwoColorize;

use crate::{Env, InfraError};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoggingOptions {
    pub debug_commands: bool,
    pub show_commands: bool,
    pub show_command_colors: bool,
}

struct Logger {
    session_dir: PathBuf,
    options: LoggingOptions,
    write_lock: Mutex<()>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();
static OPTIONS: OnceLock<LoggingOptions> = OnceLock::new();

/// Initializes command logging and returns the session directory.
///
/// # Errors
///
/// Returns a logging error when the legacy log directory cannot be resolved,
/// created, or written.
pub fn init_logging(options: LoggingOptions, env: &Env) -> Result<PathBuf, InfraError> {
    let _ = OPTIONS.set(options);

    if let Some(logger) = LOGGER.get() {
        return Ok(logger.session_dir.clone());
    }

    let base_dir = log_base_dir(env).ok_or_else(|| InfraError::Logging {
        detail: "could not resolve legacy log directory".to_owned(),
    })?;
    fs::create_dir_all(&base_dir).map_err(|err| InfraError::Logging {
        detail: format!(
            "failed to create log directory {}: {err}",
            base_dir.display()
        ),
    })?;
    let session_dir = base_dir.join(session_id());
    fs::create_dir_all(&session_dir).map_err(|err| InfraError::Logging {
        detail: format!(
            "failed to create log session directory {}: {err}",
            session_dir.display()
        ),
    })?;

    let log_path = session_dir.join("core.log");
    write_line_to_path(
        &log_path,
        "INFO",
        &format!(
            "logging initialized (debug_commands={}, show_commands={}, show_command_colors={})",
            options.debug_commands, options.show_commands, options.show_command_colors
        ),
    )
    .map_err(|err| InfraError::Logging {
        detail: format!("failed to write log file {}: {err}", log_path.display()),
    })?;

    let logger = Logger {
        session_dir: session_dir.clone(),
        options,
        write_lock: Mutex::new(()),
    };
    let _ = LOGGER.set(logger);

    Ok(session_dir)
}

pub fn on_command_start(command_display: &str, is_mutation: bool) {
    let options = OPTIONS.get().copied().unwrap_or_default();
    if options.show_commands {
        print_command_start(command_display, options, is_mutation);
    }

    let Some(logger) = LOGGER.get() else {
        return;
    };

    if is_mutation {
        let _ = write_line(
            logger,
            "INFO",
            &format!("mutation command start: {command_display}"),
        );
    } else if logger.options.debug_commands {
        let _ = write_line(
            logger,
            "DEBUG",
            &format!("command start: {command_display}"),
        );
    }
}

fn print_command_start(command_display: &str, options: LoggingOptions, is_mutation: bool) {
    if !options.show_command_colors {
        eprintln!("$ {command_display}");
    } else if is_mutation {
        eprintln!("{} {command_display}", "$".red());
    } else {
        eprintln!("{} {command_display}", "$".blue());
    }
}

pub fn on_command_spawn_error(command_display: &str, is_mutation: bool, err: &std::io::Error) {
    let Some(logger) = LOGGER.get() else {
        return;
    };

    let level = if is_mutation { "ERROR" } else { "WARN" };
    let _ = write_line(
        logger,
        level,
        &format!("failed to spawn command: {command_display}; error={err}"),
    );
}

pub fn on_command_finish(
    command_display: &str,
    status: ExitStatus,
    stdout: &[u8],
    stderr: &[u8],
    is_mutation: bool,
    status_allowed: bool,
    elapsed: Duration,
) {
    let Some(logger) = LOGGER.get() else {
        return;
    };

    let should_dump_streams = logger.options.debug_commands || is_mutation || !status_allowed;
    let level = if !status_allowed {
        "ERROR"
    } else if is_mutation {
        "INFO"
    } else {
        "DEBUG"
    };

    if should_dump_streams {
        let _ = write_line(
            logger,
            level,
            &format!(
                "command finish: {command_display}; exit={}; accepted={status_allowed}; elapsed_ms={}",
                exit_code_label(status.code()),
                elapsed.as_millis()
            ),
        );

        let _ = write_block(logger, "STDOUT", &String::from_utf8_lossy(stdout));
        let _ = write_block(logger, "STDERR", &String::from_utf8_lossy(stderr));
    }
}

fn log_base_dir(env: &Env) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return env
            .home_dir()
            .map(|home| home.join("Library").join("Logs").join("upnow"));
    }

    env.non_empty_path_var("XDG_STATE_HOME")
        .or_else(|| env.home_dir().map(|home| home.join(".local").join("state")))
        .map(|state_home| state_home.join("upnow").join("logs"))
}

fn session_id() -> String {
    format!(
        "{}-pid{}",
        Local::now().format("%Y-%m-%d_%H-%M-%S%.3f%z"),
        std::process::id()
    )
}

fn ts() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    format!("{}.{:03}", now.as_secs(), now.subsec_millis())
}

fn write_line(logger: &Logger, level: &str, message: &str) -> io::Result<()> {
    with_log_file(logger, |file| {
        writeln!(file, "[{}] [{}] {}", ts(), level, message)
    })
}

fn write_line_to_path(path: &std::path::Path, level: &str, message: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "[{}] [{}] {}", ts(), level, message)
}

fn write_block(logger: &Logger, stream: &str, content: &str) -> io::Result<()> {
    with_log_file(logger, |file| {
        writeln!(file, "[{}] [DEBUG] {stream} <<<", ts())?;
        if content.trim().is_empty() {
            writeln!(file, "(empty)")?;
        } else {
            for line in content.lines() {
                writeln!(file, "{line}")?;
            }
        }
        writeln!(file, "[{}] [DEBUG] >>>", ts())
    })
}

fn with_log_file(
    logger: &Logger,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let _guard = lock_or_recover(&logger.write_lock);
    let path = logger.session_dir.join("core.log");
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    write(&mut file)
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn exit_code_label(code: Option<i32>) -> String {
    code.map_or_else(|| "signal".to_owned(), |code| code.to_string())
}
