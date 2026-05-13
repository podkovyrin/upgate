use ratatui::style::{Color, Modifier, Style};

use crate::{OutputTheme, ThemeOptions};

pub(super) struct TuiTheme {
    pub normal: Style,
    pub muted: Style,
    pub title: Style,
    pub header: Style,
    pub selected: Style,
    pub keycap: Style,
    pub note: Style,
    pub forced: Style,
    pub pending: Style,
    pub running: Style,
    pub success: Style,
    pub error: Style,
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
                title: Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
                header: Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
                selected: Style::default().fg(Color::Black).bg(Color::Cyan),
                keycap: Style::default()
                    .fg(Color::Black)
                    .bg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
                note: Style::default().add_modifier(Modifier::ITALIC),
                forced: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                pending: Style::default().fg(Color::Gray),
                running: Style::default().fg(Color::Yellow),
                success: Style::default().fg(Color::Green),
                error: Style::default().fg(Color::Red),
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
            title: emphasized,
            header: emphasized,
            selected,
            keycap: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            note: Style::default().add_modifier(Modifier::ITALIC),
            forced: emphasized,
            pending: Style::default(),
            running: emphasized,
            success: emphasized,
            error: emphasized,
            version_changed: emphasized,
            version_changed_selected: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            frame_border: Style::default(),
            separator: Style::default(),
            scrollbar_thumb: Style::default().add_modifier(Modifier::REVERSED),
        }
    }

    pub(super) const fn row_for_selectable_state(&self, selected: bool, forced: bool) -> Style {
        if selected {
            self.selected
        } else if forced {
            self.forced
        } else {
            self.normal
        }
    }

    #[expect(clippy::unused_self)]
    pub(super) const fn emphasis(&self, style: Style) -> Style {
        style.add_modifier(Modifier::BOLD)
    }

    pub(super) fn note_for(&self, style: Style) -> Style {
        style.patch(self.note)
    }

    pub(super) fn forced_note_for(&self, selected: bool) -> Style {
        if selected {
            self.selected.patch(self.forced)
        } else {
            self.forced
        }
    }

    pub(super) const fn version_changed_for(&self, selected: bool) -> Style {
        if selected {
            self.version_changed_selected
        } else {
            self.version_changed
        }
    }
}
