use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row};
use upgate_execution::ResolvedExecutionTarget;
use upgate_execution::progress::{
    ExecutionProgressEvent, ExecutionProgressRow, ExecutionProgressState, ExecutionProgressStatus,
    ExecutionProgressSummary,
};

use crate::outcome::{manager_resolved_label, version_label};
use crate::tui::components::{
    KeyBinding, TuiTable, app_block, clamp_command_log_scroll, command_log_layout, key_footer,
    key_footer_hit, progress_update_columns, render_command_log, render_modal_frame,
    render_separator, render_table, update_header_row,
};
use crate::tui::layout::app_frame;
use crate::tui::theme::TuiTheme;

const TICK_RATE: Duration = Duration::from_millis(120);
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const FOOTER_KEYS: &[KeyBinding<'static>] = &[KeyBinding {
    key: "q",
    label: "quit",
}];
const QUIT_CONFIRM_KEYS: &[KeyBinding<'static>] = &[
    KeyBinding {
        key: "enter y",
        label: "quit later",
    },
    KeyBinding {
        key: "esc n",
        label: "cancel",
    },
];
const MAX_DRAINED_INPUT_EVENTS: usize = 256;
const QUIT_DIALOG_WIDTH: u16 = 54;
const QUIT_DIALOG_HEIGHT: u16 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressInput {
    Quit,
    ConfirmQuitAfterCurrent,
    CancelQuit,
    Interrupt,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveProgressOutcome {
    Finished(ExecutionProgressSummary),
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InteractiveProgressScreen {
    state: ExecutionProgressState,
    phase: ProgressPhase,
    spinner_tick: usize,
    trace_commands: bool,
    command_log_scroll_from_bottom: usize,
    table_offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressPhase {
    Running,
    QuitConfirm,
    Result,
    Done,
    Interrupted,
}

impl InteractiveProgressScreen {
    const fn new(state: ExecutionProgressState, trace_commands: bool) -> Self {
        Self {
            state,
            phase: ProgressPhase::Running,
            spinner_tick: 0,
            trace_commands,
            command_log_scroll_from_bottom: 0,
            table_offset: 0,
        }
    }
    const fn quit_confirmation_open(&self) -> bool {
        matches!(self.phase, ProgressPhase::QuitConfirm)
    }
    const fn finished(&self) -> bool {
        matches!(self.phase, ProgressPhase::Result | ProgressPhase::Done)
    }

    const fn should_exit(&self) -> bool {
        matches!(self.phase, ProgressPhase::Done | ProgressPhase::Interrupted)
    }

    const fn interrupted(&self) -> bool {
        matches!(self.phase, ProgressPhase::Interrupted)
    }

    const fn result_open(&self) -> bool {
        matches!(self.phase, ProgressPhase::Result)
    }

    const fn tick(&mut self) {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
    }

    fn apply_event(&mut self, event: ExecutionProgressEvent) {
        let finished = matches!(event, ExecutionProgressEvent::Finished);
        let command_log_len = self.state.command_log.len();
        self.state.apply_event(event);
        if self.command_log_scroll_from_bottom > 0 {
            self.command_log_scroll_from_bottom = self
                .command_log_scroll_from_bottom
                .saturating_add(self.state.command_log.len().saturating_sub(command_log_len));
        }
        if finished {
            self.phase = ProgressPhase::Result;
        }
    }

    fn handle_input(&mut self, input: ProgressInput, stop_requested: &AtomicBool) {
        match (self.phase, input) {
            (ProgressPhase::QuitConfirm, ProgressInput::ConfirmQuitAfterCurrent) => {
                stop_requested.store(true, Ordering::Relaxed);
                self.apply_event(ExecutionProgressEvent::StopAfterCurrentRequested);
                self.phase = ProgressPhase::Running;
            }
            (ProgressPhase::QuitConfirm, ProgressInput::CancelQuit) => {
                self.phase = ProgressPhase::Running;
            }
            (ProgressPhase::Running, ProgressInput::Quit) => {
                if self.has_running_rows() && !stop_requested.load(Ordering::Relaxed) {
                    self.phase = ProgressPhase::QuitConfirm;
                } else {
                    stop_requested.store(true, Ordering::Relaxed);
                    self.apply_event(ExecutionProgressEvent::StopAfterCurrentRequested);
                }
            }
            (ProgressPhase::Result, ProgressInput::Quit) => {
                self.phase = ProgressPhase::Done;
            }
            (_, ProgressInput::Interrupt) => {
                self.phase = ProgressPhase::Interrupted;
            }
            _ => {}
        }
    }

    fn has_running_rows(&self) -> bool {
        self.state
            .rows
            .iter()
            .any(|row| matches!(row.status, ExecutionProgressStatus::Running))
    }

    fn scroll_command_log_by(&mut self, delta: isize, visible_height: usize) {
        let next_scroll = self
            .command_log_scroll_from_bottom
            .saturating_add_signed(delta);
        self.command_log_scroll_from_bottom =
            clamp_command_log_scroll(next_scroll, self.state.command_log.len(), visible_height);
    }

    fn clamp_command_log_scroll(&mut self, visible_height: usize) {
        self.command_log_scroll_from_bottom = clamp_command_log_scroll(
            self.command_log_scroll_from_bottom,
            self.state.command_log.len(),
            visible_height,
        );
    }

    fn scroll_table_by(&mut self, delta: isize, visible_height: usize) {
        let next_offset = self.table_offset.saturating_add_signed(delta);
        self.table_offset = next_offset.min(progress_table_max_offset(
            self.progress_table_row_count(),
            visible_height,
        ));
    }

    fn clamp_table_offset(&mut self, visible_height: usize) {
        self.table_offset = self.table_offset.min(progress_table_max_offset(
            self.progress_table_row_count(),
            visible_height,
        ));
    }

    fn progress_table_row_count(&self) -> usize {
        let row_count = self.state.rows.len() + self.state.manager_failures.len();
        row_count.max(1)
    }
}

/// Runs the fullscreen interactive progress UI over a typed progress event stream.
///
/// # Errors
///
/// Returns an I/O error for terminal setup, rendering, event reading, or cleanup failures.
pub fn run_interactive_progress(
    state: ExecutionProgressState,
    rx: &Receiver<ExecutionProgressEvent>,
    stop_requested: &AtomicBool,
    trace_commands: bool,
) -> io::Result<InteractiveProgressOutcome> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    if let Err(err) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
        return Err(err);
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(err) => {
            let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(err);
        }
    };
    let mut screen = InteractiveProgressScreen::new(state, trace_commands);

    let result = run_progress_loop(&mut terminal, &mut screen, rx, stop_requested);

    let cleanup = cleanup_terminal(&mut terminal);
    match (result, cleanup) {
        (Ok(()), Ok(())) if screen.interrupted() => Ok(InteractiveProgressOutcome::Interrupted),
        (Ok(()), Ok(())) => Ok(InteractiveProgressOutcome::Finished(screen.state.summary())),
        (Err(err), Ok(()) | Err(_)) | (Ok(()), Err(err)) => Err(err),
    }
}

fn run_progress_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    screen: &mut InteractiveProgressScreen,
    rx: &Receiver<ExecutionProgressEvent>,
    stop_requested: &AtomicBool,
) -> io::Result<()> {
    loop {
        drain_progress_events(rx, screen);
        terminal.draw(|frame| draw_progress(frame, screen))?;
        if screen.should_exit() {
            break;
        }

        if event::poll(TICK_RATE)? {
            let size = terminal.size()?;
            let area = Rect::new(0, 0, size.width, size.height);
            handle_progress_ready_events(screen, stop_requested, area)?;
        }

        screen.tick();
    }
    Ok(())
}

