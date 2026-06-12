use std::io;
use std::time::Duration;

use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use unicode_width::UnicodeWidthStr;

use super::render::{
    PICKER_FOOTER_KEYS, TAB_KEY_LABEL, selection_footer_bindings, selection_footer_inputs,
    selection_tab_titles, selection_table_visible_height, target_picker_height,
    target_picker_width,
};
use super::screen::{InteractiveSelectionScreen, TargetPickerState};
use super::{MAX_DRAINED_INPUT_EVENTS, SelectionControl, SelectionInput};
use crate::SelectionRow;
use crate::tui::components::{command_log_layout, key_footer_hit, visible_tabs};
use crate::tui::layout::app_frame;
use crate::tui::selection_state::SelectionStateError;
use crate::tui::theme::TuiTheme;

#[derive(Debug, Default)]
struct SelectionScrollDeltas {
    main: isize,
    command_log: isize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionScrollTarget {
    Main,
    CommandLog,
}

pub(super) fn handle_selection_ready_events(
    screen: &mut InteractiveSelectionScreen,
    area: Rect,
) -> io::Result<Result<SelectionControl, SelectionStateError>> {
    let mut scrolls = SelectionScrollDeltas::default();
    let first_event = event::read()?;
    match handle_selection_drained_event(&first_event, screen, area, &mut scrolls) {
        Ok(SelectionControl::Continue) => {}
        control => return Ok(control),
    }

    for _ in 1..MAX_DRAINED_INPUT_EVENTS {
        if !event::poll(Duration::ZERO)? {
            break;
        }
        let event = event::read()?;
        match handle_selection_drained_event(&event, screen, area, &mut scrolls) {
            Ok(SelectionControl::Continue) => {}
            control => return Ok(control),
        }
    }

    flush_selection_scrolls(screen, area, &mut scrolls);
    Ok(Ok(SelectionControl::Continue))
}

fn handle_selection_drained_event(
    event: &Event,
    screen: &mut InteractiveSelectionScreen,
    area: Rect,
    scrolls: &mut SelectionScrollDeltas,
) -> Result<SelectionControl, SelectionStateError> {
    if let Some((target, delta)) = selection_scroll_delta(event, screen, area) {
        match target {
            SelectionScrollTarget::Main => scrolls.main += delta,
            SelectionScrollTarget::CommandLog => scrolls.command_log += delta,
        }
        return Ok(SelectionControl::Continue);
    }

    if is_ignored_mouse_event(event) {
        return Ok(SelectionControl::Continue);
    }

    flush_selection_scrolls(screen, area, scrolls);
    handle_selection_event(event, screen, area)
}

fn selection_scroll_delta(
    event: &Event,
    screen: &InteractiveSelectionScreen,
    area: Rect,
) -> Option<(SelectionScrollTarget, isize)> {
    let Event::Mouse(mouse) = event else {
        return None;
    };
    let delta = match mouse.kind {
        MouseEventKind::ScrollUp => -1,
        MouseEventKind::ScrollDown => 1,
        _ => return None,
    };
    if screen.target_picker_open() || screen.confirmation_dialog_open() {
        return None;
    }
    let app_frame = app_frame(area)?;
    let selection_body = selection_body_areas(screen.trace_commands, app_frame.body);
    if let Some(log_area) = selection_body.log
        && rect_contains(log_area, mouse.column, mouse.row)
    {
        return Some((SelectionScrollTarget::CommandLog, -delta));
    }
    rect_contains(selection_body.main, mouse.column, mouse.row)
        .then_some((SelectionScrollTarget::Main, delta))
}

fn flush_selection_scrolls(
    screen: &mut InteractiveSelectionScreen,
    area: Rect,
    scrolls: &mut SelectionScrollDeltas,
) {
    if let Some(app_frame) = app_frame(area) {
        let selection_body = selection_body_areas(screen.trace_commands, app_frame.body);
        if scrolls.main != 0 {
            screen.scroll_table_by(
                scrolls.main,
                selection_table_visible_height(selection_body.main),
            );
        }
        if scrolls.command_log != 0
            && let Some(log_area) = selection_body.log
        {
            screen.scroll_command_log_by(scrolls.command_log, usize::from(log_area.height));
        }
    }
    scrolls.main = 0;
    scrolls.command_log = 0;
}

const fn is_ignored_mouse_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved
                | MouseEventKind::Up(_)
                | MouseEventKind::Drag(_)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight,
            ..
        })
    )
}

