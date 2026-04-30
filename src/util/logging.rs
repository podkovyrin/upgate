use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Output;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use owo_colors::OwoColorize;

use crate::ui::{output_theme, terminal_output_suppressed, with_spinner_suspended};
use crate::util::env::{home_dir, non_empty_path_var};
use crate::util::text::is_blank;

#[derive(Debug, Clone, Copy, Default)]
pub struct LoggingOptions {
    pub debug_commands: bool,
    pub show_commands: bool,
}

struct Logger {
    session_dir: PathBuf,
    options: LoggingOptions,
    current_manager: Mutex<&'static str>,
    write_lock: Mutex<()>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

pub fn init_logging(options: LoggingOptions) -> Result<PathBuf> {
    if let Some(logger) = LOGGER.get() {
        return Ok(logger.session_dir.clone());
    }

    let base_dir = log_base_dir().context("failed to resolve log directory")?;

    fs::create_dir_all(&base_dir)
        .with_context(|| format!("failed to create log directory {}", base_dir.display()))?;

    let session_dir = base_dir.join(session_id());
    fs::create_dir_all(&session_dir)
        .with_context(|| format!("failed to create log session dir {}", session_dir.display()))?;

    let logger = Logger {
        session_dir,
        options,
        current_manager: Mutex::new("core"),
        write_lock: Mutex::new(()),
    };

    let _ = LOGGER.set(logger);

    let logger = LOGGER
        .get()
        .context("failed to initialize logger singleton")?;

    write_line(
        logger,
        "core",
        "INFO",
        &format!(
            "logging initialized (debug_commands={}, show_commands={})",
            logger.options.debug_commands, logger.options.show_commands
        ),
    );

    Ok(logger.session_dir.clone())
}

pub fn session_dir() -> Option<PathBuf> {
    LOGGER.get().map(|logger| logger.session_dir.clone())
}

pub fn set_current_manager(manager: Option<&'static str>) {
    let Some(logger) = LOGGER.get() else {
        return;
    };

    *lock_or_recover(&logger.current_manager) = manager.unwrap_or("core");
}

pub fn on_command_start(command_display: &str, is_mutation: bool) {
    let Some(logger) = LOGGER.get() else {
        return;
    };

    let manager = current_manager(logger);

    if logger.options.show_commands && !terminal_output_suppressed() {
        with_spinner_suspended(|| {
            if output_theme().color() {
                if is_mutation {
                    eprintln!("{} {command_display}", "$".bright_red());
                } else {
                    eprintln!("{} {command_display}", "$".cyan());
                }
            } else {
                eprintln!("$ {command_display}");
            }
        });
    }

    if is_mutation {
        write_line(
            logger,
            manager,
            "INFO",
            &format!("mutation command start: {command_display}"),
        );
    } else if logger.options.debug_commands {
        write_line(
            logger,
            manager,
            "DEBUG",
            &format!("command start: {command_display}"),
        );
    }
}

pub fn on_command_spawn_error(command_display: &str, is_mutation: bool, err: &std::io::Error) {
    let Some(logger) = LOGGER.get() else {
        return;
    };

    let manager = current_manager(logger);
    let level = if is_mutation { "ERROR" } else { "WARN" };

    write_line(
        logger,
        manager,
        level,
        &format!("failed to spawn command: {command_display}; error={err}"),
    );
}

pub fn on_command_finish(
    command_display: &str,
    output: &Output,
    is_mutation: bool,
    status_allowed: bool,
    elapsed: Duration,
) {
    let Some(logger) = LOGGER.get() else {
        return;
    };

    let manager = current_manager(logger);
    let raw_code = exit_code_label(output.status.code());
    let should_dump_streams = logger.options.debug_commands || is_mutation || !status_allowed;

    let level = if !status_allowed {
        "ERROR"
    } else if is_mutation {
        "INFO"
    } else {
        "DEBUG"
    };

    if should_dump_streams {
        write_line(
            logger,
            manager,
            level,
            &format!(
                "command finish: {command_display}; exit={raw_code}; accepted={status_allowed}; elapsed_ms={}",
                elapsed.as_millis()
            ),
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        write_block(logger, manager, "STDOUT", &stdout);
        write_block(logger, manager, "STDERR", &stderr);
    }
}

pub fn log_warning(message: impl AsRef<str>) {
    let Some(logger) = LOGGER.get() else {
        return;
    };

    write_line(logger, current_manager(logger), "WARN", message.as_ref());
}

fn write_line(logger: &Logger, manager: &str, level: &str, message: &str) {
    with_log_file(logger, manager, |file| {
        let _ = writeln!(file, "[{}] [{}] {}", ts(), level, message);
    });
}

fn write_block(logger: &Logger, manager: &str, stream: &str, content: &str) {
    with_log_file(logger, manager, |file| {
        let _ = writeln!(file, "[{}] [DEBUG] {stream} <<<", ts());
        if is_blank(content) {
            let _ = writeln!(file, "(empty)");
        } else {
            for line in content.lines() {
                let _ = writeln!(file, "{line}");
            }
        }
        let _ = writeln!(file, "[{}] [DEBUG] >>>", ts());
    });
}

fn with_log_file(logger: &Logger, manager: &str, write: impl FnOnce(&mut File)) {
    let _guard = lock_or_recover(&logger.write_lock);

    let path = logger
        .session_dir
        .join(format!("{}.log", sanitize_manager(manager)));

    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };

    write(&mut file);
}

fn sanitize_manager(manager: &str) -> String {
    manager
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn current_manager(logger: &Logger) -> &'static str {
    *lock_or_recover(&logger.current_manager)
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn log_base_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return home_dir().map(|home| home.join("Library").join("Logs").join("upnow"));
    }

    xdg_state_home().map(|state_home| state_home.join("upnow").join("logs"))
}

fn xdg_state_home() -> Option<PathBuf> {
    non_empty_path_var("XDG_STATE_HOME")
        .or_else(|| home_dir().map(|home| home.join(".local").join("state")))
}

fn session_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    format!("{}-{}", now.as_secs(), std::process::id())
}

fn ts() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    format!("{}.{:03}", now.as_secs(), now.subsec_millis())
}

fn exit_code_label(code: Option<i32>) -> String {
    code.map_or_else(|| "signal".to_string(), |code| code.to_string())
}
