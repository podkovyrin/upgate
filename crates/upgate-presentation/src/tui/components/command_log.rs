use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};

use super::scrollbar::render_vertical_scrollbar;
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
    scroll_from_bottom: usize,
    theme: &TuiTheme,
) {
    render_separator(frame, separator_area, theme);

    let visible_height = usize::from(log_area.height);
    let scroll_from_bottom =
        clamp_command_log_scroll(scroll_from_bottom, commands.len(), visible_height);
    let max_offset = commands.len().saturating_sub(visible_height);
    let offset_from_top = max_offset.saturating_sub(scroll_from_bottom);
    let items = commands.iter().map(|command| {
        ListItem::new(Line::from(Span::styled(
            format!("$ {command}"),
            theme.muted,
        )))
    });
    let mut state = ListState::default().with_offset(offset_from_top);
    frame.render_stateful_widget(List::new(items), log_area, &mut state);
    render_vertical_scrollbar(
        frame,
        log_area,
        commands.len(),
        offset_from_top,
        visible_height,
        theme,
    );
}

pub fn clamp_command_log_scroll(
    scroll_from_bottom: usize,
    command_count: usize,
    visible_height: usize,
) -> usize {
    scroll_from_bottom.min(command_count.saturating_sub(visible_height))
}

const fn command_log_height(enabled: bool, area: Rect) -> Option<u16> {
    if !enabled {
        return None;
    }

    match area.height {
        0..=5 => None,
        6 => Some(3),
        _ => Some(4),
    }
}
