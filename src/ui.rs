use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressFinish, ProgressStyle};

use crate::managers::RunMode;

#[derive(Debug, Clone, Copy)]
enum OutputMode {
    Plain,
    Styled { color: bool },
}

#[derive(Debug, Clone, Copy)]
pub struct OutputTheme {
    mode: OutputMode,
    pub verbose: bool,
}

impl OutputTheme {
    fn from_options(options: ThemeOptions) -> Self {
        let stdout_is_tty = std::io::stdout().is_terminal();
        let plain = options.plain_flag || !stdout_is_tty;

        let no_color_env = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let term_is_dumb = std::env::var("TERM").is_ok_and(|v| v == "dumb");

        let mode = if plain {
            OutputMode::Plain
        } else {
            let color = !options.no_color_flag && !no_color_env && !term_is_dumb && stdout_is_tty;
            OutputMode::Styled { color }
        };

        Self {
            mode,
            verbose: options.verbose_flag,
        }
    }

    pub const fn plain(self) -> bool {
        matches!(self.mode, OutputMode::Plain)
    }

    pub const fn color(self) -> bool {
        match self.mode {
            OutputMode::Plain => false,
            OutputMode::Styled { color } => color,
        }
    }

    #[cfg(test)]
    pub(crate) const fn test_plain(verbose: bool) -> Self {
        Self {
            mode: OutputMode::Plain,
            verbose,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ThemeOptions {
    plain_flag: bool,
    no_color_flag: bool,
    verbose_flag: bool,
}

static OUTPUT_THEME: OnceLock<OutputTheme> = OnceLock::new();
static ACTIVE_SPINNER: OnceLock<Mutex<Option<ProgressBar>>> = OnceLock::new();
static TERMINAL_OUTPUT_SUPPRESSED: AtomicBool = AtomicBool::new(false);

pub fn init_output_theme(plain_flag: bool, no_color_flag: bool, verbose_flag: bool) {
    let options = ThemeOptions {
        plain_flag,
        no_color_flag,
        verbose_flag,
    };
    let theme = OutputTheme::from_options(options);
    let _ = OUTPUT_THEME.set(theme);
}

pub fn output_theme() -> OutputTheme {
    *OUTPUT_THEME.get_or_init(|| {
        OutputTheme::from_options(ThemeOptions {
            plain_flag: false,
            no_color_flag: false,
            verbose_flag: false,
        })
    })
}

pub struct ManagerSpinner(Option<ProgressBar>);

fn active_spinner_slot() -> &'static Mutex<Option<ProgressBar>> {
    ACTIVE_SPINNER.get_or_init(|| Mutex::new(None))
}

fn lock_active_spinner() -> MutexGuard<'static, Option<ProgressBar>> {
    active_spinner_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

pub fn start_manager_spinner(manager: &str, run_mode: RunMode) -> ManagerSpinner {
    let theme = output_theme();
    if terminal_output_suppressed() || theme.plain() || !std::io::stderr().is_terminal() {
        return ManagerSpinner(None);
    }

    let pb = ProgressBar::new_spinner().with_finish(ProgressFinish::AndClear);
    pb.set_style(spinner_style(theme.color()));
    pb.set_message(format!("{} {manager}...", run_mode.action_label()));
    pb.enable_steady_tick(Duration::from_millis(90));

    *lock_active_spinner() = Some(pb.clone());

    ManagerSpinner(Some(pb))
}

fn spinner_style(color: bool) -> ProgressStyle {
    let (template, ticks): (&str, &[&str]) = if color {
        (
            "{spinner:.cyan} {msg}",
            &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
        )
    } else {
        ("{spinner} {msg}", &["-", "\\", "|", "/"])
    };

    ProgressStyle::with_template(template)
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
        .tick_strings(ticks)
}

pub fn with_spinner_suspended<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let spinner = lock_active_spinner().clone();

    if let Some(pb) = spinner {
        pb.suspend(f)
    } else {
        f()
    }
}

pub fn finish_manager_spinner(spinner: ManagerSpinner) {
    if let Some(pb) = spinner.0 {
        *lock_active_spinner() = None;
        pb.finish_and_clear();
    }
}

pub fn terminal_output_suppressed() -> bool {
    TERMINAL_OUTPUT_SUPPRESSED.load(Ordering::Relaxed)
}

pub struct TerminalOutputSuppression {
    previous: bool,
}

impl TerminalOutputSuppression {
    pub fn enter() -> Self {
        let previous = TERMINAL_OUTPUT_SUPPRESSED.swap(true, Ordering::Relaxed);
        Self { previous }
    }
}

impl Drop for TerminalOutputSuppression {
    fn drop(&mut self) {
        TERMINAL_OUTPUT_SUPPRESSED.store(self.previous, Ordering::Relaxed);
    }
}
