use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Result, anyhow};
use crossterm::event::Event;
use ratatui::Frame;

use self::events::{SelectionControl, handle_event};
use self::model::SelectionApp;
use self::view::draw_selection;
use super::terminal::{FullscreenControl, FullscreenScreen, run_fullscreen_screen};
use crate::interactive::{InteractiveCancelled, apply::InteractiveApplyPlan};
use crate::managers::ManagerCtx;
use crate::util::process::CommandFailedError;

mod events;
mod model;
mod view;

const TICK_RATE: Duration = Duration::from_millis(180);

#[derive(Debug, Clone)]
pub struct SelectionResult {
    pub manager_id: &'static str,
    pub chosen_versions: Vec<Option<usize>>,
}

pub type SelectionPlanningFn = Box<dyn FnOnce() -> Result<Option<SelectionPlan>> + Send + 'static>;

pub struct SelectionPlanningTask {
    pub manager_id: &'static str,
    pub plan: SelectionPlanningFn,
}

impl SelectionPlanningTask {
    pub fn new(manager_id: &'static str, plan: SelectionPlanningFn) -> Self {
        Self { manager_id, plan }
    }
}

pub struct SelectionPlan {
    pub ctx: ManagerCtx,
    pub plan: InteractiveApplyPlan,
}

pub struct LazySelectionOutput {
    pub planned: Vec<SelectionPlan>,
    pub results: Vec<SelectionResult>,
    pub had_manager_failure: bool,
    pub interrupted: bool,
}

pub fn run_lazy_selection(tasks: Vec<SelectionPlanningTask>) -> Result<LazySelectionOutput> {
    let mut screen = SelectionScreen::new(tasks);
    let tui_result = run_fullscreen_screen(&mut screen, TICK_RATE);
    screen.join_worker()?;
    tui_result?;
    Ok(screen.into_output())
}

struct SelectionScreen {
    app: SelectionApp,
    tasks: Option<Vec<SelectionPlanningTask>>,
    planned: Vec<SelectionPlan>,
    stop_requested: Arc<AtomicBool>,
    rx: Receiver<PlanningEvent>,
    tx: Option<Sender<PlanningEvent>>,
    worker: Option<JoinHandle<Result<()>>>,
}

impl SelectionScreen {
    fn new(tasks: Vec<SelectionPlanningTask>) -> Self {
        let app = SelectionApp::new_lazy(&tasks);
        let planned = Vec::new();
        let stop_requested = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();

        Self {
            app,
            tasks: Some(tasks),
            planned,
            stop_requested,
            rx,
            tx: Some(tx),
            worker: None,
        }
    }

    fn start_worker(&mut self) {
        let Some(tasks) = self.tasks.take() else {
            return;
        };
        let Some(tx) = self.tx.take() else {
            return;
        };

        let worker_stop = Arc::clone(&self.stop_requested);
        self.worker = Some(thread::spawn(move || {
            run_planning_worker(tasks, &worker_stop, &tx)
        }));
    }

    fn join_worker(&mut self) -> Result<()> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };

        worker
            .join()
            .map_err(|_| anyhow::anyhow!("interactive planning worker thread panicked"))?
    }

    fn into_output(self) -> LazySelectionOutput {
        LazySelectionOutput {
            results: if self.app.interrupted {
                Vec::new()
            } else {
                self.app.results()
            },
            planned: self.planned,
            had_manager_failure: self.app.had_manager_failure,
            interrupted: self.app.interrupted,
        }
    }
}

impl FullscreenScreen for SelectionScreen {
    fn before_draw(&mut self) -> Result<()> {
        drain_planning_events(&self.rx, &mut self.app, &mut self.planned);
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        draw_selection(frame, &mut self.app, &self.planned);
    }

    fn after_initial_draw(&mut self) -> Result<()> {
        self.start_worker();
        Ok(())
    }

    fn should_exit(&mut self) -> bool {
        self.app.planning_done && self.app.interrupted
    }

    fn handle_event(&mut self, event: Event) -> Result<FullscreenControl> {
        let control = match handle_event(&event, &mut self.app, &self.planned) {
            SelectionControl::Confirm if self.app.planning_done => FullscreenControl::Exit,
            SelectionControl::Continue | SelectionControl::Confirm => FullscreenControl::Continue,
            SelectionControl::Cancel => {
                self.stop_requested.store(true, Ordering::Relaxed);
                self.app.cancel_requested = true;
                self.app.interrupted = true;
                if self.app.planning_done {
                    FullscreenControl::Exit
                } else {
                    FullscreenControl::Continue
                }
            }
        };

        Ok(control)
    }

    fn tick(&mut self) {
        self.app.tick();
    }
}

enum PlanningEvent {
    ManagerStarted(&'static str),
    ManagerFinished {
        manager_id: &'static str,
        result: Box<Result<Option<SelectionPlan>>>,
    },
    Finished,
}

fn run_planning_worker(
    tasks: Vec<SelectionPlanningTask>,
    stop_requested: &AtomicBool,
    tx: &Sender<PlanningEvent>,
) -> Result<()> {
    for task in tasks {
        if stop_requested.load(Ordering::Relaxed) {
            break;
        }

        let manager_id = task.manager_id;
        tx.send(PlanningEvent::ManagerStarted(manager_id))
            .map_err(|_| anyhow!("planning event receiver was dropped"))?;
        let result = (task.plan)();
        tx.send(PlanningEvent::ManagerFinished {
            manager_id,
            result: Box::new(result),
        })
        .map_err(|_| anyhow!("planning event receiver was dropped"))?;

        if stop_requested.load(Ordering::Relaxed) {
            break;
        }
    }

    tx.send(PlanningEvent::Finished)
        .map_err(|_| anyhow!("planning event receiver was dropped"))?;
    Ok(())
}

fn drain_planning_events(
    rx: &Receiver<PlanningEvent>,
    app: &mut SelectionApp,
    planned: &mut Vec<SelectionPlan>,
) {
    while let Ok(event) = rx.try_recv() {
        match event {
            PlanningEvent::ManagerStarted(manager_id) => {
                app.set_manager_loading(manager_id, "Planning updates...");
            }
            PlanningEvent::ManagerFinished { manager_id, result } => match *result {
                Ok(Some(planned_manager)) => {
                    let plan_idx = planned.len();
                    let pinned = planned_manager.ctx.policy.pinned.clone();
                    app.finish_manager_plan(
                        manager_id,
                        plan_idx,
                        &planned_manager.plan.candidates,
                        &pinned,
                    );
                    planned.push(planned_manager);
                }
                Ok(None) => {
                    app.finish_empty_manager_plan(manager_id);
                }
                Err(err) => {
                    app.had_manager_failure = true;
                    if is_interrupted_error(&err) {
                        app.interrupted = true;
                        app.set_manager_error(manager_id, "Planning cancelled");
                    } else {
                        app.set_manager_error(manager_id, format!("{err:#}"));
                    }
                }
            },
            PlanningEvent::Finished => {
                app.planning_done = true;
                if app.cancel_requested {
                    app.mark_loading_cancelled();
                }
            }
        }
    }
}

fn is_interrupted_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<CommandFailedError>()
            .is_some_and(CommandFailedError::was_signaled)
            || cause.downcast_ref::<InteractiveCancelled>().is_some()
    })
}
