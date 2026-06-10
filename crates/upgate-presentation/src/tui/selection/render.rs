use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Wrap};
use unicode_width::UnicodeWidthStr;
use upgate_domain::{SelectedUpdate, VersionPolicy};

use super::SelectionInput;
use super::screen::{
    ConfirmationSummary, InteractiveSelectionScreen, ManagerPlanningStatus, ManagerSelectionState,
    SelectionTabRef, SelectionTabStatus, TargetPickerState, target_option_matches_selected,
};
use crate::outcome::{manager_resolved_label, version_label};
use crate::selection_view::note_part_text;
use crate::tui::components::{
    KeyBinding, TuiTable, app_block, command_log_layout, key_footer, render_command_log,
    render_modal_frame, render_separator, render_table, render_tabs, selection_update_columns,
    update_header_row, version_picker_columns, visible_tabs,
};
use crate::tui::layout::app_frame;
use crate::tui::progress::spinner_frame;
use crate::tui::text::{truncate_with_ellipsis, version_diff_spans};
use crate::tui::theme::TuiTheme;
use crate::{CandidateNoteKind, CandidateNotePart, SelectionRow, TargetOption};

pub(super) const TAB_KEY_LABEL: &str = " ⇥ ";
const FOOTER_KEYS: &[KeyBinding<'static>] = &[
    KeyBinding {
        key: "up/down j/k",
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
const FOOTER_INPUTS: &[Option<SelectionInput>] = &[
    None,
    Some(SelectionInput::ToggleCurrent),
    Some(SelectionInput::SelectVisible),
    Some(SelectionInput::SelectNoneVisible),
    Some(SelectionInput::ToggleViewAll),
    Some(SelectionInput::OpenTargetPicker),
    Some(SelectionInput::Confirm),
    Some(SelectionInput::Cancel),
];
const COMPACT_FOOTER_KEYS: &[KeyBinding<'static>] = &[
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
const COMPACT_FOOTER_INPUTS: &[Option<SelectionInput>] = &[
    Some(SelectionInput::ToggleViewAll),
    Some(SelectionInput::OpenTargetPicker),
    Some(SelectionInput::Confirm),
    Some(SelectionInput::Cancel),
];
const MINIMAL_FOOTER_KEYS: &[KeyBinding<'static>] = &[
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
const MINIMAL_FOOTER_INPUTS: &[Option<SelectionInput>] = &[
    Some(SelectionInput::OpenTargetPicker),
    Some(SelectionInput::Confirm),
    Some(SelectionInput::Cancel),
];
const COMPACT_FOOTER_WIDTH: u16 = 96;
const MINIMAL_FOOTER_WIDTH: u16 = 52;
pub(super) const PICKER_FOOTER_KEYS: &[KeyBinding<'static>] = &[
    KeyBinding {
        key: "up/down j/k",
        label: "target",
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
        label: "select",
    },
];
const PICKER_MAIN_MOVE_KEY: KeyBinding<'static> = KeyBinding {
    key: "shift+up/down J/K",
    label: "row",
};
const CONFIRMATION_FOOTER_KEYS: &[KeyBinding<'static>] = &[
    KeyBinding {
        key: "enter C",
        label: "apply",
    },
    KeyBinding {
        key: "esc",
        label: "back",
    },
    KeyBinding {
        key: "q",
        label: "quit",
    },
];

#[derive(Debug, Clone)]
struct SelectionRenderRow {
    selected: bool,
    manager: String,
    name: String,
    current: String,
    target: String,
    note_parts: Vec<CandidateNotePart>,
    forced: bool,
}

#[derive(Debug, Clone)]
struct TargetPickerRenderRow {
    target: String,
    note_parts: Vec<CandidateNotePart>,
}

pub(super) fn draw_selection(
    frame: &mut ratatui::Frame<'_>,
    screen: &mut InteractiveSelectionScreen,
) {
    let theme = TuiTheme::current();
    draw_selection_with_theme(frame, screen, &theme);
}

fn draw_selection_with_theme(
    frame: &mut ratatui::Frame<'_>,
    screen: &mut InteractiveSelectionScreen,
    theme: &TuiTheme,
) {
    let area = frame.area();
    let block = app_block(theme);
    let Some(app_frame) = app_frame(area) else {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new("Terminal too small"), inner);
        return;
    };
    frame.render_widget(block, app_frame.outer);

    draw_tabs(frame, screen, app_frame.header, theme);
    render_separator(frame, app_frame.header_separator, theme);

    draw_selection_body(frame, screen, app_frame.body, theme);

    render_separator(frame, app_frame.footer_separator, theme);
    frame.render_widget(
        Paragraph::new(footer_line(screen, app_frame.footer.width, theme)),
        app_frame.footer,
    );

    if let Some(picker) = screen.target_picker() {
        draw_target_picker(frame, screen, picker, app_frame.outer, theme);
    }
    if screen.confirmation_dialog_open() {
        draw_confirmation_dialog(frame, screen, app_frame.outer, theme);
    }
}

fn draw_selection_body(
    frame: &mut ratatui::Frame<'_>,
    screen: &mut InteractiveSelectionScreen,
    area: Rect,
    theme: &TuiTheme,
) {
    let Some(layout) = command_log_layout(screen.trace_commands, area) else {
        draw_selection_main(frame, screen, area, theme);
        return;
    };

    draw_selection_main(frame, screen, layout.main, theme);
    screen.clamp_command_log_scroll(usize::from(layout.log.height));
    render_command_log(
        frame,
        layout.separator,
        layout.log,
        &screen.command_log,
        screen.command_log_scroll_from_bottom,
        theme,
    );
}

fn draw_selection_main(
    frame: &mut ratatui::Frame<'_>,
    screen: &mut InteractiveSelectionScreen,
    area: Rect,
    theme: &TuiTheme,
) {
    if let Some(message) = screen.placeholder_message() {
        draw_centered_placeholder(frame, area, &message, theme.muted);
    } else {
        draw_list_content(frame, screen, area, theme);
    }
}

fn draw_tabs(
    frame: &mut ratatui::Frame<'_>,
    screen: &mut InteractiveSelectionScreen,
    area: Rect,
    theme: &TuiTheme,
) {
    let tab_key_width = UnicodeWidthStr::width(TAB_KEY_LABEL);
    let key_area_width = u16::try_from(tab_key_width)
        .unwrap_or(u16::MAX)
        .min(area.width);
    let [tabs_area, key_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(key_area_width)]).areas(area);
    let titles = selection_tab_titles(screen, theme);

    let tabs = visible_tabs(
        &titles,
        screen.active_tab,
        screen.tab_offset(),
        tabs_area.width,
    );
    screen.sync_tab_offset(tabs.start);
    render_tabs(frame, tabs_area, &tabs, theme);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(TAB_KEY_LABEL, theme.keycap))),
        key_area,
    );
}