fn cleanup_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let raw_mode = disable_raw_mode();
    let screen = execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    );
    let cursor = terminal.show_cursor();
    raw_mode?;
    screen?;
    cursor
}

fn drain_progress_events(
    rx: &Receiver<ExecutionProgressEvent>,
    screen: &mut InteractiveProgressScreen,
) {
    while let Ok(event) = rx.try_recv() {
        screen.apply_event(event);
    }
}

#[derive(Debug, Default)]
struct ProgressScrollDeltas {
    table: isize,
    command_log: isize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressScrollTarget {
    Table,
    CommandLog,
}

fn handle_progress_ready_events(
    screen: &mut InteractiveProgressScreen,
    stop_requested: &AtomicBool,
    area: Rect,
) -> io::Result<()> {
    let mut scrolls = ProgressScrollDeltas::default();
    let first_event = event::read()?;
    handle_progress_drained_event(&first_event, screen, stop_requested, area, &mut scrolls);

    for _ in 1..MAX_DRAINED_INPUT_EVENTS {
        if !event::poll(Duration::ZERO)? {
            break;
        }
        let event = event::read()?;
        handle_progress_drained_event(&event, screen, stop_requested, area, &mut scrolls);
    }
    flush_progress_scrolls(screen, area, &mut scrolls);
    Ok(())
}

fn handle_progress_drained_event(
    event: &Event,
    screen: &mut InteractiveProgressScreen,
    stop_requested: &AtomicBool,
    area: Rect,
    scrolls: &mut ProgressScrollDeltas,
) {
    if let Some((target, delta)) = progress_scroll_delta(event, screen, area) {
        match target {
            ProgressScrollTarget::Table => scrolls.table += delta,
            ProgressScrollTarget::CommandLog => scrolls.command_log += delta,
        }
        return;
    }

    if is_ignored_mouse_event(event) {
        return;
    }

    flush_progress_scrolls(screen, area, scrolls);
    handle_progress_event(event, screen, stop_requested, area);
}

fn progress_scroll_delta(
    event: &Event,
    screen: &InteractiveProgressScreen,
    area: Rect,
) -> Option<(ProgressScrollTarget, isize)> {
    let Event::Mouse(mouse) = event else {
        return None;
    };
    let table_delta = match mouse.kind {
        MouseEventKind::ScrollUp => -1,
        MouseEventKind::ScrollDown => 1,
        _ => return None,
    };
    if screen.quit_confirmation_open() {
        return None;
    }
    let app_frame = app_frame(area)?;
    let body = progress_body_areas(screen.trace_commands, app_frame.body);
    if let Some(log_area) = body.log
        && rect_contains(log_area, mouse.column, mouse.row)
    {
        return Some((ProgressScrollTarget::CommandLog, -table_delta));
    }
    rect_contains(body.main, mouse.column, mouse.row)
        .then_some((ProgressScrollTarget::Table, table_delta))
}

fn flush_progress_scrolls(
    screen: &mut InteractiveProgressScreen,
    area: Rect,
    scrolls: &mut ProgressScrollDeltas,
) {
    if let Some(app_frame) = app_frame(area) {
        let body = progress_body_areas(screen.trace_commands, app_frame.body);
        if scrolls.table != 0 {
            screen.scroll_table_by(scrolls.table, progress_table_visible_height(body.main));
        }
        if scrolls.command_log != 0
            && let Some(log_area) = body.log
        {
            screen.scroll_command_log_by(scrolls.command_log, usize::from(log_area.height));
        }
    }
    scrolls.table = 0;
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

fn handle_progress_event(
    event: &Event,
    screen: &mut InteractiveProgressScreen,
    stop_requested: &AtomicBool,
    area: Rect,
) {
    if let Event::Mouse(mouse) = event {
        handle_progress_mouse(screen, *mouse, stop_requested, area);
        return;
    }

    if screen.result_open()
        && let Some(delta) = result_key_scroll_delta(event)
    {
        let Some(app_frame) = app_frame(area) else {
            return;
        };
        let body = progress_body_areas(screen.trace_commands, app_frame.body);
        screen.scroll_table_by(delta, progress_table_visible_height(body.main));
        return;
    }

    let input = progress_input_from_event_for_phase(event, screen.phase);
    screen.handle_input(input, stop_requested);
}

fn handle_progress_mouse(
    screen: &mut InteractiveProgressScreen,
    mouse: MouseEvent,
    stop_requested: &AtomicBool,
    area: Rect,
) {
    let Some(app_frame) = app_frame(area) else {
        return;
    };

    if screen.quit_confirmation_open() {
        handle_quit_dialog_mouse(screen, mouse, stop_requested, app_frame.inner);
        return;
    }

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if rect_contains(app_frame.footer, mouse.column, mouse.row)
                && key_footer_hit(FOOTER_KEYS, mouse.column - app_frame.footer.x) == Some(0)
            {
                screen.handle_input(ProgressInput::Quit, stop_requested);
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
}

fn handle_quit_dialog_mouse(
    screen: &mut InteractiveProgressScreen,
    mouse: MouseEvent,
    stop_requested: &AtomicBool,
    area: Rect,
) {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return;
    }
    let Some(inner) = modal_inner_rect(area, QUIT_DIALOG_WIDTH, QUIT_DIALOG_HEIGHT) else {
        return;
    };
    if !rect_contains(inner, mouse.column, mouse.row) {
        screen.handle_input(ProgressInput::CancelQuit, stop_requested);
        return;
    }
    let [_, _, _, _, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    if !rect_contains(footer_area, mouse.column, mouse.row) {
        return;
    }
    match key_footer_hit(QUIT_CONFIRM_KEYS, mouse.column - footer_area.x) {
        Some(0) => screen.handle_input(ProgressInput::ConfirmQuitAfterCurrent, stop_requested),
        Some(1) => screen.handle_input(ProgressInput::CancelQuit, stop_requested),
        _ => {}
    }
}

struct ProgressBodyAreas {
    main: Rect,
    log: Option<Rect>,
}

fn progress_body_areas(trace_commands: bool, area: Rect) -> ProgressBodyAreas {
    command_log_layout(trace_commands, area).map_or(
        ProgressBodyAreas {
            main: area,
            log: None,
        },
        |layout| ProgressBodyAreas {
            main: layout.main,
            log: Some(layout.log),
        },
    )
}

fn progress_table_visible_height(area: Rect) -> usize {
    usize::from(area.height.saturating_sub(1))
}

const fn progress_table_max_offset(row_count: usize, visible_height: usize) -> usize {
    row_count.saturating_sub(visible_height)
}

fn modal_inner_rect(area: Rect, width: u16, height: u16) -> Option<Rect> {
    if area.is_empty() || width == 0 || height == 0 {
        return None;
    }
    let width = width.min(area.width);
    let height = height.min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    Some(Rect {
        x: popup.x.saturating_add(1),
        y: popup.y.saturating_add(1),
        width: popup.width.saturating_sub(2),
        height: popup.height.saturating_sub(2),
    })
}

const fn rect_contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

fn progress_input_from_event_for_phase(event: &Event, phase: ProgressPhase) -> ProgressInput {
    let Event::Key(key) = event else {
        return ProgressInput::Ignore;
    };
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return ProgressInput::Ignore;
    }

    if matches!(phase, ProgressPhase::Result) {
        return match key.code {
            KeyCode::Char('q' | 'Q') => ProgressInput::Quit,
            _ => ProgressInput::Ignore,
        };
    }

    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return ProgressInput::Interrupt;
    }

    if matches!(phase, ProgressPhase::QuitConfirm) {
        return match key.code {
            KeyCode::Enter | KeyCode::Char('y' | 'Y') => ProgressInput::ConfirmQuitAfterCurrent,
            KeyCode::Esc | KeyCode::Char('n' | 'N') => ProgressInput::CancelQuit,
            _ => ProgressInput::Ignore,
        };
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => ProgressInput::Quit,
        _ => ProgressInput::Ignore,
    }
}

fn result_key_scroll_delta(event: &Event) -> Option<isize> {
    let Event::Key(key) = event else {
        return None;
    };
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return None;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k' | 'K') => Some(-1),
        KeyCode::Down | KeyCode::Char('j' | 'J') => Some(1),
        _ => None,
    }
}

