use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::widgets::{Row, Table, TableState};

use crate::tui::components::scrollbar::render_vertical_scrollbar;
use crate::tui::theme::TuiTheme;

pub(crate) fn render_selection_table(
    frame: &mut Frame<'_>,
    area: Rect,
    rows: Vec<Row<'static>>,
    selected: Option<usize>,
    theme: &TuiTheme,
) {
    let content_length = rows.len();
    let table = Table::new(rows, selection_update_columns())
        .column_spacing(2)
        .header(update_header_row(theme))
        .row_highlight_style(theme.selected);

    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);

    render_vertical_scrollbar(
        frame,
        area,
        content_length,
        state.offset(),
        usize::from(area.height).saturating_sub(1),
        theme,
    );
}

fn update_header_row(theme: &TuiTheme) -> Row<'static> {
    Row::new(vec!["", "Manager", "Name", "Current", "Target", "Note"]).style(theme.header)
}

fn selection_update_columns() -> [Constraint; 6] {
    [
        Constraint::Length(4),
        Constraint::Max(10),
        Constraint::Max(30),
        Constraint::Max(18),
        Constraint::Max(18),
        Constraint::Fill(1),
    ]
}
