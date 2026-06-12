use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Tabs};

use crate::tui::theme::TuiTheme;

const LEFT_OVERFLOW_HINT_WIDTH: usize = 5;
const LEFT_OVERFLOW_HINT: &str = " ⇧+⇥ ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleTabs {
    pub start: usize,
    pub selected_in_slice: usize,
    pub titles: Vec<Line<'static>>,
    pub has_left_overflow: bool,
}

pub fn visible_tabs(
    all_titles: &[Line<'static>],
    selected: usize,
    previous_offset: usize,
    available_width: u16,
) -> VisibleTabs {
    if all_titles.is_empty() {
        return VisibleTabs {
            start: 0,
            selected_in_slice: 0,
            titles: Vec::new(),
            has_left_overflow: false,
        };
    }

    let selected = selected.min(all_titles.len() - 1);
    let mut start = previous_offset.min(selected);
    let available_width = usize::from(available_width);

    while start < selected
        && tabs_width(&all_titles[start..=selected]) + left_overflow_hint_width(start > 0)
            > available_width
    {
        start += 1;
    }

    let mut used = left_overflow_hint_width(start > 0);
    let mut titles = Vec::new();
    for title in all_titles.iter().skip(start) {
        let title_width = tab_width(title);
        if used + title_width > available_width && !titles.is_empty() {
            break;
        }

        titles.push(title.clone());
        used += title_width;
    }

    let selected_in_slice = selected
        .saturating_sub(start)
        .min(titles.len().saturating_sub(1));
    VisibleTabs {
        start,
        selected_in_slice,
        titles,
        has_left_overflow: start > 0,
    }
}

pub fn render_tabs(frame: &mut Frame<'_>, area: Rect, tabs: VisibleTabs, theme: &TuiTheme) {
    if area.is_empty() {
        return;
    }

    let left_hint_width = if tabs.has_left_overflow {
        area.width
            .min(u16::try_from(LEFT_OVERFLOW_HINT_WIDTH).unwrap_or(u16::MAX))
    } else {
        0
    };

    let [left_area, tabs_area] =
        Layout::horizontal([Constraint::Length(left_hint_width), Constraint::Fill(1)]).areas(area);

    if tabs.has_left_overflow && left_hint_width > 0 {
        frame.render_widget(left_overflow_hint(theme), left_area);
    }

    if !tabs.titles.is_empty() {
        let widget = Tabs::new(tabs.titles)
            .select(tabs.selected_in_slice)
            .style(theme.normal)
            .highlight_style(theme.keycap)
            .divider("")
            .padding(" ", " ");
        frame.render_widget(widget, tabs_area);
    }
}

fn left_overflow_hint(theme: &TuiTheme) -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::styled(LEFT_OVERFLOW_HINT, theme.keycap)))
}

fn tabs_width(titles: &[Line<'_>]) -> usize {
    titles.iter().map(tab_width).sum()
}

fn tab_width(title: &Line<'_>) -> usize {
    title.width() + 2
}

const fn left_overflow_hint_width(enabled: bool) -> usize {
    if enabled { LEFT_OVERFLOW_HINT_WIDTH } else { 0 }
}