fn draw_progress(frame: &mut ratatui::Frame<'_>, screen: &mut InteractiveProgressScreen) {
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
    frame.render_widget(
        Paragraph::new(header_line(screen, &theme)),
        app_frame.header,
    );
    render_separator(frame, app_frame.header_separator, &theme);
    draw_progress_body(frame, screen, app_frame.body, &theme);
    render_separator(frame, app_frame.footer_separator, &theme);
    frame.render_widget(
        Paragraph::new(key_footer(FOOTER_KEYS, &theme)),
        app_frame.footer,
    );

    if screen.quit_confirmation_open() {
        draw_quit_dialog(frame, app_frame.inner, &theme);
    }
}

fn header_line(screen: &InteractiveProgressScreen, theme: &TuiTheme) -> Line<'static> {
    let running_manager = screen
        .state
        .rows
        .iter()
        .find(|row| matches!(row.status, ExecutionProgressStatus::Running));
    let title = running_manager.map_or_else(
        || {
            if screen.finished() {
                format!(
                    "Apply complete: {}",
                    progress_summary_label(screen.state.summary())
                )
            } else {
                "Applying updates".to_owned()
            }
        },
        |row| format!("Applying updates: {}", row.manager_id),
    );
    Line::from(Span::styled(title, theme.title))
}