fn handle_selection_event(
    event: &Event,
    screen: &mut InteractiveSelectionScreen,
    area: Rect,
) -> Result<SelectionControl, SelectionStateError> {
    if screen.confirmation_dialog_open() {
        return Ok(handle_confirmation_dialog_event(event, screen));
    }

    if let Event::Mouse(mouse) = event {
        return handle_selection_mouse(screen, *mouse, area);
    }

    let input = selection_input_from_event(event, screen.target_picker_open());
    screen.handle_input(input)
}

fn handle_confirmation_dialog_event(
    event: &Event,
    screen: &mut InteractiveSelectionScreen,
) -> SelectionControl {
    let Event::Key(key) = event else {
        return SelectionControl::Continue;
    };
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return SelectionControl::Continue;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return SelectionControl::Interrupt;
    }

    match key.code {
        KeyCode::Char('q' | 'Q') => SelectionControl::Cancel,
        KeyCode::Esc => {
            screen.close_confirmation_dialog();
            SelectionControl::Continue
        }
        KeyCode::Char('C') | KeyCode::Enter => SelectionControl::Confirm,
        _ => SelectionControl::Continue,
    }
}

fn handle_selection_mouse(
    screen: &mut InteractiveSelectionScreen,
    mouse: MouseEvent,
    area: Rect,
) -> Result<SelectionControl, SelectionStateError> {
    let Some(app_frame) = app_frame(area) else {
        return Ok(SelectionControl::Continue);
    };

    if screen.target_picker_open() {
        return handle_target_picker_mouse(screen, mouse, app_frame.outer);
    }

    let selection_body = selection_body_areas(screen.trace_commands, app_frame.body);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if rect_contains(app_frame.header, mouse.column, mouse.row) {
                handle_selection_tab_click(screen, mouse.column, app_frame.header)?;
            } else if rect_contains(app_frame.footer, mouse.column, mouse.row) {
                if let Some(input) = selection_footer_input(mouse.column, app_frame.footer) {
                    return screen.handle_input(input);
                }
            } else if rect_contains(selection_body.main, mouse.column, mouse.row)
                && let Some(row_idx) =
                    selection_row_index_at(screen, selection_body.main, mouse.row)
            {
                screen.set_cursor_row(row_idx);
                if selection_checkbox_hit(selection_body.main, mouse.column) {
                    return screen.handle_input(SelectionInput::ToggleCurrent);
                }
                return screen.handle_input(SelectionInput::OpenTargetPicker);
            }
        }
        MouseEventKind::Down(_)
        | MouseEventKind::Up(_)
        | MouseEventKind::Drag(_)
        | MouseEventKind::Moved
        | MouseEventKind::ScrollUp
        | MouseEventKind::ScrollDown
        | MouseEventKind::ScrollLeft
        | MouseEventKind::ScrollRight => {}
    }

    Ok(SelectionControl::Continue)
}