pub(super) fn selection_tab_titles(
    screen: &InteractiveSelectionScreen,
    theme: &TuiTheme,
) -> Vec<Line<'static>> {
    screen
        .visible_tab_refs()
        .into_iter()
        .map(|tab| match tab {
            SelectionTabRef::All => {
                selection_tab_title("All", all_tab_status(screen), screen.spinner_tick, theme)
            }
            SelectionTabRef::Manager(manager_idx) => {
                let manager = &screen.managers[manager_idx];
                selection_tab_title(
                    manager.manager_id.as_str(),
                    manager_tab_status(manager),
                    screen.spinner_tick,
                    theme,
                )
            }
        })
        .collect()
}

fn selection_tab_title(
    label: &str,
    status: SelectionTabStatus,
    spinner_tick: usize,
    theme: &TuiTheme,
) -> Line<'static> {
    match status {
        SelectionTabStatus::Loading => Line::from(vec![
            Span::styled(spinner_frame(spinner_tick), theme.running),
            Span::raw(" "),
            Span::raw(label.to_owned()),
        ]),
        SelectionTabStatus::Ready => Line::raw(label.to_owned()),
    }
}

fn all_tab_status(screen: &InteractiveSelectionScreen) -> SelectionTabStatus {
    if screen
        .managers
        .iter()
        .filter(|manager| manager.planning_status != ManagerPlanningStatus::Empty)
        .any(|manager| {
            manager.planning_status == ManagerPlanningStatus::Planning
                || manager.planning_status == ManagerPlanningStatus::Waiting
        })
    {
        return SelectionTabStatus::Loading;
    }
    SelectionTabStatus::Ready
}

