use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear};

use crate::interactive::tui::layout::centered_rect;
use crate::interactive::tui::theme::TuiTheme;

pub(in crate::interactive::tui) fn render_modal_frame(
    frame: &mut Frame<'_>,
    area: Rect,
    width: u16,
    height: u16,
    title: Option<Line<'static>>,
    theme: &TuiTheme,
) -> Option<Rect> {
    if area.is_empty() || width == 0 || height == 0 {
        return None;
    }

    let popup = centered_rect(area, width, height);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.modal_border);
    if let Some(title) = title {
        block = block.title(title);
    }
    let inner = block.inner(popup);

    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);

    Some(inner)
}
