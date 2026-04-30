use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row};
use unicode_width::UnicodeWidthStr;

use super::SelectionPlan;
use super::model::{SelectionApp, SelectionContentState, VersionPickerState, tab_label};
use crate::interactive::tui::components::footer::{KeyBinding, key_footer};
use crate::interactive::tui::components::frame::{app_block, render_separator};
use crate::interactive::tui::components::modal::render_modal_frame;
use crate::interactive::tui::components::table::{
    TuiTable, render_table, selection_update_columns, update_header_row, version_picker_columns,
};
use crate::interactive::tui::components::tabs::{render_tabs as render_tab_widget, visible_tabs};
use crate::interactive::tui::layout::app_frame;
use crate::interactive::tui::text::{truncate_with_ellipsis, version_diff_spans};
use crate::interactive::tui::theme::TuiTheme;
use crate::managers::{ApplyCandidate, ApplyCandidateDisplayNote, ApplyCandidateNotePart};
use crate::outcome::version_label;

const TAB_KEY_LABEL: &str = " ⇥ ";
const FOOTER_KEYS: &[KeyBinding<'static>] = &[
    KeyBinding {
        key: "↑ ↓ j k",
        label: "move",
    },
    KeyBinding {
        key: "space x",
        label: "toggle",
    },
    KeyBinding {
        key: "a",
        label: "all",
    },
    KeyBinding {
        key: "n",
        label: "none",
    },
    KeyBinding {
        key: "v",
        label: "view all",
    },
    KeyBinding {
        key: "enter",
        label: "details",
    },
    KeyBinding {
        key: "C",
        label: "confirm",
    },
    KeyBinding {
        key: "q",
        label: "quit",
    },
];
const VERSION_PICKER_MAIN_MOVE_KEY: KeyBinding<'static> = KeyBinding {
    key: "⇧+↑/↓/J/K",
    label: "move",
};

const VERSION_PICKER_FOOTER_KEYS: &[KeyBinding<'static>] = &[
    KeyBinding {
        key: "↑ ↓ j k",
        label: "move",
    },
    KeyBinding {
        key: "r",
        label: "recommended",
    },
    KeyBinding {
        key: "esc",
        label: "cancel",
    },
    KeyBinding {
        key: "enter",
        label: "confirm",
    },
];

#[derive(Debug, Clone)]
struct SelectionRenderRow {
    selected: bool,
    manager: &'static str,
    name: String,
    current: String,
    target: String,
    note: String,
    forced: bool,
}

pub(super) fn draw_selection(
    frame: &mut Frame<'_>,
    app: &mut SelectionApp,
    plans: &[SelectionPlan],
) {
    let area = frame.area();
    let theme = TuiTheme::current();

    let block = app_block(&theme);
    let Some(app_frame) = app_frame(area) else {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new("Terminal too small"), inner);
        return;
    };
    frame.render_widget(block, app_frame.outer);

    app.ensure_tab_visible(usize::from(app_frame.header.width));
    draw_tabs(frame, app, app_frame.header, &theme);
    render_separator(frame, app_frame.header_separator, &theme);
    draw_content(frame, app, plans, app_frame.body, &theme);
    render_separator(frame, app_frame.footer_separator, &theme);
    frame.render_widget(
        Paragraph::new(footer_line(app.version_picker.is_some(), &theme)),
        app_frame.footer,
    );

    if let Some(picker) = app.version_picker {
        draw_version_picker(frame, app, plans, picker, app_frame.inner, &theme);
    }
}

fn draw_content(
    frame: &mut Frame<'_>,
    app: &mut SelectionApp,
    plans: &[SelectionPlan],
    area: Rect,
    theme: &TuiTheme,
) {
    match app.content_state(plans) {
        SelectionContentState::List => draw_list_content(frame, app, plans, area, theme),
        SelectionContentState::Placeholder { message } => {
            draw_centered_placeholder(frame, area, &message, theme.muted);
        }
        SelectionContentState::Error { message } => {
            draw_centered_placeholder(frame, area, &message, theme.error);
        }
    }
}

fn draw_tabs(frame: &mut Frame<'_>, app: &SelectionApp, area: Rect, theme: &TuiTheme) {
    let tab_key_width = UnicodeWidthStr::width(TAB_KEY_LABEL);
    let key_area_width = u16::try_from(tab_key_width)
        .unwrap_or(u16::MAX)
        .min(area.width);
    let [tabs_area, key_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(key_area_width)]).areas(area);
    let titles = (0..app.tab_count())
        .map(|idx| Line::raw(tab_label(app, idx)))
        .collect::<Vec<_>>();
    let tabs = visible_tabs(&titles, app.active_tab_idx, app.tab_offset, tabs_area.width);

    debug_assert!(tabs.start <= app.active_tab_idx);
    render_tab_widget(frame, tabs_area, &tabs, theme);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(TAB_KEY_LABEL, theme.keycap))),
        key_area,
    );
}

