use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::interactive::tui::theme::TuiTheme;

pub(in crate::interactive::tui) fn render_vertical_scrollbar(
    frame: &mut Frame<'_>,
    area: Rect,
    content_length: usize,
    position: usize,
    viewport_content_length: usize,
    theme: &TuiTheme,
) {
    if content_length <= viewport_content_length {
        return;
    }

    let mut scrollbar_state = ScrollbarState::new(content_length)
        .position(position)
        .viewport_content_length(viewport_content_length);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("┃")
            .thumb_style(theme.scrollbar_thumb),
        area,
        &mut scrollbar_state,
    );
}
