use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row};
use upnow_execution::progress::{
    ExecutionProgressEvent, ExecutionProgressRow, ExecutionProgressState, ExecutionProgressStatus,
    ExecutionProgressSummary,
};

use crate::tui::components::{
    KeyBinding, TuiTable, app_block, key_footer, progress_update_columns, render_modal_frame,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressInput {
    Quit,
    ConfirmQuitAfterCurrent,
    CancelQuit,
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveProgressScreen {
    state: ExecutionProgressState,
    phase: ProgressPhase,
    spinner_tick: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressPhase {
    Running,
    QuitConfirm,
    Done,
}

impl InteractiveProgressScreen {
    pub const fn new(state: ExecutionProgressState) -> Self {
        Self {
            state,
            phase: ProgressPhase::Running,
            spinner_tick: 0,
        }
    }
    pub const fn state(&self) -> &ExecutionProgressState {
        &self.state
    }
    pub const fn quit_confirmation_open(&self) -> bool {
        matches!(self.phase, ProgressPhase::QuitConfirm)
    }
    pub const fn finished(&self) -> bool {
        matches!(self.phase, ProgressPhase::Done)
    }

    pub const fn tick(&mut self) {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
    }

    pub fn apply_event(&mut self, event: ExecutionProgressEvent) {
        let finished = matches!(event, ExecutionProgressEvent::Finished);
        self.state.apply_event(event);
        if finished {
            self.phase = ProgressPhase::Done;
        }
    }

    pub fn handle_input(&mut self, input: ProgressInput, stop_requested: &AtomicBool) {
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
            _ => {}
        }
    }

    fn has_running_rows(&self) -> bool {
        self.state
            .rows
            .iter()
            .any(|row| matches!(row.status, ExecutionProgressStatus::Running))
    }
}

/// Runs the fullscreen interactive progress UI over a typed progress event stream.
///
/// # Errors
///
/// Returns an I/O error for terminal setup, rendering, event reading, or cleanup failures.
#[expect(clippy::needless_pass_by_value)]
pub fn run_interactive_progress(
    state: ExecutionProgressState,
    rx: &Receiver<ExecutionProgressEvent>,
    stop_requested: Arc<AtomicBool>,
) -> io::Result<ExecutionProgressSummary> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    if let Err(err) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(err);
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(err) => {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(err);
        }
    };
    let mut screen = InteractiveProgressScreen::new(state);

    let result = run_progress_loop(&mut terminal, &mut screen, rx, &stop_requested);

    let cleanup = cleanup_terminal(&mut terminal);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(screen.state.summary()),
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
        if screen.finished() {
            break;
        }

        if event::poll(TICK_RATE)? {
            let input = progress_input_from_event(&event::read()?, screen.quit_confirmation_open());
            screen.handle_input(input, stop_requested);
        }

        screen.tick();
    }
    Ok(())
}

fn cleanup_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let raw_mode = disable_raw_mode();
    let screen = execute!(terminal.backend_mut(), LeaveAlternateScreen);
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
pub fn progress_input_from_event(event: &Event, quit_confirmation_open: bool) -> ProgressInput {
    let Event::Key(key) = event else {
        return ProgressInput::Ignore;
    };
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return ProgressInput::Ignore;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return ProgressInput::Quit;
    }

    if quit_confirmation_open {
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

fn draw_progress(frame: &mut ratatui::Frame<'_>, screen: &InteractiveProgressScreen) {
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
    draw_progress_table(frame, screen, app_frame.body, &theme);
    render_separator(frame, app_frame.footer_separator, &theme);
    frame.render_widget(Paragraph::new(footer_line(&theme)), app_frame.footer);

    if screen.quit_confirmation_open() {
        draw_quit_dialog(frame, app_frame.inner, &theme);
    }
}

fn header_line(screen: &InteractiveProgressScreen, theme: &TuiTheme) -> Line<'static> {
    let running_manager = screen
        .state
        .rows
        .iter()
        .find(|row| matches!(row.status, ExecutionProgressStatus::Running))
        .map(|row| row.manager_id.as_str().to_owned());
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
        |manager| format!("Applying updates: {manager}"),
    );
    Line::from(Span::styled(title, theme.title))
}

fn draw_progress_table(
    frame: &mut ratatui::Frame<'_>,
    screen: &InteractiveProgressScreen,
    area: ratatui::layout::Rect,
    theme: &TuiTheme,
) {
    let mut rows = screen
        .state
        .rows
        .iter()
        .map(|row| progress_table_row(row, screen.spinner_tick, theme))
        .collect::<Vec<_>>();
    rows.extend(screen.state.manager_failures.iter().map(|failure| {
        Row::new(vec![
            Cell::new("[x]").style(theme.error),
            Cell::new(failure.manager_id.as_str().to_owned()).style(theme.error),
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

    render_table(
        frame,
        area,
        TuiTable::new(rows, progress_update_columns()).header(update_header_row(theme)),
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
        Cell::new(row.manager_id.as_str().to_owned()).style(style),
        Cell::new(row.package_name.as_str().to_owned()).style(theme.emphasis(style)),
        Cell::new(row.installed_version.as_str().to_owned()).style(style),
        Cell::new(row.target_version.as_str().to_owned()).style(style),
        Cell::new(status_note(&row.status)).style(style),
    ])
    .style(style)
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

pub(crate) fn spinner_frame(spinner_tick: usize) -> &'static str {
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

fn footer_line(theme: &TuiTheme) -> Line<'static> {
    key_footer(FOOTER_KEYS, theme)
}

fn draw_quit_dialog(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, theme: &TuiTheme) {
    let Some(inner) = render_modal_frame(frame, area, 54, 7, None, theme) else {
        return;
    };
    let lines = vec![
        Line::from(Span::styled("Apply is in progress", theme.title)).centered(),
        Line::raw(""),
        Line::raw("Quit after the current manager command finishes?").centered(),
        Line::raw(""),
        key_footer(QUIT_CONFIRM_KEYS, theme).centered(),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}
