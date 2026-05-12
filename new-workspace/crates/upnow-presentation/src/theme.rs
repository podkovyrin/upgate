use std::io::IsTerminal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Plain,
    Styled { color: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputTheme {
    mode: OutputMode,
    pub verbose: bool,
}

impl OutputTheme {
    #[must_use]
    pub fn from_environment(options: ThemeOptions) -> Self {
        Self::from_terminal(
            options,
            TerminalCapabilities {
                stdout_is_tty: std::io::stdout().is_terminal(),
                stderr_is_tty: std::io::stderr().is_terminal(),
                no_color_env: std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()),
                term_is_dumb: std::env::var("TERM").is_ok_and(|value| value == "dumb"),
            },
        )
    }

    #[must_use]
    pub const fn from_terminal(options: ThemeOptions, capabilities: TerminalCapabilities) -> Self {
        let plain = options.plain || !capabilities.stdout_is_tty;
        let mode = if plain {
            OutputMode::Plain
        } else {
            OutputMode::Styled {
                color: !options.no_color
                    && !capabilities.no_color_env
                    && !capabilities.term_is_dumb,
            }
        };

        Self {
            mode,
            verbose: options.verbose,
        }
    }

    #[must_use]
    pub const fn plain(verbose: bool) -> Self {
        Self {
            mode: OutputMode::Plain,
            verbose,
        }
    }

    #[must_use]
    pub const fn styled(color: bool, verbose: bool) -> Self {
        Self {
            mode: OutputMode::Styled { color },
            verbose,
        }
    }

    #[must_use]
    pub const fn is_plain(self) -> bool {
        matches!(self.mode, OutputMode::Plain)
    }

    #[must_use]
    pub const fn color(self) -> bool {
        match self.mode {
            OutputMode::Plain => false,
            OutputMode::Styled { color } => color,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ThemeOptions {
    pub plain: bool,
    pub no_color: bool,
    pub verbose: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub stdout_is_tty: bool,
    pub stderr_is_tty: bool,
    pub no_color_env: bool,
    pub term_is_dumb: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_flag_forces_plain_without_color() {
        let theme = OutputTheme::from_terminal(
            ThemeOptions {
                plain: true,
                no_color: false,
                verbose: false,
            },
            tty_capabilities(),
        );

        assert!(theme.is_plain());
        assert!(!theme.color());
    }

    #[test]
    fn non_tty_stdout_uses_plain_output() {
        let theme = OutputTheme::from_terminal(
            ThemeOptions::default(),
            TerminalCapabilities {
                stdout_is_tty: false,
                stderr_is_tty: true,
                no_color_env: false,
                term_is_dumb: false,
            },
        );

        assert!(theme.is_plain());
        assert!(!theme.color());
    }

    #[test]
    fn no_color_disables_color_without_plain_mode() {
        let theme = OutputTheme::from_terminal(
            ThemeOptions {
                plain: false,
                no_color: true,
                verbose: false,
            },
            tty_capabilities(),
        );

        assert!(!theme.is_plain());
        assert!(!theme.color());
    }

    #[test]
    fn no_color_environment_disables_color() {
        let theme = OutputTheme::from_terminal(
            ThemeOptions::default(),
            TerminalCapabilities {
                stdout_is_tty: true,
                stderr_is_tty: true,
                no_color_env: true,
                term_is_dumb: false,
            },
        );

        assert!(!theme.is_plain());
        assert!(!theme.color());
    }

    #[test]
    fn dumb_terminal_disables_color() {
        let theme = OutputTheme::from_terminal(
            ThemeOptions::default(),
            TerminalCapabilities {
                stdout_is_tty: true,
                stderr_is_tty: true,
                no_color_env: false,
                term_is_dumb: true,
            },
        );

        assert!(!theme.is_plain());
        assert!(!theme.color());
    }

    #[test]
    fn tty_uses_color_when_not_disabled() {
        let theme = OutputTheme::from_terminal(ThemeOptions::default(), tty_capabilities());

        assert!(!theme.is_plain());
        assert!(theme.color());
    }

    const fn tty_capabilities() -> TerminalCapabilities {
        TerminalCapabilities {
            stdout_is_tty: true,
            stderr_is_tty: true,
            no_color_env: false,
            term_is_dumb: false,
        }
    }
}
