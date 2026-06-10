use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Row, Table, TableState};

use crate::tui::components::scrollbar::render_vertical_scrollbar;
use crate::tui::theme::TuiTheme;

const COMPACT_TABLE_WIDTH: u16 = 96;
const VERY_NARROW_TABLE_WIDTH: u16 = 72;

pub struct TuiTable<const N: usize> {
    rows: Vec<Row<'static>>,
    columns: [Constraint; N],
    header: Option<Row<'static>>,
    selected: Option<usize>,
    offset: usize,
    row_highlight_style: Option<Style>,
}

impl<const N: usize> TuiTable<N> {
    pub(crate) const fn new(rows: Vec<Row<'static>>, columns: [Constraint; N]) -> Self {
        Self {
            rows,
            columns,
            header: None,
            selected: None,
            offset: 0,
            row_highlight_style: None,
        }
    }

    pub(crate) fn header(mut self, header: Row<'static>) -> Self {
        self.header = Some(header);
        self
    }

    pub(crate) const fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }

    pub(crate) const fn offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    pub(crate) const fn row_highlight_style(mut self, style: Style) -> Self {
        self.row_highlight_style = Some(style);
        self
    }
}

pub fn render_table<const N: usize>(
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

    let mut state = TableState::default()
        .with_offset(config.offset)
        .with_selected(config.selected);
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

pub fn update_header_row(theme: &TuiTheme) -> Row<'static> {
    Row::new(vec!["", "Manager", "Name", "Current", "Target", "Note"]).style(theme.header)
}

pub const fn selection_update_columns(width: u16) -> [Constraint; 6] {
    if width < VERY_NARROW_TABLE_WIDTH {
        return [
            Constraint::Length(3),
            Constraint::Max(7),
            Constraint::Max(18),
            Constraint::Length(0),
            Constraint::Max(12),
            Constraint::Fill(1),
        ];
    }

    if width < COMPACT_TABLE_WIDTH {
        return [
            Constraint::Length(3),
            Constraint::Max(7),
            Constraint::Max(22),
            Constraint::Max(14),
            Constraint::Max(14),
            Constraint::Fill(1),
        ];
    }

    [
        Constraint::Length(3),
        Constraint::Max(7),
        Constraint::Max(30),
        Constraint::Max(18),
        Constraint::Max(18),
        Constraint::Fill(1),
    ]
}

pub const fn progress_update_columns(width: u16) -> [Constraint; 6] {
    if width < VERY_NARROW_TABLE_WIDTH {
        return [
            Constraint::Length(3),
            Constraint::Max(7),
            Constraint::Max(18),
            Constraint::Length(0),
            Constraint::Max(12),
            Constraint::Fill(1),
        ];
    }

    if width < COMPACT_TABLE_WIDTH {
        return [
            Constraint::Length(3),
            Constraint::Max(7),
            Constraint::Max(22),
            Constraint::Max(14),
            Constraint::Max(14),
            Constraint::Fill(1),
        ];
    }

    [
        Constraint::Length(3),
        Constraint::Max(7),
        Constraint::Max(30),
        Constraint::Max(14),
        Constraint::Max(14),
        Constraint::Fill(1),
    ]
}

pub const fn version_picker_columns(width: u16) -> [Constraint; 3] {
    if width < VERY_NARROW_TABLE_WIDTH {
        return [
            Constraint::Length(3),
            Constraint::Max(12),
            Constraint::Fill(1),
        ];
    }

    if width < COMPACT_TABLE_WIDTH {
        return [
            Constraint::Length(3),
            Constraint::Max(14),
            Constraint::Fill(1),
        ];
    }

    [
        Constraint::Length(3),
        Constraint::Max(24),
        Constraint::Fill(1),
    ]
}
