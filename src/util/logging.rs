use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LoggingOptions {
    pub(crate) debug_commands: bool,
    pub(crate) show_commands: bool,
}

struct Logger {
    session_dir: PathBuf,
    options: LoggingOptions,
    current_manager: Mutex<String>,
    write_lock: Mutex<()>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

pub(crate) fn init_logging(options: LoggingOptions) -> Result<PathBuf> {
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
        current_manager: Mutex::new("core".to_string()),
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

pub(crate) fn session_dir() -> Option<PathBuf> {
    LOGGER.get().map(|logger| logger.session_dir.clone())
}

pub(crate) fn set_current_manager(manager: Option<&str>) {
    let Some(logger) = LOGGER.get() else {
        return;
    };

    let mut slot = logger
        .current_manager
        .lock()
        .expect("logger manager mutex poisoned");
    *slot = manager.unwrap_or("core").to_string();
}

pub(crate) fn on_command_start(command_display: &str, is_mutation: bool) {
    let Some(logger) = LOGGER.get() else {
        return;
    };

    let manager = current_manager(logger);

    if logger.options.show_commands {
        crate::ui::with_spinner_suspended(|| {
            eprintln!("$ {command_display}");
        });
    }

    if is_mutation {
        write_line(
            logger,
            &manager,
            "INFO",
            &format!("mutation command start: {command_display}"),
        );
    } else if logger.options.debug_commands {
        write_line(
            logger,
            &manager,
            "DEBUG",
            &format!("command start: {command_display}"),
        );
    }
}

pub(crate) fn on_command_spawn_error(
    command_display: &str,
    is_mutation: bool,
    err: &std::io::Error,
) {
    let Some(logger) = LOGGER.get() else {
        return;
    };

    let manager = current_manager(logger);
    let level = if is_mutation { "ERROR" } else { "WARN" };

    write_line(
        logger,
        &manager,
        level,
        &format!("failed to spawn command: {command_display}; error={err}"),
    );
}

pub(crate) fn on_command_finish(
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
            &manager,
            level,
            &format!(
                "command finish: {command_display}; exit={raw_code}; accepted={status_allowed}; elapsed_ms={}",
                elapsed.as_millis()
            ),
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        write_block(logger, &manager, "STDOUT", &stdout);
        write_block(logger, &manager, "STDERR", &stderr);
    }
}

pub(crate) fn is_mutating_command(program: &OsStr, args: &[OsString]) -> bool {
    let program_name = Path::new(program)
        .file_name()
        .unwrap_or(program)
        .to_string_lossy()
        .to_ascii_lowercase();

    let args_lower: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().to_ascii_lowercase())
        .collect();

    let first = args_lower.first().map(String::as_str);

    match program_name.as_str() {
        "brew" => matches!(
            first,
            Some("update")
                | Some("update-reset")
                | Some("upgrade")
                | Some("install")
                | Some("uninstall")
                | Some("remove")
                | Some("tap")
        ),
        "npm" => args_lower.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "update" | "install" | "uninstall" | "remove" | "add"
            )
        }),
        "yarn" => args_lower
            .iter()
            .any(|arg| matches!(arg.as_str(), "add" | "remove" | "upgrade" | "up")),
        "pnpm" => args_lower.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "add" | "install" | "update" | "up" | "remove" | "uninstall"
            )
        }),
        "bun" => args_lower.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "update" | "add" | "install" | "remove" | "uninstall"
            )
        }),
        "cargo" => matches!(first, Some("install") | Some("uninstall")),
        "pipx" => matches!(
            first,
            Some("install")
                | Some("upgrade")
                | Some("upgrade-all")
                | Some("uninstall")
                | Some("reinstall")
                | Some("inject")
                | Some("uninject")
        ),
        "uv" => {
            let is_dry_run = args_lower.iter().any(|arg| arg == "--dry-run");
            starts_with_args(&args_lower, &["tool", "install"])
                || starts_with_args(&args_lower, &["tool", "upgrade"])
                || starts_with_args(&args_lower, &["tool", "uninstall"])
                || (starts_with_args(&args_lower, &["pip", "install"]) && !is_dry_run)
        }
        "go" => first == Some("install"),
        "gem" => matches!(first, Some("install") | Some("update") | Some("uninstall")),
        "dotnet" => {
            starts_with_args(&args_lower, &["tool", "install"])
                || starts_with_args(&args_lower, &["tool", "update"])
                || starts_with_args(&args_lower, &["tool", "uninstall"])
        }
        "mise" => first == Some("upgrade") && !args_lower.iter().any(|arg| arg == "--dry-run"),
        _ => false,
    }
}

fn starts_with_args(args: &[String], prefix: &[&str]) -> bool {
    args.len() >= prefix.len()
        && prefix
            .iter()
            .enumerate()
            .all(|(idx, part)| args.get(idx).is_some_and(|arg| arg == part))
}

fn write_line(logger: &Logger, manager: &str, level: &str, message: &str) {
    let _guard = logger
        .write_lock
        .lock()
        .expect("logger write mutex poisoned");
    let path = logger
        .session_dir
        .join(format!("{}.log", sanitize_manager(manager)));
    let mut file = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => file,
        Err(_) => return,
    };

    let _ = writeln!(file, "[{}] [{}] {}", ts(), level, message);
}

fn write_block(logger: &Logger, manager: &str, stream: &str, content: &str) {
    let _guard = logger
        .write_lock
        .lock()
        .expect("logger write mutex poisoned");
    let path = logger
        .session_dir
        .join(format!("{}.log", sanitize_manager(manager)));
    let mut file = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => file,
        Err(_) => return,
    };

    let _ = writeln!(file, "[{}] [DEBUG] {stream} <<<", ts());
    if content.trim().is_empty() {
        let _ = writeln!(file, "(empty)");
    } else {
        for line in content.lines() {
            let _ = writeln!(file, "{line}");
        }
    }
    let _ = writeln!(file, "[{}] [DEBUG] >>>", ts());
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

fn current_manager(logger: &Logger) -> String {
    logger
        .current_manager
        .lock()
        .expect("logger manager mutex poisoned")
        .clone()
}

fn log_base_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").ok()?;
        let trimmed = home.trim();
        if trimmed.is_empty() {
            return None;
        }

        return Some(
            PathBuf::from(trimmed)
                .join("Library")
                .join("Logs")
                .join("upnow"),
        );
    }

    xdg_state_home().map(|p| p.join("upnow").join("logs"))
}

fn xdg_state_home() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("XDG_STATE_HOME") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }

    let home = std::env::var("HOME").ok()?;
    let trimmed = home.trim();
    if trimmed.is_empty() {
        return None;
    }

    Some(PathBuf::from(trimmed).join(".local").join("state"))
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
    match code {
        Some(code) => code.to_string(),
        None => "signal".to_string(),
    }
}
