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
    pub const fn plain(verbose: bool) -> Self {
        Self {
            mode: OutputMode::Plain,
            verbose,
        }
    }
    pub const fn styled(color: bool, verbose: bool) -> Self {
        Self {
            mode: OutputMode::Styled { color },
            verbose,
        }
    }
    pub const fn is_plain(self) -> bool {
        matches!(self.mode, OutputMode::Plain)
    }
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
#[expect(clippy::struct_excessive_bools)]
pub struct TerminalCapabilities {
    pub stdout_is_tty: bool,
    pub stderr_is_tty: bool,
    pub no_color_env: bool,
    pub term_is_dumb: bool,
}