fn draw_list_content(
    frame: &mut Frame<'_>,
    app: &mut SelectionApp,
    plans: &[SelectionPlan],
    area: Rect,
    theme: &TuiTheme,
) {
    if area.height < 2 {
        frame.render_widget(Paragraph::new("Terminal too small"), area);
        return;
    }

    app.clamp_cursor(plans);

    let render_rows = selection_render_rows(app, plans);
    let table_rows = render_rows
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let highlighted = idx == app.cursor_idx;
            selection_table_row(row, highlighted, theme)
        })
        .collect::<Vec<_>>();

    let selected = (app.cursor_idx < render_rows.len()).then_some(app.cursor_idx);
    render_table(
        frame,
        area,
        TuiTable::new(table_rows, selection_update_columns())
            .header(update_header_row(theme))
            .selected(selected)
            .row_highlight_style(theme.selected),
        theme,
    );
}

fn draw_centered_placeholder(frame: &mut Frame<'_>, area: Rect, message: &str, style: Style) {
    let [line_area] = Layout::vertical([Constraint::Length(1)])
        .flex(Flex::Center)
        .areas(area);
    let line = Line::from(Span::styled(
        truncate_with_ellipsis(message, usize::from(area.width)),
        style,
    ))
    .centered();
    frame.render_widget(Paragraph::new(line), line_area);
}

fn selection_render_rows(
    app: &mut SelectionApp,
    plans: &[SelectionPlan],
) -> Vec<SelectionRenderRow> {
    let row_count = app.visible_row_count(plans);
    (0..row_count)
        .filter_map(|row| {
            let row = app.visible_row_at(row, plans)?;
            let manager = app.manager_selection(row.manager_idx)?;
            let candidates = manager.candidates(plans)?;
            let candidate = &candidates[row.candidate_idx];
            let selected_version_idx = manager.selected_version_idx[row.candidate_idx];
            let update = candidate.selected_update(selected_version_idx);
            let selected = manager.selected[row.candidate_idx];
            let (note, forced) = match candidate.display_note(selected_version_idx, selected) {
                ApplyCandidateDisplayNote::Normal(note) => (note, false),
                ApplyCandidateDisplayNote::Forced(note) => (note, true),
            };

            Some(SelectionRenderRow {
                selected,
                manager: manager.manager_id,
                name: update.name.clone(),
                current: version_label(&update.current),
                target: version_label(&update.target),
                note,
                forced,
            })
        })
        .collect()
}

fn selection_table_row(
    row: &SelectionRenderRow,
    highlighted: bool,
    theme: &TuiTheme,
) -> Row<'static> {
    let style = theme.row_for_selectable_state(highlighted, false);
    let marker = if row.selected { "[x]" } else { "[ ]" };
    let target = Line::from(version_diff_spans(
        &row.current,
        &row.target,
        style,
        theme,
        highlighted,
    ));
    let note = if row.forced {
        forced_note_cell(&row.note, style, highlighted, theme)
    } else {
        Cell::new(row.note.clone()).style(theme.note_for(style))
    };

    Row::new(vec![
        Cell::new(marker).style(style),
        Cell::new(row.manager).style(style),
        Cell::new(row.name.clone()).style(theme.emphasis(style)),
        Cell::new(row.current.clone()).style(style),
        Cell::new(target).style(style),
        note,
    ])
    .style(style)
}

fn forced_note_cell(
    note: &str,
    base_style: Style,
    highlighted: bool,
    theme: &TuiTheme,
) -> Cell<'static> {
    let mut spans = vec![Span::styled("forced", theme.forced_note_for(highlighted))];
    if !note.is_empty() {
        spans.push(Span::styled(", ", theme.note_for(base_style)));
        spans.push(Span::styled(note.to_string(), theme.note_for(base_style)));
    }

    Cell::new(Line::from(spans)).style(theme.note_for(base_style))
}

fn footer_line(picker_open: bool, theme: &TuiTheme) -> Line<'static> {
    if !picker_open {
        return key_footer(FOOTER_KEYS, theme);
    }

    key_footer(&[VERSION_PICKER_MAIN_MOVE_KEY], theme)
}

