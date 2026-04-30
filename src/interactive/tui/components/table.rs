use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Row, Table, TableState};

use crate::interactive::tui::components::scrollbar::render_vertical_scrollbar;
use crate::interactive::tui::theme::TuiTheme;

pub(in crate::interactive::tui) struct TuiTable<const N: usize> {
    rows: Vec<Row<'static>>,
    columns: [Constraint; N],
    header: Option<Row<'static>>,
    selected: Option<usize>,
    row_highlight_style: Option<Style>,
}

impl<const N: usize> TuiTable<N> {
    pub(in crate::interactive::tui) fn new(
        rows: Vec<Row<'static>>,
        columns: [Constraint; N],
    ) -> Self {
        Self {
            rows,
            columns,
            header: None,
            selected: None,
            row_highlight_style: None,
        }
    }

    pub(in crate::interactive::tui) fn header(mut self, header: Row<'static>) -> Self {
        self.header = Some(header);
        self
    }

    pub(in crate::interactive::tui) fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }

    pub(in crate::interactive::tui) fn row_highlight_style(mut self, style: Style) -> Self {
        self.row_highlight_style = Some(style);
        self
    }
}

pub(in crate::interactive::tui) fn render_table<const N: usize>(
    frame: &mut Frame<'_>,
    area: Rect,
    config: TuiTable<N>,
    theme: &TuiTheme,
) {
    let content_length = config.rows.len();
    let has_header = config.header.is_some();
    let mut table = Table::new(config.rows, config.columns).column_spacing(2);
    if let Some(header) = config.header {
        table = table.header(header);
    }
    if let Some(style) = config.row_highlight_style {
        table = table.row_highlight_style(style);
    }

    let mut state = TableState::default().with_selected(config.selected);
    frame.render_stateful_widget(table, area, &mut state);

    let header_height = usize::from(has_header);
    render_vertical_scrollbar(
        frame,
        area,
        content_length,
        state.offset(),
        usize::from(area.height).saturating_sub(header_height),
        theme,
    );
}

pub(in crate::interactive::tui) fn update_header_row(theme: &TuiTheme) -> Row<'static> {
    Row::new(vec!["", "Manager", "Name", "Current", "Target", "Note"]).style(theme.header)
}

pub(in crate::interactive::tui) fn selection_update_columns() -> [Constraint; 6] {
    [
        Constraint::Length(4),
        Constraint::Max(10),
        Constraint::Max(30),
        Constraint::Max(18),
        Constraint::Max(18),
        Constraint::Fill(1),
    ]
}

pub(in crate::interactive::tui) fn progress_update_columns() -> [Constraint; 6] {
    [
        Constraint::Length(4),
        Constraint::Max(10),
        Constraint::Min(12),
        Constraint::Max(14),
        Constraint::Max(14),
        Constraint::Fill(1),
    ]
}

pub(in crate::interactive::tui) fn version_picker_columns() -> [Constraint; 3] {
    [
        Constraint::Length(4),
        Constraint::Max(24),
        Constraint::Fill(1),
    ]
}