fn draw_progress_body(
    frame: &mut ratatui::Frame<'_>,
    screen: &mut InteractiveProgressScreen,
    area: Rect,
    theme: &TuiTheme,
) {
    let Some(layout) = command_log_layout(screen.trace_commands, area) else {
        draw_progress_table(frame, screen, area, theme);
        return;
    };

    draw_progress_table(frame, screen, layout.main, theme);
    screen.clamp_command_log_scroll(usize::from(layout.log.height));
    render_command_log(
        frame,
        layout.separator,
        layout.log,
        &screen.state.command_log,
        screen.command_log_scroll_from_bottom,
        theme,
    );
}

fn draw_progress_table(
    frame: &mut ratatui::Frame<'_>,
    screen: &mut InteractiveProgressScreen,
    area: Rect,
    theme: &TuiTheme,
) {
    let mut rows = screen
        .state
        .rows
        .iter()
        .map(|row| {
            if screen.result_open() {
                result_table_row(row, theme)
            } else {
                progress_table_row(row, screen.spinner_tick, theme)
            }
        })
        .collect::<Vec<_>>();
    rows.extend(screen.state.manager_failures.iter().map(|failure| {
        if screen.result_open() {
            return Row::new(vec![
                Cell::new("").style(theme.error),
                Cell::new(failure.manager_id.to_string()).style(theme.error),
                Cell::new("manager").style(theme.error),
                Cell::new(""),
                Cell::new(""),
                Cell::new(""),
            ])
            .style(theme.error);
        }
        Row::new(vec![
            Cell::new("[x]").style(theme.error),
            Cell::new(failure.manager_id.to_string()).style(theme.error),
            Cell::new("manager").style(theme.error),
            Cell::new(""),
            Cell::new(""),
            Cell::new(failure.detail.clone()).style(theme.error),
        ])
        .style(theme.error)
    }));

    if rows.is_empty() {
        rows.push(Row::new(vec![
            Cell::new(""),
            Cell::new(""),
            Cell::new("no selected updates"),
            Cell::new(""),
            Cell::new(""),
            Cell::new(""),
        ]));
    }

    screen.clamp_table_offset(progress_table_visible_height(area));
    render_table(
        frame,
        area,
        TuiTable::new(rows, progress_update_columns(area.width))
            .header(update_header_row(theme))
            .offset(screen.table_offset),
        theme,
    );
}