const fn manager_tab_status(manager: &ManagerSelectionState) -> SelectionTabStatus {
    match manager.planning_status {
        ManagerPlanningStatus::Waiting | ManagerPlanningStatus::Planning => {
            SelectionTabStatus::Loading
        }
        ManagerPlanningStatus::Ready
        | ManagerPlanningStatus::Empty
        | ManagerPlanningStatus::Error { .. } => SelectionTabStatus::Ready,
    }
}

fn draw_list_content(
    frame: &mut ratatui::Frame<'_>,
    screen: &mut InteractiveSelectionScreen,
    area: Rect,
    theme: &TuiTheme,
) {
    if area.height < 2 {
        frame.render_widget(Paragraph::new("Terminal too small"), area);
        return;
    }

    screen.clamp_cursor();
    screen.keep_cursor_visible(selection_table_visible_height(area));
    let render_rows = selection_render_rows(screen);
    let table_rows = render_rows
        .iter()
        .enumerate()
        .map(|(idx, row)| selection_table_row(row, screen.cursor() == Some(idx), theme))
        .collect::<Vec<_>>();

    let selected = screen.cursor().filter(|cursor| *cursor < render_rows.len());
    render_table(
        frame,
        area,
        TuiTable::new(table_rows, selection_update_columns(area.width))
            .header(update_header_row(theme))
            .selected(selected)
            .offset(screen.table_offset)
            .row_highlight_style(theme.selected_row_highlight),
        theme,
    );
}

pub(super) fn selection_table_visible_height(area: Rect) -> usize {
    usize::from(area.height.saturating_sub(1))
}

fn draw_centered_placeholder(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    message: &str,
    style: Style,
) {
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

fn selection_render_rows(screen: &InteractiveSelectionScreen) -> Vec<SelectionRenderRow> {
    screen
        .visible_row_refs()
        .into_iter()
        .map(|visible| {
            let manager = &screen.managers[visible.manager_idx];
            let row = screen.row(visible);
            let selected_target = manager.state.selected_target(&row.plan_item_id);
            let selected_option =
                selected_target.and_then(|target| selected_target_option(row, target));
            let selected_exact_option = match selected_target {
                Some(SelectedUpdate::Exact { .. }) => selected_option,
                _ => None,
            };
            let selected = selected_target.is_some();
            let target =
                match selected_target {
                    Some(SelectedUpdate::Exact { target_version }) => {
                        version_label(target_version.as_str())
                    }
                    Some(SelectedUpdate::ManagerResolved) => manager_resolved_label().to_owned(),
                    Some(SelectedUpdate::Recommended | SelectedUpdate::ForcePlannedCandidate)
                    | None => row.target_version.as_ref().map_or_else(
                        || {
                            if row.target_options.iter().any(|option| {
                                matches!(option, TargetOption::ManagerResolved { .. })
                            }) {
                                manager_resolved_label().to_owned()
                            } else {
                                "unavailable".to_owned()
                            }
                        },
                        |version| version_label(version.as_str()),
                    ),
                };
            let forced = matches!(selected_target, Some(SelectedUpdate::ForcePlannedCandidate))
                || selected_option.is_some_and(TargetOption::has_violation);
            let note_parts = selected_exact_option
                .map_or_else(|| row.notes.clone(), |option| option.note_parts().to_vec());

            SelectionRenderRow {
                selected,
                manager: manager.manager_id.to_string(),
                name: row.package_name.to_string(),
                current: version_label(row.installed_version.as_str()),
                target,
                note_parts,
                forced,
            }
        })
        .collect()
}

fn selected_target_option<'a>(
    row: &'a SelectionRow,
    selected_target: &SelectedUpdate,
) -> Option<&'a TargetOption> {
    row.target_options
        .iter()
        .find(|option| target_option_matches_selected(option, selected_target))
}

