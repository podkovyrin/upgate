use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::tui::components::render_separator;
use crate::tui::theme::TuiTheme;

pub struct CommandLogLayout {
    pub main: Rect,
    pub separator: Rect,
    pub log: Rect,
}

pub fn command_log_layout(enabled: bool, area: Rect) -> Option<CommandLogLayout> {
    let height = command_log_height(enabled, area)?;
    let [main, separator, log] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(height),
    ])
    .areas(area);

    Some(CommandLogLayout {
        main,
        separator,
        log,
    })
}

pub fn render_command_log(
    frame: &mut Frame<'_>,
    separator_area: Rect,
    log_area: Rect,
    commands: &[String],
    theme: &TuiTheme,
) {
    render_separator(frame, separator_area, theme);

    let visible_height = usize::from(log_area.height);
    let start = commands.len().saturating_sub(visible_height);
    let lines = commands
        .iter()
        .skip(start)
        .map(|command| Line::from(Span::styled(format!("$ {command}"), theme.muted)))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), log_area);
}

fn command_log_height(enabled: bool, area: Rect) -> Option<u16> {
    if !enabled {
        return None;
    }

    match area.height {
        0..=5 => None,
        6 => Some(3),
        _ => Some(4),
    }
}
