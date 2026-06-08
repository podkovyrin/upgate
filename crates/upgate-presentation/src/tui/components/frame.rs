use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::tui::theme::TuiTheme;

pub fn app_block(theme: &TuiTheme) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.frame_border)
}

pub fn render_separator(frame: &mut Frame<'_>, area: Rect, theme: &TuiTheme) {
    if area.is_empty() {
        return;
    }

    frame.render_widget(
        Paragraph::new("─".repeat(usize::from(area.width))).style(theme.separator),
        area,
    );
}
