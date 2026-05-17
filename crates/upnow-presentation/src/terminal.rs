use std::time::Duration;

use indicatif::{ProgressBar, ProgressFinish, ProgressStyle};

use crate::{OutputTheme, TerminalCapabilities};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchTerminalAction {
    Scan,
    Plan,
    Apply,
}

impl BatchTerminalAction {
    const fn spinner_label(self) -> &'static str {
        match self {
            Self::Scan => "Scanning",
            Self::Plan => "Planning",
            Self::Apply => "Applying",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationNotice {
    Skip,
    Real,
}

impl MutationNotice {
    pub const fn render(self) -> &'static str {
        match self {
            Self::Skip => {
                "note: apply runs in safe mode: mutating commands are skipped (safe mode)"
            }
            Self::Real => "warning: apply runs with real mutating commands are ENABLED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchTerminal {
    theme: OutputTheme,
    stderr_is_tty: bool,
    spinner_suppressed: bool,
}

impl BatchTerminal {
    pub const fn new(theme: OutputTheme, capabilities: TerminalCapabilities) -> Self {
        Self {
            theme,
            stderr_is_tty: capabilities.stderr_is_tty,
            spinner_suppressed: false,
        }
    }
    pub fn from_environment(theme: OutputTheme) -> Self {
        Self {
            theme,
            stderr_is_tty: std::io::IsTerminal::is_terminal(&std::io::stderr()),
            spinner_suppressed: false,
        }
    }
    pub const fn disabled(theme: OutputTheme) -> Self {
        Self {
            theme,
            stderr_is_tty: false,
            spinner_suppressed: true,
        }
    }
    pub const fn suppress_spinner(self) -> Self {
        Self {
            spinner_suppressed: true,
            ..self
        }
    }
    pub const fn spinner_enabled(self) -> bool {
        !self.spinner_suppressed && !self.theme.is_plain() && self.stderr_is_tty
    }
    pub const fn notice_enabled(self) -> bool {
        self.stderr_is_tty
    }
    pub fn start_manager_spinner(
        self,
        action: BatchTerminalAction,
        _manager_id: &str,
    ) -> ManagerSpinner {
        self.start_action_spinner(action)
    }
    pub fn start_action_spinner(self, action: BatchTerminalAction) -> ManagerSpinner {
        if !self.spinner_enabled() {
            return ManagerSpinner(None);
        }

        let progress = ProgressBar::new_spinner().with_finish(ProgressFinish::AndClear);
        progress.set_style(spinner_style(self.theme.color()));
        progress.set_message(format!("{}...", action.spinner_label()));
        progress.enable_steady_tick(Duration::from_millis(90));

        ManagerSpinner(Some(progress))
    }
}

#[derive(Debug)]
pub struct ManagerSpinner(Option<ProgressBar>);

impl Drop for ManagerSpinner {
    fn drop(&mut self) {
        if let Some(progress) = self.0.take() {
            progress.finish_and_clear();
        }
    }
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