fn handle_target_picker_mouse(
    screen: &mut InteractiveSelectionScreen,
    mouse: MouseEvent,
    area: Rect,
) -> Result<SelectionControl, SelectionStateError> {
    let Some(picker) = screen.target_picker() else {
        return Ok(SelectionControl::Continue);
    };
    let row = screen.row(picker.visible_row);
    let Some(inner) = target_picker_inner_rect(area, row.target_options.len()) else {
        return Ok(SelectionControl::Continue);
    };
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return Ok(SelectionControl::Continue);
    }
    if !rect_contains(inner, mouse.column, mouse.row) {
        return screen.handle_input(SelectionInput::PickerCancel);
    }

    let [_, _, _, _, list_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    if rect_contains(list_area, mouse.column, mouse.row)
        && let Some(option_idx) = target_picker_option_index_at(row, picker, list_area, mouse.row)
    {
        screen.set_target_picker_cursor(option_idx);
        return screen.handle_input(SelectionInput::PickerConfirm);
    }

    if !rect_contains(footer_area, mouse.column, mouse.row) {
        return Ok(SelectionControl::Continue);
    }

    let Some(hit) = key_footer_hit(PICKER_FOOTER_KEYS, mouse.column - footer_area.x) else {
        return Ok(SelectionControl::Continue);
    };
    let input = match hit {
        1 => SelectionInput::RecommendedTarget,
        2 => SelectionInput::PickerCancel,
        3 => SelectionInput::PickerConfirm,
        _ => SelectionInput::Ignore,
    };
    screen.handle_input(input)
}

fn target_picker_option_index_at(
    row: &SelectionRow,
    picker: TargetPickerState,
    area: Rect,
    mouse_row: u16,
) -> Option<usize> {
    if area.is_empty() || mouse_row < area.y || mouse_row >= area.bottom() {
        return None;
    }
    let option_count = row.target_options.len();
    let visible_height = usize::from(area.height);
    let offset = table_offset_for_selected(option_count, picker.cursor, visible_height);
    let row_in_view = usize::from(mouse_row - area.y);
    let option_idx = offset + row_in_view;
    (option_idx < option_count).then_some(option_idx)
}

struct SelectionBodyAreas {
    main: Rect,
    log: Option<Rect>,
}

fn selection_body_areas(trace_commands: bool, area: Rect) -> SelectionBodyAreas {
    command_log_layout(trace_commands, area).map_or(
        SelectionBodyAreas {
            main: area,
            log: None,
        },
        |layout| SelectionBodyAreas {
            main: layout.main,
            log: Some(layout.log),
        },
    )
}

fn handle_selection_tab_click(
    screen: &mut InteractiveSelectionScreen,
    column: u16,
    area: Rect,
) -> Result<(), SelectionStateError> {
    let tab_key_width = UnicodeWidthStr::width(TAB_KEY_LABEL);
    let key_area_width = u16::try_from(tab_key_width)
        .unwrap_or(u16::MAX)
        .min(area.width);
    let [tabs_area, key_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(key_area_width)]).areas(area);

    if rect_contains(key_area, column, area.y) {
        screen.handle_input(SelectionInput::NextTab)?;
        return Ok(());
    }

    let theme = TuiTheme::current();
    let titles = selection_tab_titles(screen, &theme);
    let tabs = visible_tabs(
        &titles,
        screen.active_tab,
        screen.tab_offset(),
        tabs_area.width,
    );
    let left_hint_width = if tabs.has_left_overflow {
        tabs_area.width.min(5)
    } else {
        0
    };
    let [left_area, visible_tabs_area] =
        Layout::horizontal([Constraint::Length(left_hint_width), Constraint::Fill(1)])
            .areas(tabs_area);
    if tabs.has_left_overflow && rect_contains(left_area, column, area.y) {
        screen.handle_input(SelectionInput::PreviousTab)?;
        return Ok(());
    }
    if !rect_contains(visible_tabs_area, column, area.y) {
        return Ok(());
    }

    let mut cursor = visible_tabs_area.x;
    for (idx, title) in tabs.titles.iter().enumerate() {
        let width = u16::try_from(title.width().saturating_add(2)).unwrap_or(u16::MAX);
        if column >= cursor && column < cursor.saturating_add(width) {
            screen.select_tab(tabs.start + idx);
            return Ok(());
        }
        cursor = cursor.saturating_add(width);
    }
    Ok(())
}

