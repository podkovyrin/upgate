mod input;
mod render;
mod screen;

use std::io;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use upgate_domain::{ManagerId, SelectedItem, UpdateSelectionPolicy, VersionPolicy};

use self::input::handle_selection_ready_events;
use self::render::draw_selection;
use self::screen::InteractiveSelectionScreen;
use crate::SelectionView;

const MAX_DRAINED_INPUT_EVENTS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveManagerSelectionDraft {
    pub manager_id: ManagerId,
    pub selected_items: Vec<SelectedItem>,
    pub selection_policy: UpdateSelectionPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractiveSelectionOutcome {
    Confirmed(Vec<InteractiveManagerSelectionDraft>),
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionInput {
    Up,
    Down,
    NextTab,
    PreviousTab,
    ToggleCurrent,
    SelectVisible,
    SelectNoneVisible,
    ToggleViewAll,
    OpenTargetPicker,
    PickerUp,
    PickerDown,
    PickerPreviousRow,
    PickerNextRow,
    PickerConfirm,
    PickerCancel,
    RecommendedTarget,
    Confirm,
    Cancel,
    Interrupt,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionControl {
    Continue,
    Confirm,
    Cancel,
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractiveSelectionPlanningEvent {
    ManagerStarted {
        manager_id: ManagerId,
    },
    CommandStarted {
        command: String,
    },
    ManagerReady {
        view: SelectionView,
        selection_policy: UpdateSelectionPolicy,
        version_policy: VersionPolicy,
    },
    ManagerError {
        manager_id: ManagerId,
        detail: String,
    },
    PlanningFailed {
        detail: String,
    },
    Finished,
}

/// Runs the terminal selection UI from manager ids while planning events arrive externally.
///
/// # Errors
///
/// Returns an I/O error for terminal setup, rendering, event reading, or typed selection
/// validation failures surfaced by the event loop.
#[expect(clippy::needless_pass_by_value)]
pub fn run_interactive_selection_with_planning_events(
    manager_ids: Vec<ManagerId>,
    planning_events: Receiver<InteractiveSelectionPlanningEvent>,
    trace_commands: bool,
) -> io::Result<InteractiveSelectionOutcome> {
    run_interactive_selection_screen(
        InteractiveSelectionScreen::from_manager_ids(manager_ids).trace_commands(trace_commands),
        &planning_events,
    )
}

fn run_interactive_selection_screen(
    mut screen: InteractiveSelectionScreen,
    planning_events: &Receiver<InteractiveSelectionPlanningEvent>,
) -> io::Result<InteractiveSelectionOutcome> {
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

    let result = run_selection_loop(&mut terminal, &mut screen, planning_events);

    let cleanup = cleanup_selection_terminal(&mut terminal);
    match (result, cleanup) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(err), Ok(()) | Err(_)) | (Ok(_), Err(err)) => Err(err),
    }
}

fn cleanup_selection_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> io::Result<()> {
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

fn run_selection_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    screen: &mut InteractiveSelectionScreen,
    planning_events: &Receiver<InteractiveSelectionPlanningEvent>,
) -> io::Result<InteractiveSelectionOutcome> {
    loop {
        drain_planning_events(screen, planning_events);
        terminal.draw(|frame| draw_selection(frame, screen))?;
        if !event::poll(Duration::from_millis(100))? {
            screen.tick();
            continue;
        }
        drain_planning_events(screen, planning_events);
        let size = terminal.size()?;
        let area = Rect::new(0, 0, size.width, size.height);
        let control = handle_selection_ready_events(screen, area)?.map_err(io::Error::other)?;
        match control {
            SelectionControl::Continue => {}
            SelectionControl::Confirm => {
                return Ok(InteractiveSelectionOutcome::Confirmed(
                    screen.selection_drafts(),
                ));
            }
            SelectionControl::Cancel => return Ok(InteractiveSelectionOutcome::Cancelled),
            SelectionControl::Interrupt => return Ok(InteractiveSelectionOutcome::Interrupted),
        }
    }
}

fn drain_planning_events(
    screen: &mut InteractiveSelectionScreen,
    planning_events: &Receiver<InteractiveSelectionPlanningEvent>,
) {
    loop {
        match planning_events.try_recv() {
            Ok(event) => screen.apply_planning_event(event),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                screen.planning_events_disconnected();
                break;
            }
        }
    }
}