fn selection_table_row(
    row: &SelectionRenderRow,
    highlighted: bool,
    theme: &TuiTheme,
) -> Row<'static> {
    let style = theme.row_for_selectable_state(highlighted);
    let marker = if row.selected { "[x]" } else { "[ ]" };
    let target = if row.target == "unavailable" || row.target == manager_resolved_label() {
        Line::from(Span::styled(row.target.clone(), style))
    } else {
        Line::from(version_diff_spans(
            &row.current,
            &row.target,
            style,
            theme,
            highlighted,
        ))
    };
    let note = if row.forced {
        forced_note_cell(&row.note_parts, theme)
    } else {
        Cell::new(note_line(&row.note_parts, theme)).style(theme.note)
    };

    Row::new(vec![
        Cell::new(marker).style(style),
        Cell::new(row.manager.clone()).style(style),
        Cell::new(row.name.clone()).style(theme.emphasis(style)),
        Cell::new(row.current.clone()).style(style),
        Cell::new(target).style(style),
        note,
    ])
    .style(style)
}

fn forced_note_cell(note_parts: &[CandidateNotePart], theme: &TuiTheme) -> Cell<'static> {
    let mut spans = vec![Span::styled("forced", theme.forced)];
    let note = note_text(note_parts);
    if !note.is_empty() {
        spans.push(Span::styled(", ", theme.note));
        spans.push(Span::styled(note, theme.note));
    }

    Cell::new(Line::from(spans)).style(theme.note)
}

fn footer_line(screen: &InteractiveSelectionScreen, width: u16, theme: &TuiTheme) -> Line<'static> {
    if screen.confirmation_dialog_open() {
        return Line::raw("");
    }

    if screen.target_picker_open() {
        return picker_footer_line(theme);
    }

    key_footer(selection_footer_bindings(width), theme)
}

pub(super) const fn selection_footer_bindings(width: u16) -> &'static [KeyBinding<'static>] {
    if width < MINIMAL_FOOTER_WIDTH {
        MINIMAL_FOOTER_KEYS
    } else if width < COMPACT_FOOTER_WIDTH {
        COMPACT_FOOTER_KEYS
    } else {
        FOOTER_KEYS
    }
}

pub(super) const fn selection_footer_inputs(width: u16) -> &'static [Option<SelectionInput>] {
    if width < MINIMAL_FOOTER_WIDTH {
        MINIMAL_FOOTER_INPUTS
    } else if width < COMPACT_FOOTER_WIDTH {
        COMPACT_FOOTER_INPUTS
    } else {
        FOOTER_INPUTS
    }
}

fn picker_footer_line(theme: &TuiTheme) -> Line<'static> {
    key_footer(&[PICKER_MAIN_MOVE_KEY], theme)
}

