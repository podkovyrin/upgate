use ratatui::style::{Color, Modifier, Style};

use crate::{OutputTheme, ThemeOptions};

pub(super) struct TuiTheme {
    pub normal: Style,
    pub muted: Style,
    pub header: Style,
    pub selected: Style,
    pub selected_row_highlight: Style,
    pub keycap: Style,
    pub primary_keycap: Style,
    pub note: Style,
    pub forced: Style,
    pub running: Style,
    pub version_changed: Style,
    pub version_changed_selected: Style,
    pub frame_border: Style,
    pub separator: Style,
    pub scrollbar_thumb: Style,
}

impl TuiTheme {
    pub(super) fn current() -> Self {
        Self::from_output_theme(OutputTheme::from_environment(ThemeOptions::default()))
    }

    fn from_output_theme(output: OutputTheme) -> Self {
        if output.color() {
            return Self {
                normal: Style::default(),
                muted: Style::default().fg(Color::Gray),
                header: Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
                selected: Style::default().fg(Color::Black).bg(Color::Cyan),
                selected_row_highlight: Style::default().bg(Color::Cyan),
                keycap: Style::default()
                    .fg(Color::Black)
                    .bg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
                primary_keycap: Style::default()
                    .fg(Color::White)
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
                note: Style::default().fg(Color::Gray),
                forced: Style::default().fg(Color::Red),
                running: Style::default().fg(Color::Yellow),
                version_changed: Style::default().fg(Color::Blue),
                version_changed_selected: Style::default()
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                frame_border: Style::default(),
                separator: Style::default().fg(Color::Gray),
                scrollbar_thumb: Style::default().fg(Color::Black),
            };
        }

        let selected = Style::default().add_modifier(Modifier::REVERSED);
        let emphasized = Style::default().add_modifier(Modifier::BOLD);
        Self {
            normal: Style::default(),
            muted: Style::default(),
            header: emphasized,
            selected,
            selected_row_highlight: selected,
            keycap: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            primary_keycap: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            note: Style::default(),
            forced: Style::default(),
            running: emphasized,
            version_changed: emphasized,
            version_changed_selected: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            frame_border: Style::default(),
            separator: Style::default(),
            scrollbar_thumb: Style::default().add_modifier(Modifier::REVERSED),
        }
    }

    pub(super) const fn row_for_selectable_state(&self, selected: bool) -> Style {
        if selected { self.selected } else { self.normal }
    }

    #[expect(clippy::unused_self)]
    pub(super) const fn emphasis(&self, style: Style) -> Style {
        style.add_modifier(Modifier::BOLD)
    }

    pub(super) const fn version_changed_for(&self, selected: bool) -> Style {
        if selected {
            self.version_changed_selected
        } else {
            self.version_changed
        }
    }
}
