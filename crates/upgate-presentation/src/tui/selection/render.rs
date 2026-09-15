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
    spinner_frame, update_header_row, version_picker_columns, visible_tabs,
};
use crate::tui::layout::app_frame;
use crate::tui::text::{truncate_with_ellipsis, version_diff_spans};
use crate::tui::theme::TuiTheme;
use crate::{CandidateNoteKind, CandidateNotePart, SelectionRow, TargetOption};

pub(super) const TAB_KEY_LABEL: &str = " ⇥ ";
const PICKER_MAIN_MOVE_KEY: KeyBinding<'static> = KeyBinding {
    key: "J/K",
    label: "row",
};
const CONFIRMATION_FOOTER_KEYS: &[KeyBinding<'static>] = &[
    KeyBinding {
        key: "C",
        label: "confirm",
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

#[derive(Debug)]
struct SelectionRenderRow {
    selected: bool,
    removed: bool,
    manager: String,
    name: String,
    current: String,
    target: String,
    note_parts: Vec<CandidateNotePart>,
    forced: bool,
}

pub(super) fn draw_selection(
    frame: &mut ratatui::Frame<'_>,
    screen: &mut InteractiveSelectionScreen,
) {
    let theme = TuiTheme::current();
    let area = frame.area();
    let block = app_block(&theme);
    let Some(app_frame) = app_frame(area).filter(|_| area.width >= 20) else {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new("Terminal too small"), inner);
        return;
    };
    frame.render_widget(block, app_frame.outer);

    draw_tabs(frame, screen, app_frame.header, &theme);
    render_separator(frame, app_frame.header_separator, &theme);

    draw_selection_body(frame, screen, app_frame.body, &theme);

    render_separator(frame, app_frame.footer_separator, &theme);
    frame.render_widget(
        Paragraph::new(footer_line(screen, app_frame.footer.width, &theme)),
        app_frame.footer,
    );

    if let Some(picker) = screen.target_picker() {
        draw_target_picker(frame, screen, picker, app_frame.outer, &theme);
    }
    if screen.confirmation_dialog_open() {
        draw_confirmation_dialog(frame, screen, app_frame.outer, &theme);
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
    let [area, status_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let summary = screen.confirmation_summary();
    let status = if let Some(query) = &screen.search_query {
        screen.feedback.as_ref().map_or_else(
            || format!("/{query}"),
            |feedback| format!("{feedback} · /{query}"),
        )
    } else {
        screen.feedback.clone().unwrap_or_else(|| {
            format!(
                "{} updates · {} removals",
                summary.selected_total - summary.removals.len(),
                summary.removals.len()
            )
        })
    };
    frame.render_widget(Paragraph::new(status), status_area);
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
    render_tabs(frame, tabs_area, tabs, theme);
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
    if screen.managers.iter().any(|manager| {
        matches!(
            manager.planning_status,
            ManagerPlanningStatus::Planning | ManagerPlanningStatus::Waiting
        )
    }) {
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
    screen.keep_cursor_visible(usize::from(area.height.saturating_sub(1)));
    let render_rows = selection_render_rows(screen);
    let row_count = render_rows.len();
    let table_rows = render_rows
        .into_iter()
        .enumerate()
        .map(|(idx, row)| selection_table_row(row, screen.cursor() == Some(idx), theme))
        .collect::<Vec<_>>();

    let selected = screen.cursor().filter(|cursor| *cursor < row_count);
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
    usize::from(area.height.saturating_sub(2))
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

            let removed = manager.state.is_removed(&row.plan_item_id);
            SelectionRenderRow {
                selected,
                removed,
                manager: manager.manager_id.as_str().to_owned(),
                name: row.package_name.as_str().to_owned(),
                current: version_label(row.installed_version.as_str()),
                target: if removed { "remove".to_owned() } else { target },
                note_parts: if removed { Vec::new() } else { note_parts },
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
    row: SelectionRenderRow,
    highlighted: bool,
    theme: &TuiTheme,
) -> Row<'static> {
    let style = theme.row_for_selectable_state(highlighted);
    let marker = if row.removed {
        " − "
    } else if row.selected {
        " ↑ "
    } else {
        "   "
    };
    let target =
        if row.removed || row.target == "unavailable" || row.target == manager_resolved_label() {
            Line::from(Span::styled(row.target, style))
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
        Cell::new(row.manager).style(style),
        Cell::new(row.name).style(theme.emphasis(style)),
        Cell::new(row.current).style(style),
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

    if screen.search_query.is_some() {
        let bindings = [
            KeyBinding {
                key: "↑/↓",
                label: "matches",
            },
            KeyBinding {
                key: "esc/enter",
                label: "done",
            },
        ];
        let footer = key_footer(&bindings, theme);
        return if footer.width() <= usize::from(width) {
            footer
        } else {
            key_footer(&bindings[1..], theme)
        };
    }

    if screen.target_picker_open() {
        return picker_footer_line(theme);
    }

    render_footer(&selection_footer(screen, width), theme)
}

pub(super) fn selection_footer(
    screen: &InteractiveSelectionScreen,
    width: u16,
) -> Vec<(KeyBinding<'static>, SelectionInput)> {
    let mut entries = vec![(
        KeyBinding {
            key: "j/k",
            label: "move",
        },
        SelectionInput::Ignore,
    )];
    if screen.planning_finished {
        entries.push((
            KeyBinding {
                key: "/",
                label: "search",
            },
            SelectionInput::OpenSearch,
        ));
    }
    if let Some(visible) = screen.current_visible_row() {
        append_row_actions(&mut entries, screen, visible);
    }
    entries.push((
        KeyBinding {
            key: "a/n",
            label: "all/none",
        },
        SelectionInput::Ignore,
    ));
    entries.push((
        KeyBinding {
            key: "v",
            label: if screen.show_all {
                "hide all"
            } else {
                "show all"
            },
        },
        SelectionInput::ToggleViewAll,
    ));
    if screen.current_visible_row().is_some() {
        entries.push((
            KeyBinding {
                key: "enter",
                label: "details",
            },
            SelectionInput::OpenTargetPicker,
        ));
    }
    entries.push((
        KeyBinding {
            key: "C",
            label: "confirm",
        },
        SelectionInput::Confirm,
    ));
    entries.push((
        KeyBinding {
            key: "q",
            label: "quit",
        },
        SelectionInput::Cancel,
    ));
    fit_footer(entries, width)
}

fn append_row_actions(
    entries: &mut Vec<(KeyBinding<'static>, SelectionInput)>,
    screen: &InteractiveSelectionScreen,
    visible: super::screen::VisibleRow,
) {
    let row = screen.row(visible);
    let state = &screen.managers[visible.manager_idx].state;
    let selected =
        state.is_removed(&row.plan_item_id) || state.selected_target(&row.plan_item_id).is_some();
    let update_available = row.status == crate::SelectionRowStatus::Update
        || row
            .target_options
            .iter()
            .any(|option| matches!(option, TargetOption::ForcedCandidate { .. }));
    if selected || update_available {
        entries.push((
            KeyBinding {
                key: "space/x",
                label: if selected { "deselect" } else { "update" },
            },
            SelectionInput::ToggleCurrent,
        ));
    }
    if !state.is_removed(&row.plan_item_id)
        && matches!(row.removal, upgate_domain::RemovalSupport::Supported(_))
    {
        entries.push((
            KeyBinding {
                key: "d",
                label: "remove",
            },
            SelectionInput::ToggleRemoval,
        ));
    }
}

pub(super) fn picker_actions(
    screen: &InteractiveSelectionScreen,
    width: u16,
) -> Vec<(KeyBinding<'static>, SelectionInput)> {
    let mut entries = Vec::new();
    if let Some(picker) = screen.target_picker() {
        entries.push((
            KeyBinding {
                key: "j/k",
                label: "target",
            },
            SelectionInput::Ignore,
        ));
        append_row_actions(&mut entries, screen, picker.visible_row);
        if !screen.managers[picker.visible_row.manager_idx]
            .state
            .is_removed(&screen.row(picker.visible_row).plan_item_id)
            && screen
                .row(picker.visible_row)
                .target_options
                .iter()
                .any(|option| matches!(option, TargetOption::Recommended { .. }))
        {
            entries.push((
                KeyBinding {
                    key: "r",
                    label: "recommended",
                },
                SelectionInput::RecommendedTarget,
            ));
        }
    }
    entries.push((
        KeyBinding {
            key: "esc",
            label: "cancel",
        },
        SelectionInput::PickerCancel,
    ));
    entries.push((
        KeyBinding {
            key: "enter",
            label: "select",
        },
        SelectionInput::PickerConfirm,
    ));
    fit_footer(entries, width)
}

fn fit_footer(
    mut entries: Vec<(KeyBinding<'static>, SelectionInput)>,
    width: u16,
) -> Vec<(KeyBinding<'static>, SelectionInput)> {
    let theme = TuiTheme::current();
    for key in ["j/k", "a/n", "r", "enter", "d", "v", "/", "q"] {
        if render_footer(&entries, &theme).width() <= usize::from(width) {
            break;
        }
        if let Some(index) = entries
            .iter()
            .position(|(binding, _)| binding.key == key && binding.label != "select")
        {
            entries.remove(index);
        }
    }
    if render_footer(&entries, &theme).width() > usize::from(width)
        && let Some((binding, _)) = entries
            .iter_mut()
            .find(|(binding, _)| binding.key == "space/x")
    {
        binding.key = "x";
    }
    for key in ["x", "esc"] {
        if render_footer(&entries, &theme).width() <= usize::from(width) {
            break;
        }
        if let Some(index) = entries.iter().position(|(binding, _)| binding.key == key) {
            entries.remove(index);
        }
    }
    entries
}

fn render_footer(entries: &[(KeyBinding<'_>, SelectionInput)], theme: &TuiTheme) -> Line<'static> {
    let bindings = entries
        .iter()
        .map(|(binding, _)| KeyBinding {
            key: binding.key,
            label: binding.label,
        })
        .collect::<Vec<_>>();
    key_footer(&bindings, theme)
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
        target_picker_height(row.target_options.len() + 1),
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
        Paragraph::new(render_footer(
            &picker_actions(screen, footer_area.width),
            theme,
        )),
        footer_area,
    );
}

fn draw_confirmation_dialog(
    frame: &mut ratatui::Frame<'_>,
    screen: &mut InteractiveSelectionScreen,
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

    let body = Paragraph::new(body).wrap(Wrap { trim: true });
    let height = body.line_count(body_area.width);
    let maximum = height.saturating_sub(usize::from(body_area.height));
    screen.confirmation_scroll = screen
        .confirmation_scroll
        .min(u16::try_from(maximum).unwrap_or(u16::MAX));
    frame.render_widget(body.scroll((screen.confirmation_scroll, 0)), body_area);
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
        Span::styled("Apply: ", theme.header),
        Span::raw(format!(
            "{} updates · {} removals",
            summary.selected_total - summary.removals.len(),
            summary.removals.len()
        )),
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

    if !summary.removals.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::raw("Remove:"));
        lines.extend(summary.removals.iter().cloned().map(Line::raw));
        lines.push(Line::raw(""));
        lines.push(Line::raw(
            "Removal clears the item’s upgate selection preference.",
        ));
        lines.push(Line::raw("↑/↓ scroll review"));
    }
    lines
}

fn confirmation_dialog_height(summary: &ConfirmationSummary) -> u16 {
    let manager_rows = (summary.managers.len() + summary.removals.len() + 5).max(1);
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
    let mut table_rows = row
        .target_options
        .iter()
        .enumerate()
        .map(|(idx, option)| {
            let selected = selected_target
                .is_some_and(|target| target_option_matches_selected(option, target));
            let target = option.target_version().map_or_else(
                || manager_resolved_label().to_owned(),
                |version| version_label(version.as_str()),
            );
            target_picker_table_row(
                &current,
                target,
                option.note_parts(),
                selected,
                idx == picker.cursor,
                theme,
            )
        })
        .collect::<Vec<_>>();

    let removing = screen.managers[picker.visible_row.manager_idx]
        .state
        .is_removed(&row.plan_item_id);
    table_rows.push(Row::new(vec![
        Cell::new(if removing { " − " } else { "   " }),
        Cell::new(if removing { "Undo removal" } else { "Remove" }),
        Cell::new(row.removal_label()),
    ]));
    let selected = Some(picker.cursor);
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
        frame.render_widget(
            Paragraph::new(row.removal_label()).wrap(Wrap { trim: true }),
            area,
        );
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
                    let ids = std::iter::once(finding.id.as_str())
                        .chain(finding.aliases.iter().take(2).map(String::as_str))
                        .collect::<Vec<_>>();
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
    target: String,
    note_parts: &[CandidateNotePart],
    selected: bool,
    highlighted: bool,
    theme: &TuiTheme,
) -> Row<'static> {
    let style = theme.row_for_selectable_state(highlighted);
    let marker = if selected { " ↑ " } else { "   " };
    let target_spans = if target == manager_resolved_label() {
        vec![Span::styled(target, style)]
    } else {
        version_diff_spans(current, &target, style, theme, highlighted)
    };
    let note = note_line(note_parts, theme);

    Row::new(vec![
        Cell::new(marker).style(style),
        Cell::new(Line::from(target_spans)).style(style),
        Cell::new(note),
    ])
    .style(style)
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