fn draw_target_picker(
    frame: &mut ratatui::Frame<'_>,
    screen: &InteractiveSelectionScreen,
    picker: TargetPickerState,
    area: Rect,
    theme: &TuiTheme,
) {
    let row = screen.row(picker.visible_row);
    let manager = &screen.managers[picker.visible_row.manager_idx];
    let Some(inner) = render_modal_frame(
        frame,
        area,
        target_picker_width(area),
        target_picker_height(row.target_options.len()),
        None,
        theme,
    ) else {
        return;
    };

    if inner.height < 6 || inner.width < 20 {
        frame.render_widget(Paragraph::new("Terminal too small"), inner);
        return;
    }

    let [
        title_area,
        _,
        policy_area,
        current_area,
        _,
        list_area,
        detail_area,
        footer_area,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .areas(inner);

    let title = Line::from(Span::styled(
        format!(
            "{}: {}",
            manager.manager_id.as_str(),
            row.package_name.as_str()
        ),
        theme.header,
    ))
    .centered();
    frame.render_widget(Paragraph::new(title), title_area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} version policy: ", manager.manager_id.as_str()),
                theme.header,
            ),
            Span::raw(version_policy_dialog_label(manager.version_policy)),
        ])),
        policy_area,
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Current: ", theme.header),
            Span::raw(version_label(row.installed_version.as_str())),
        ])),
        current_area,
    );

    draw_target_picker_rows(frame, screen, picker, list_area, theme);
    draw_target_picker_details(frame, row, picker.cursor, detail_area, theme);
    frame.render_widget(
        Paragraph::new(key_footer(PICKER_FOOTER_KEYS, theme)),
        footer_area,
    );
}

fn draw_confirmation_dialog(
    frame: &mut ratatui::Frame<'_>,
    screen: &InteractiveSelectionScreen,
    area: Rect,
    theme: &TuiTheme,
) {
    let summary = screen.confirmation_summary();
    let Some(inner) = render_modal_frame(
        frame,
        area,
        confirmation_dialog_width(area),
        confirmation_dialog_height(&summary),
        Some(Line::from(Span::styled("Confirm Apply", theme.header))),
        theme,
    ) else {
        return;
    };

    if inner.height < 4 || inner.width < 20 {
        frame.render_widget(Paragraph::new("Terminal too small"), inner);
        return;
    }

    let [body_area, footer_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);
    let body = confirmation_dialog_lines(&summary, theme);

    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), body_area);
    frame.render_widget(
        Paragraph::new(key_footer(CONFIRMATION_FOOTER_KEYS, theme)),
        footer_area,
    );
}

fn confirmation_dialog_lines(
    summary: &ConfirmationSummary,
    theme: &TuiTheme,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("Selected updates: ", theme.header),
        Span::raw(summary.selected_total.to_string()),
    ])];

    if summary.managers.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "No managers selected.",
            theme.muted,
        )));
        return lines;
    }

    lines.push(Line::raw(""));
    for manager in &summary.managers {
        lines.push(Line::from(vec![
            Span::styled(manager.manager.clone(), theme.header),
            Span::raw(format!(": {}", manager.selected_count)),
        ]));
    }

    lines
}

fn confirmation_dialog_height(summary: &ConfirmationSummary) -> u16 {
    let manager_rows = summary.managers.len().max(1);
    let body_rows = manager_rows.saturating_add(3);
    u16::try_from(body_rows.saturating_add(3))
        .unwrap_or(u16::MAX)
        .clamp(7, 18)
}

fn confirmation_dialog_width(area: Rect) -> u16 {
    area.width.saturating_sub(4).clamp(42, 72)
}

fn draw_target_picker_rows(
    frame: &mut ratatui::Frame<'_>,
    screen: &InteractiveSelectionScreen,
    picker: TargetPickerState,
    area: Rect,
    theme: &TuiTheme,
) {
    let row = screen.row(picker.visible_row);
    let selected_target = screen.managers[picker.visible_row.manager_idx]
        .state
        .selected_target(&row.plan_item_id);
    let current = version_label(row.installed_version.as_str());
    let render_rows = target_picker_rows(&row.target_options);
    let table_rows = render_rows
        .iter()
        .enumerate()
        .map(|(idx, render_row)| {
            let selected = selected_target.is_some_and(|target| {
                target_option_matches_selected(&row.target_options[idx], target)
            });
            target_picker_table_row(&current, render_row, selected, idx == picker.cursor, theme)
        })
        .collect::<Vec<_>>();

    let selected = (picker.cursor < render_rows.len()).then_some(picker.cursor);
    render_table(
        frame,
        area,
        TuiTable::new(table_rows, version_picker_columns(area.width))
            .selected(selected)
            .row_highlight_style(theme.selected_row_highlight),
        theme,
    );
}