fn draw_version_picker(
    frame: &mut Frame<'_>,
    app: &SelectionApp,
    plans: &[SelectionPlan],
    picker: VersionPickerState,
    area: Rect,
    theme: &TuiTheme,
) {
    let Some(manager) = app.manager_selection(picker.manager) else {
        return;
    };
    let Some(candidates) = manager.candidates(plans) else {
        return;
    };
    let candidate = &candidates[picker.candidate];
    let selected_idx = manager.selected_version_idx[picker.candidate];
    let Some(inner) = render_modal_frame(
        frame,
        area,
        version_picker_width(area),
        version_picker_height(version_picker_row_count(candidate)),
        None,
        theme,
    ) else {
        return;
    };

    if inner.height < 5 || inner.width < 20 {
        frame.render_widget(Paragraph::new("Terminal too small"), inner);
        return;
    }

    let [title_area, _, current_area, _, list_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    let title = Line::from(Span::styled(
        format!("{}: {}", manager.manager_id, candidate.update().name),
        theme.title,
    ))
    .centered();
    frame.render_widget(Paragraph::new(title), title_area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Current: ", theme.header),
            Span::raw(version_label(&candidate.update().current)),
        ])),
        current_area,
    );

    draw_version_picker_rows(frame, app, plans, picker, list_area, selected_idx, theme);
    frame.render_widget(
        Paragraph::new(key_footer(VERSION_PICKER_FOOTER_KEYS, theme)),
        footer_area,
    );
}

fn draw_version_picker_rows(
    frame: &mut Frame<'_>,
    app: &SelectionApp,
    plans: &[SelectionPlan],
    picker: VersionPickerState,
    area: Rect,
    selected_idx: usize,
    theme: &TuiTheme,
) {
    let Some(manager) = app.manager_selection(picker.manager) else {
        return;
    };
    let Some(candidates) = manager.candidates(plans) else {
        return;
    };
    let candidate = &candidates[picker.candidate];
    let rows = version_picker_rows(candidate, manager.selected_version_idx[picker.candidate]);
    let current = version_label(&candidate.update().current);
    let table_rows = rows
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let highlighted = idx == picker.cursor;
            let selected = idx == selected_idx;
            version_picker_table_row(&current, row, selected, highlighted, theme)
        })
        .collect::<Vec<_>>();

    let selected = (picker.cursor < rows.len()).then_some(picker.cursor);
    render_table(
        frame,
        area,
        TuiTable::new(table_rows, version_picker_columns())
            .selected(selected)
            .row_highlight_style(theme.selected),
        theme,
    );
}

fn version_picker_table_row(
    current: &str,
    row: &VersionPickerRow,
    selected: bool,
    highlighted: bool,
    theme: &TuiTheme,
) -> Row<'static> {
    let style = theme.row_for_selectable_state(highlighted, false);
    let marker = if selected { "[x]" } else { "[ ]" };
    let target = Line::from(version_diff_spans(
        current,
        &version_label(&row.target),
        style,
        theme,
        highlighted,
    ));
    let note = picker_note_line(&row.note_parts, theme.note_for(style), theme);

    Row::new(vec![
        Cell::new(marker).style(style),
        Cell::new(target).style(style),
        Cell::new(note).style(theme.note_for(style)),
    ])
    .style(style)
}

fn picker_note_line(
    note_parts: &[ApplyCandidateNotePart],
    style: Style,
    theme: &TuiTheme,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (idx, part) in note_parts.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled("; ", style));
        }
        let part_style = if part.violation {
            style.patch(theme.forced)
        } else {
            style
        };
        spans.push(Span::styled(part.text.clone(), part_style));
    }

    Line::from(spans)
}

fn version_picker_height(version_count: usize) -> u16 {
    let body = u16::try_from(version_count.min(10)).unwrap_or(10);
    body.saturating_add(8).clamp(9, 18)
}

fn version_picker_width(area: Rect) -> u16 {
    area.width.saturating_sub(4).clamp(62, 96)
}

#[derive(Debug)]
struct VersionPickerRow {
    target: String,
    note_parts: Vec<ApplyCandidateNotePart>,
}

fn version_picker_row_count(candidate: &ApplyCandidate) -> usize {
    candidate.versions().len().max(1)
}

fn version_picker_rows(
    candidate: &ApplyCandidate,
    selected_version_idx: usize,
) -> Vec<VersionPickerRow> {
    if candidate.versions().is_empty() {
        let update = candidate.selected_update(selected_version_idx);
        return vec![VersionPickerRow {
            target: update.target.clone(),
            note_parts: candidate.note_parts(),
        }];
    }

    candidate
        .versions()
        .iter()
        .map(|version| VersionPickerRow {
            target: version.update().target.clone(),
            note_parts: version.note_parts(),
        })
        .collect()
}
