use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row};

use super::components::footer::{KeyBinding, key_footer};
use super::components::frame::{app_block, render_separator};
use super::components::modal::render_modal_frame;
use super::components::table::{
    TuiTable, progress_update_columns, render_table, update_header_row,
};
use super::layout::app_frame;
use super::terminal::{FullscreenControl, FullscreenScreen, run_fullscreen_screen};
use super::theme::TuiTheme;
use crate::managers::PlannedUpdate;
use crate::outcome::{
    ItemOutcome, OutcomeStatus, OutcomeVersions, drain_text_outcomes, version_label,
};

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

pub type ApplyProgressFn = Box<dyn FnOnce() -> Result<()> + Send + 'static>;

pub struct ApplyProgressTask {
    pub manager_id: &'static str,
    pub selected: Vec<PlannedUpdate>,
    pub apply: ApplyProgressFn,
}

impl ApplyProgressTask {
    pub fn new(
        manager_id: &'static str,
        selected: Vec<PlannedUpdate>,
        apply: ApplyProgressFn,
    ) -> Self {
        Self {
            manager_id,
            selected,
            apply,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyProgressSummary {
    pub had_failure: bool,
    pub interrupted: bool,
}

#[derive(Debug)]
struct ApplyProgressApp {
    rows: Vec<ApplyRow>,
    current_manager: Option<&'static str>,
    phase: ApplyProgressPhase,
    had_failure: bool,
    interrupted: bool,
    stop_after_current: bool,
    spinner_tick: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyProgressPhase {
    Running,
    QuitConfirm,
    Done,
}

#[derive(Debug, Clone)]
struct ApplyRow {
    manager: &'static str,
    name: String,
    current: String,
    target: String,
    status: ApplyRowStatus,
    note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ApplyRowStatus {
    Pending,
    Running,
    Done,
    Failed,
}

enum ApplyEvent {
    ManagerStarted(&'static str),
    ManagerFinished {
        manager: &'static str,
        result: Result<()>,
        outcomes: Vec<ItemOutcome>,
    },
    Finished,
}

impl ApplyProgressApp {
    fn new(tasks: &[ApplyProgressTask]) -> Self {
        let rows = tasks
            .iter()
            .flat_map(|task| {
                task.selected.iter().map(|item| ApplyRow {
                    manager: task.manager_id,
                    name: item.name.clone(),
                    current: version_label(&item.current),
                    target: version_label(&item.target),
                    status: ApplyRowStatus::Pending,
                    note: String::new(),
                })
            })
            .collect();

        Self {
            rows,
            current_manager: None,
            phase: ApplyProgressPhase::Running,
            had_failure: false,
            interrupted: false,
            stop_after_current: false,
            spinner_tick: 0,
        }
    }

    fn tick(&mut self) {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
    }

    fn apply_event(&mut self, event: ApplyEvent) {
        match event {
            ApplyEvent::ManagerStarted(manager) => {
                self.current_manager = Some(manager);
                for row in self.rows.iter_mut().filter(|row| row.manager == manager) {
                    row.status = ApplyRowStatus::Running;
                    row.note.clear();
                }
            }
            ApplyEvent::ManagerFinished {
                manager,
                result,
                outcomes,
            } => {
                let manager_error = result.err().map(|err| format!("{err:#}"));
                let outcome_notes = outcome_notes_by_item(&outcomes);
                let manager_level_error = manager_level_error(&outcomes).or(manager_error);

                let mut failed_any = false;
                for row in self.rows.iter_mut().filter(|row| row.manager == manager) {
                    if let Some(note) = outcome_notes.get(&(row.name.clone(), row.target.clone())) {
                        row.status = ApplyRowStatus::Failed;
                        row.note.clone_from(note);
                        failed_any = true;
                    } else if let Some(note) = &manager_level_error {
                        row.status = ApplyRowStatus::Failed;
                        row.note.clone_from(note);
                        failed_any = true;
                    } else if row.status == ApplyRowStatus::Running {
                        row.status = ApplyRowStatus::Done;
                    }
                }

                self.had_failure |= failed_any;
                self.current_manager = None;
                if self.stop_after_current {
                    self.interrupted = true;
                }
            }
            ApplyEvent::Finished => {
                self.phase = ApplyProgressPhase::Done;
                self.current_manager = None;
            }
        }
    }
}

pub fn run_apply_progress(tasks: Vec<ApplyProgressTask>) -> Result<ApplyProgressSummary> {
    let mut app = ApplyProgressApp::new(&tasks);
    let stop_requested = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let worker_stop = Arc::clone(&stop_requested);
    let worker = thread::spawn(move || run_apply_worker(tasks, &worker_stop, &tx));

    let tui_result = {
        let mut screen = ApplyProgressScreen {
            app: &mut app,
            rx: &rx,
            stop_requested: &stop_requested,
        };
        run_fullscreen_screen(&mut screen, TICK_RATE)
    };

    let worker_result = worker
        .join()
        .map_err(|_| anyhow::anyhow!("apply worker thread panicked"))?;
    worker_result?;
    tui_result?;

    Ok(ApplyProgressSummary {
        had_failure: app.had_failure,
        interrupted: app.interrupted,
    })
}

struct ApplyProgressScreen<'a> {
    app: &'a mut ApplyProgressApp,
    rx: &'a Receiver<ApplyEvent>,
    stop_requested: &'a AtomicBool,
}

impl FullscreenScreen for ApplyProgressScreen<'_> {
    fn before_draw(&mut self) -> Result<()> {
        drain_apply_events(self.rx, self.app);
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        draw_apply_progress(frame, self.app);
    }

    fn should_exit(&mut self) -> bool {
        self.app.phase == ApplyProgressPhase::Done
    }

    fn handle_event(&mut self, event: Event) -> Result<FullscreenControl> {
        match handle_event(&event, self.app, self.stop_requested) {
            ApplyControl::Continue => Ok(FullscreenControl::Continue),
            ApplyControl::Cancel => {
                self.stop_requested.store(true, Ordering::Relaxed);
                self.app.stop_after_current = true;
                self.app.interrupted = true;
                self.app.phase = ApplyProgressPhase::Running;
                Ok(FullscreenControl::Continue)
            }
        }
    }

    fn tick(&mut self) {
        self.app.tick();
    }
}

fn run_apply_worker(
    tasks: Vec<ApplyProgressTask>,
    stop_requested: &AtomicBool,
    tx: &Sender<ApplyEvent>,
) -> Result<()> {
    for task in tasks {
        if stop_requested.load(Ordering::Relaxed) {
            break;
        }

        let manager = task.manager_id;
        tx.send(ApplyEvent::ManagerStarted(manager))?;
        let result = (task.apply)();
        let outcomes = drain_text_outcomes();
        tx.send(ApplyEvent::ManagerFinished {
            manager,
            result,
            outcomes,
        })?;

        if stop_requested.load(Ordering::Relaxed) {
            break;
        }
    }

    tx.send(ApplyEvent::Finished)?;
    Ok(())
}

fn drain_apply_events(rx: &Receiver<ApplyEvent>, app: &mut ApplyProgressApp) {
    while let Ok(event) = rx.try_recv() {
        app.apply_event(event);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyControl {
    Continue,
    Cancel,
}

fn handle_event(
    event: &Event,
    app: &mut ApplyProgressApp,
    stop_requested: &AtomicBool,
) -> ApplyControl {
    let Event::Key(key) = event else {
        return ApplyControl::Continue;
    };

    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return ApplyControl::Continue;
    }

    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return ApplyControl::Cancel;
    }

    if app.phase == ApplyProgressPhase::QuitConfirm {
        return match key.code {
            KeyCode::Enter | KeyCode::Char('y' | 'Y') => ApplyControl::Cancel,
            KeyCode::Esc | KeyCode::Char('n' | 'N') => {
                app.phase = ApplyProgressPhase::Running;
                ApplyControl::Continue
            }
            _ => ApplyControl::Continue,
        };
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            if app.current_manager.is_some() && !stop_requested.load(Ordering::Relaxed) {
                app.phase = ApplyProgressPhase::QuitConfirm;
                ApplyControl::Continue
            } else {
                ApplyControl::Cancel
            }
        }
        _ => ApplyControl::Continue,
    }
}

fn draw_apply_progress(frame: &mut Frame<'_>, app: &ApplyProgressApp) {
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

    frame.render_widget(Paragraph::new(header_line(app, &theme)), app_frame.header);
    render_separator(frame, app_frame.header_separator, &theme);
    draw_progress_table(frame, app, app_frame.body, &theme);
    render_separator(frame, app_frame.footer_separator, &theme);
    frame.render_widget(Paragraph::new(footer_line(&theme)), app_frame.footer);

    if app.phase == ApplyProgressPhase::QuitConfirm {
        draw_quit_dialog(frame, app_frame.inner, &theme);
    }
}

fn header_line(app: &ApplyProgressApp, theme: &TuiTheme) -> Line<'static> {
    let title = app.current_manager.map_or_else(
        || {
            if app.phase == ApplyProgressPhase::Done {
                "Apply complete".to_string()
            } else {
                "Applying updates".to_string()
            }
        },
        |manager| format!("Applying updates: {manager}"),
    );

    Line::from(Span::styled(title, theme.title))
}

fn draw_progress_table(
    frame: &mut Frame<'_>,
    app: &ApplyProgressApp,
    area: Rect,
    theme: &TuiTheme,
) {
    let rows = app
        .rows
        .iter()
        .map(|row| progress_table_row(row, app.spinner_tick, theme))
        .collect::<Vec<_>>();
    render_table(
        frame,
        area,
        TuiTable::new(rows, progress_update_columns()).header(update_header_row(theme)),
        theme,
    );
}

fn progress_table_row(row: &ApplyRow, spinner_tick: usize, theme: &TuiTheme) -> Row<'static> {
    let style = apply_row_style(row.status, theme);
    Row::new(vec![
        Cell::new(status_label(row.status, spinner_tick)).style(theme.emphasis(style)),
        Cell::new(row.manager).style(style),
        Cell::new(row.name.clone()).style(theme.emphasis(style)),
        Cell::new(row.current.clone()).style(style),
        Cell::new(row.target.clone()).style(style),
        Cell::new(row.note.clone()).style(style),
    ])
    .style(style)
}

fn apply_row_style(status: ApplyRowStatus, theme: &TuiTheme) -> Style {
    match status {
        ApplyRowStatus::Pending => theme.pending,
        ApplyRowStatus::Running => theme.running,
        ApplyRowStatus::Done => theme.success,
        ApplyRowStatus::Failed => theme.error,
    }
}

fn status_label(status: ApplyRowStatus, spinner_tick: usize) -> &'static str {
    match status {
        ApplyRowStatus::Pending => "[ ]",
        ApplyRowStatus::Running => SPINNER[spinner_tick % SPINNER.len()],
        ApplyRowStatus::Done => "[✔]",
        ApplyRowStatus::Failed => "[⨯]",
    }
}

fn footer_line(theme: &TuiTheme) -> Line<'static> {
    key_footer(FOOTER_KEYS, theme)
}

fn draw_quit_dialog(frame: &mut Frame<'_>, area: Rect, theme: &TuiTheme) {
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

fn outcome_notes_by_item(outcomes: &[ItemOutcome]) -> HashMap<(String, String), String> {
    outcomes
        .iter()
        .filter(|outcome| outcome.status == OutcomeStatus::Error)
        .filter_map(|outcome| {
            let OutcomeVersions::Change { to, .. } = &outcome.versions else {
                return None;
            };
            Some((
                (outcome.name.clone(), version_label(to)),
                outcome
                    .diagnostics
                    .detail
                    .clone()
                    .unwrap_or_else(|| "failed".to_string()),
            ))
        })
        .collect()
}

fn manager_level_error(outcomes: &[ItemOutcome]) -> Option<String> {
    outcomes
        .iter()
        .find(|outcome| {
            outcome.status == OutcomeStatus::Error
                && matches!(outcome.versions, OutcomeVersions::None)
        })
        .map(|outcome| {
            outcome
                .diagnostics
                .detail
                .clone()
                .unwrap_or_else(|| "manager command failed".to_string())
        })
}
