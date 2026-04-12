use crate::manager::RunMode;
use indicatif::{ProgressBar, ProgressFinish, ProgressStyle};
use std::io::IsTerminal;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
enum OutputMode {
    Plain,
    Styled { color: bool },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct OutputTheme {
    mode: OutputMode,
    pub(crate) verbose: bool,
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

    pub(crate) fn plain(self) -> bool {
        matches!(self.mode, OutputMode::Plain)
    }

    pub(crate) fn color(self) -> bool {
        match self.mode {
            OutputMode::Plain => false,
            OutputMode::Styled { color } => color,
        }
    }

    pub(crate) fn unicode(self) -> bool {
        !self.plain()
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

pub(crate) fn init_output_theme(plain_flag: bool, no_color_flag: bool, verbose_flag: bool) {
    let options = ThemeOptions {
        plain_flag,
        no_color_flag,
        verbose_flag,
    };
    let theme = OutputTheme::from_options(options);
    let _ = OUTPUT_THEME.set(theme);
}

pub(crate) fn output_theme() -> OutputTheme {
    *OUTPUT_THEME.get_or_init(|| {
        OutputTheme::from_options(ThemeOptions {
            plain_flag: false,
            no_color_flag: false,
            verbose_flag: false,
        })
    })
}

pub(crate) struct ManagerSpinner(Option<ProgressBar>);

fn active_spinner_slot() -> &'static Mutex<Option<ProgressBar>> {
    ACTIVE_SPINNER.get_or_init(|| Mutex::new(None))
}

pub(crate) fn start_manager_spinner(manager: &str, run_mode: RunMode) -> ManagerSpinner {
    if output_theme().plain() || !std::io::stderr().is_terminal() {
        return ManagerSpinner(None);
    }

    let action = match run_mode {
        RunMode::Plan => "Planning",
        RunMode::Apply => "Applying",
        RunMode::Scan => "Scanning",
    };

    let pb = ProgressBar::new_spinner().with_finish(ProgressFinish::AndClear);
    let style = if output_theme().color() {
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .expect("invalid spinner template")
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
    } else {
        ProgressStyle::with_template("{spinner} {msg}")
            .expect("invalid spinner template")
            .tick_strings(&["-", "\\", "|", "/"])
    };

    pb.set_style(style);
    pb.set_message(format!("{action} {manager}..."));
    pb.enable_steady_tick(Duration::from_millis(90));

    *active_spinner_slot()
        .lock()
        .expect("active spinner mutex poisoned") = Some(pb.clone());

    ManagerSpinner(Some(pb))
}

pub(crate) fn with_spinner_suspended<F: FnOnce()>(f: F) {
    let spinner = active_spinner_slot()
        .lock()
        .expect("active spinner mutex poisoned")
        .clone();

    if let Some(pb) = spinner {
        pb.suspend(f);
    } else {
        f();
    }
}

pub(crate) fn finish_manager_spinner(spinner: ManagerSpinner) {
    if let Some(pb) = spinner.0 {
        *active_spinner_slot()
            .lock()
            .expect("active spinner mutex poisoned") = None;
        pb.finish_and_clear();
    }
}