fn draw_target_picker_details(
    frame: &mut ratatui::Frame<'_>,
    row: &SelectionRow,
    cursor: usize,
    area: Rect,
    theme: &TuiTheme,
) {
    let Some(option) = row.target_options.get(cursor) else {
        return;
    };
    let lines = target_picker_detail_lines(option, theme);
    if lines.is_empty() {
        return;
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
}

fn target_picker_detail_lines(option: &TargetOption, theme: &TuiTheme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for part in option.note_parts() {
        match &part.kind {
            CandidateNoteKind::AuditVulnerable { findings } => {
                for finding in findings.iter().take(2) {
                    let mut ids = vec![finding.id.clone()];
                    ids.extend(finding.aliases.iter().take(2).cloned());
                    lines.push(Line::from(vec![
                        Span::styled("Advisory: ", theme.header),
                        Span::raw(ids.join(", ")),
                    ]));
                    if let Some(summary) = finding.summary.as_ref() {
                        lines.push(Line::from(vec![
                            Span::styled("Summary: ", theme.header),
                            Span::raw(summary.clone()),
                        ]));
                    }
                    if let Some(reference) = finding.references.first() {
                        lines.push(Line::from(vec![
                            Span::styled("Reference: ", theme.header),
                            Span::raw(reference.clone()),
                        ]));
                    }
                }
            }
            CandidateNoteKind::AuditLookupFailed { detail } => {
                lines.push(Line::from(vec![
                    Span::styled("Audit: ", theme.header),
                    Span::raw(detail.clone()),
                ]));
            }
            _ => {}
        }
    }
    lines.truncate(4);
    lines
}

fn target_picker_table_row(
    current: &str,
    row: &TargetPickerRenderRow,
    selected: bool,
    highlighted: bool,
    theme: &TuiTheme,
) -> Row<'static> {
    let style = theme.row_for_selectable_state(highlighted);
    let marker = if selected { "[x]" } else { "[ ]" };
    let target = if row.target == manager_resolved_label() {
        vec![Span::styled(row.target.clone(), style)]
    } else {
        version_diff_spans(current, &row.target, style, theme, highlighted)
    };
    let note = note_line(&row.note_parts, theme);

    Row::new(vec![
        Cell::new(marker).style(style),
        Cell::new(Line::from(target)).style(style),
        Cell::new(note),
    ])
    .style(style)
}

fn target_picker_rows(options: &[TargetOption]) -> Vec<TargetPickerRenderRow> {
    options
        .iter()
        .map(|option| TargetPickerRenderRow {
            target: option.target_version().map_or_else(
                || manager_resolved_label().to_owned(),
                |version| version_label(version.as_str()),
            ),
            note_parts: option.note_parts().to_vec(),
        })
        .collect()
}

pub(super) fn target_picker_height(option_count: usize) -> u16 {
    let body = u16::try_from(option_count.min(10)).unwrap_or(10);
    body.saturating_add(13).clamp(14, 23)
}

pub(super) fn target_picker_width(area: Rect) -> u16 {
    area.width.saturating_sub(4).clamp(62, 96)
}

const fn version_policy_dialog_label(policy: VersionPolicy) -> &'static str {
    match policy {
        VersionPolicy::None => "none",
        VersionPolicy::Stable => "stable",
        VersionPolicy::SameTrack => "same track",
    }
}

fn note_line(note_parts: &[CandidateNotePart], theme: &TuiTheme) -> Line<'static> {
    let mut spans = Vec::new();
    for (idx, part) in note_parts.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled("; ", theme.note));
        }
        spans.push(Span::styled(note_part_text(part), theme.note));
    }

    Line::from(spans)
}

fn note_text(note_parts: &[CandidateNotePart]) -> String {
    note_parts
        .iter()
        .map(note_part_text)
        .collect::<Vec<_>>()
        .join("; ")
}