fn selection_footer_input(column: u16, area: Rect) -> Option<SelectionInput> {
    let bindings = selection_footer_bindings(area.width);
    selection_footer_inputs(area.width)
        .get(key_footer_hit(bindings, column - area.x)?)
        .copied()
        .flatten()
}

const fn selection_checkbox_hit(area: Rect, column: u16) -> bool {
    column >= area.x && column < area.x.saturating_add(4)
}

fn selection_row_index_at(
    screen: &InteractiveSelectionScreen,
    area: Rect,
    row: u16,
) -> Option<usize> {
    if area.height < 2 || row <= area.y || row >= area.bottom() {
        return None;
    }
    let rows = screen.visible_row_refs();
    let row_in_view = usize::from(row - area.y - 1);
    let row_idx = screen.table_offset + row_in_view;
    (row_idx < rows.len()).then_some(row_idx)
}

fn table_offset_for_selected(row_count: usize, selected: usize, visible_height: usize) -> usize {
    let max_offset = row_count.saturating_sub(visible_height);
    selected
        .saturating_sub(visible_height.saturating_sub(1))
        .min(max_offset)
}

fn target_picker_inner_rect(area: Rect, option_count: usize) -> Option<Rect> {
    if area.is_empty() {
        return None;
    }
    let width = target_picker_width(area).min(area.width);
    let height = target_picker_height(option_count).min(area.height);
    if width == 0 || height == 0 {
        return None;
    }
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    Some(popup.inner(Margin {
        vertical: 1,
        horizontal: 1,
    }))
}

const fn rect_contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

fn selection_input_from_event(event: &Event, target_picker_open: bool) -> SelectionInput {
    let Event::Key(key) = event else {
        return SelectionInput::Ignore;
    };
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return SelectionInput::Ignore;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return SelectionInput::Interrupt;
    }

    let input = match key.code {
        KeyCode::Char('q' | 'Q') => SelectionInput::Cancel,
        #[expect(
            clippy::match_same_arms,
            reason = "Esc is intentionally reserved for target-picker handling below"
        )]
        KeyCode::Esc => SelectionInput::Ignore,
        KeyCode::Char('C') => SelectionInput::Confirm,
        KeyCode::Up | KeyCode::Char('k' | 'K') => SelectionInput::Up,
        KeyCode::Down | KeyCode::Char('j' | 'J') => SelectionInput::Down,
        KeyCode::Tab => SelectionInput::NextTab,
        KeyCode::BackTab => SelectionInput::PreviousTab,
        KeyCode::Char(' ' | 'x' | 'X') => SelectionInput::ToggleCurrent,
        KeyCode::Char('a' | 'A') => SelectionInput::SelectVisible,
        KeyCode::Char('n' | 'N') => SelectionInput::SelectNoneVisible,
        KeyCode::Char('v' | 'V') => SelectionInput::ToggleViewAll,
        KeyCode::Char('r' | 'R') => SelectionInput::RecommendedTarget,
        KeyCode::Enter => SelectionInput::OpenTargetPicker,
        _ => SelectionInput::Ignore,
    };

    if target_picker_open {
        match key.code {
            KeyCode::Char('K') => return SelectionInput::PickerPreviousRow,
            KeyCode::Char('J') => return SelectionInput::PickerNextRow,
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                return SelectionInput::PickerPreviousRow;
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                return SelectionInput::PickerNextRow;
            }
            _ => {}
        }

        match input {
            SelectionInput::Up => SelectionInput::PickerUp,
            SelectionInput::Down => SelectionInput::PickerDown,
            SelectionInput::OpenTargetPicker => SelectionInput::PickerConfirm,
            SelectionInput::Ignore if key.code == KeyCode::Esc => SelectionInput::PickerCancel,
            SelectionInput::Cancel => SelectionInput::Ignore,
            _ => input,
        }
    } else {
        input
    }
}