fn progress_table_row(
    row: &ExecutionProgressRow,
    spinner_tick: usize,
    theme: &TuiTheme,
) -> Row<'static> {
    let style = progress_row_style(&row.status, theme);
    Row::new(vec![
        Cell::new(status_label(&row.status, spinner_tick)).style(theme.emphasis(style)),
        Cell::new(row.manager_id.to_string()).style(style),
        Cell::new(row.package_name.to_string()).style(theme.emphasis(style)),
        Cell::new(row.installed_version.to_string()).style(style),
        Cell::new(target_label(&row.target)).style(style),
        Cell::new(status_note(&row.status)).style(style),
    ])
    .style(style)
}

fn result_table_row(row: &ExecutionProgressRow, theme: &TuiTheme) -> Row<'static> {
    let style = progress_row_style(&row.status, theme);
    Row::new(vec![
        Cell::new("").style(style),
        Cell::new(row.manager_id.to_string()).style(style),
        Cell::new(row.package_name.to_string()).style(theme.emphasis(style)),
        Cell::new(result_current_label(row)).style(style),
        Cell::new("").style(style),
        Cell::new("").style(style),
    ])
    .style(style)
}

fn result_current_label(row: &ExecutionProgressRow) -> String {
    match row.status {
        ExecutionProgressStatus::Succeeded { .. } => target_label(&row.target),
        ExecutionProgressStatus::Pending
        | ExecutionProgressStatus::Running
        | ExecutionProgressStatus::Failed { .. }
        | ExecutionProgressStatus::Skipped { .. } => row.installed_version.to_string(),
    }
}

