use ratatui::layout::{Constraint, Layout, Margin, Rect};

pub(super) struct AppFrame {
    pub outer: Rect,
    pub inner: Rect,
    pub header: Rect,
    pub header_separator: Rect,
    pub body: Rect,
    pub footer_separator: Rect,
    pub footer: Rect,
}

pub(super) fn app_frame(area: Rect) -> Option<AppFrame> {
    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    if inner.height < 5 {
        return None;
    }

    let [header, header_separator, body, footer_separator, footer] =
        vertical_header_body_footer(inner);
    Some(AppFrame {
        outer: area,
        inner,
        header,
        header_separator,
        body,
        footer_separator,
        footer,
    })
}

pub(super) fn vertical_header_body_footer(area: Rect) -> [Rect; 5] {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area)
}
