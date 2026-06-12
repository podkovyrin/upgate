use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear};

use crate::tui::theme::TuiTheme;

pub fn render_modal_frame(
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
        .border_style(theme.frame_border);
    if let Some(title) = title {
        block = block.title(title);
    }
    let inner = block.inner(popup);

    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);

    Some(inner)
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}