fn target_label(target: &ResolvedExecutionTarget) -> String {
    match target {
        ResolvedExecutionTarget::Known(version) => version_label(version.as_str()),
        ResolvedExecutionTarget::ManagerResolved => manager_resolved_label().to_owned(),
    }
}

const fn progress_row_style(status: &ExecutionProgressStatus, theme: &TuiTheme) -> Style {
    match status {
        ExecutionProgressStatus::Pending => theme.pending,
        ExecutionProgressStatus::Running => theme.running,
        ExecutionProgressStatus::Succeeded { .. } => theme.success,
        ExecutionProgressStatus::Failed { .. } => theme.error,
        ExecutionProgressStatus::Skipped { .. } => theme.muted,
    }
}

pub(super) fn spinner_frame(spinner_tick: usize) -> &'static str {
    SPINNER[spinner_tick % SPINNER.len()]
}

fn status_label(status: &ExecutionProgressStatus, spinner_tick: usize) -> &'static str {
    match status {
        ExecutionProgressStatus::Pending => "[ ]",
        ExecutionProgressStatus::Running => spinner_frame(spinner_tick),
        ExecutionProgressStatus::Succeeded { .. } => "[✔]",
        ExecutionProgressStatus::Failed { .. } => "[⨯]",
        ExecutionProgressStatus::Skipped { .. } => "[-]",
    }
}

const fn progress_summary_label(summary: ExecutionProgressSummary) -> &'static str {
    match (summary.had_failure, summary.stopped_after_current) {
        (false, false) => "ok",
        (true, false) => "failed",
        (false, true) => "stopped",
        (true, true) => "failed stopped",
    }
}

fn status_note(status: &ExecutionProgressStatus) -> String {
    match status {
        ExecutionProgressStatus::Pending => String::new(),
        ExecutionProgressStatus::Running => "running".to_owned(),
        ExecutionProgressStatus::Succeeded {
            skipped_mutation, ..
        } => {
            if *skipped_mutation {
                "skipped".to_owned()
            } else {
                String::new()
            }
        }
        ExecutionProgressStatus::Failed { detail }
        | ExecutionProgressStatus::Skipped { detail } => detail.clone(),
    }
}

fn draw_quit_dialog(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, theme: &TuiTheme) {
    let Some(inner) = render_modal_frame(
        frame,
        area,
        QUIT_DIALOG_WIDTH,
        QUIT_DIALOG_HEIGHT,
        None,
        theme,
    ) else {
        return;
    };
    let lines = vec![
        Line::from(Span::styled("Apply is in progress", theme.title)).centered(),
        Line::raw(""),
        Line::raw("Quit after the current manager command finishes?").centered(),
        Line::raw(""),
        key_footer(QUIT_CONFIRM_KEYS, theme),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}
