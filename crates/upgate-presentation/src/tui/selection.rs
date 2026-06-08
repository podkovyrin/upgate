use std::io;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use crate::{
    CandidateNoteKind, CandidateNotePart, SelectionRow, SelectionRowStatus, SelectionRowVisibility,
    SelectionView, TargetOption,
};
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
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Wrap};
use unicode_width::UnicodeWidthStr;
use upgate_domain::{
    ManagerId, PlanIssue, PlanItemId, SelectedItem, SelectedUpdate, UpdateSelectionPolicy,
    VersionPolicy,
};

use crate::outcome::{manager_resolved_label, version_label};
use crate::selection_view::note_part_text;
use crate::tui::components::{
    KeyBinding, TuiTable, app_block, clamp_command_log_scroll, command_log_layout, key_footer,
    key_footer_hit, render_command_log, render_modal_frame, render_selection_table,
    render_separator, render_table, render_tabs, version_picker_columns, visible_tabs,
};
use crate::tui::layout::app_frame;
use crate::tui::progress::spinner_frame;
use crate::tui::text::{truncate_with_ellipsis, version_diff_spans};
use crate::tui::theme::TuiTheme;
use crate::tui::{InteractiveSelectionState, SelectionStateError};

const TAB_KEY_LABEL: &str = " ⇥ ";
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
const PICKER_FOOTER_KEYS: &[KeyBinding<'static>] = &[
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
        label: "confirm",
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
const MAX_DRAINED_INPUT_EVENTS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveSelectionPlan {
    pub view: SelectionView,
    pub issues: Vec<PlanIssue>,
    pub selection_policy: UpdateSelectionPolicy,
    pub version_policy: VersionPolicy,
}

impl InteractiveSelectionPlan {
    pub const fn new(
        view: SelectionView,
        issues: Vec<PlanIssue>,
        selection_policy: UpdateSelectionPolicy,
        version_policy: VersionPolicy,
    ) -> Self {
        Self {
            view,
            issues,
            selection_policy,
            version_policy,
        }
    }
}

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
        issues: Vec<PlanIssue>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveSelectionScreen {
    managers: Vec<ManagerSelectionState>,
    command_log: Vec<String>,
    command_log_scroll_from_bottom: usize,
    trace_commands: bool,
    planning_finished: bool,
    planning_failure: Option<String>,
    spinner_tick: usize,
    active_tab: usize,
    tab_offset: usize,
    cursor: Option<usize>,
    table_offset: usize,
    show_all: bool,
    target_picker: Option<TargetPickerState>,
    confirmation_dialog: Option<ConfirmationDialogState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagerSelectionState {
    manager_id: ManagerId,
    version_policy: VersionPolicy,
    issues: Vec<PlanIssue>,
    planning_status: ManagerPlanningStatus,
    state: InteractiveSelectionState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ManagerPlanningStatus {
    Waiting,
    Planning,
    Ready,
    Empty,
    Error { detail: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionTabStatus {
    Loading,
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionTabRef {
    All,
    Manager(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectionTabIdentity {
    All,
    Manager(ManagerId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisibleRow {
    manager_idx: usize,
    row_idx: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TargetPickerState {
    visible_row: VisibleRow,
    cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConfirmationDialogState;

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfirmationManagerSummary {
    manager: String,
    selected_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfirmationSummary {
    selected_total: usize,
    managers: Vec<ConfirmationManagerSummary>,
}

impl InteractiveSelectionScreen {
    pub fn new(plans: Vec<InteractiveSelectionPlan>) -> Self {
        let managers = plans
            .into_iter()
            .map(|plan| {
                let manager_id = plan.view.manager_id.clone();
                let state =
                    InteractiveSelectionState::new(plan.view, plan.selection_policy.clone());
                ManagerSelectionState {
                    manager_id,
                    version_policy: plan.version_policy,
                    issues: plan.issues,
                    planning_status: if state.rows().is_empty() {
                        ManagerPlanningStatus::Empty
                    } else {
                        ManagerPlanningStatus::Ready
                    },
                    state,
                }
            })
            .collect();

        let mut screen = Self {
            managers,
            command_log: Vec::new(),
            command_log_scroll_from_bottom: 0,
            trace_commands: false,
            planning_finished: true,
            planning_failure: None,
            spinner_tick: 0,
            active_tab: 0,
            tab_offset: 0,
            cursor: None,
            table_offset: 0,
            show_all: false,
            target_picker: None,
            confirmation_dialog: None,
        };
        screen.clamp_cursor();
        screen
    }
    pub fn from_manager_ids(manager_ids: Vec<ManagerId>) -> Self {
        let managers = manager_ids
            .into_iter()
            .map(|manager_id| empty_manager_state(manager_id, ManagerPlanningStatus::Waiting))
            .collect();

        let mut screen = Self {
            managers,
            command_log: Vec::new(),
            command_log_scroll_from_bottom: 0,
            trace_commands: false,
            planning_finished: false,
            planning_failure: None,
            spinner_tick: 0,
            active_tab: 0,
            tab_offset: 0,
            cursor: None,
            table_offset: 0,
            show_all: false,
            target_picker: None,
            confirmation_dialog: None,
        };
        screen.clamp_cursor();
        screen
    }
    pub const fn trace_commands(mut self, trace_commands: bool) -> Self {
        self.trace_commands = trace_commands;
        self
    }

    pub const fn tick(&mut self) {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
    }

    pub fn apply_planning_event(&mut self, event: InteractiveSelectionPlanningEvent) {
        let active_tab = self.active_tab_identity();
        let open_picker_row = self
            .target_picker
            .map(|picker| self.row(picker.visible_row).plan_item_id.clone());
        let focused_row = open_picker_row
            .clone()
            .or_else(|| self.current_visible_row_id());
        match event {
            InteractiveSelectionPlanningEvent::ManagerStarted { manager_id } => {
                self.replace_manager_state(
                    manager_id,
                    VersionPolicy::None,
                    ManagerPlanningStatus::Planning,
                    Vec::new(),
                    UpdateSelectionPolicy::default(),
                    Vec::new(),
                );
            }
            InteractiveSelectionPlanningEvent::CommandStarted { command } => {
                self.command_log.push(command);
                if self.command_log_scroll_from_bottom > 0 {
                    self.command_log_scroll_from_bottom =
                        self.command_log_scroll_from_bottom.saturating_add(1);
                }
            }
            InteractiveSelectionPlanningEvent::ManagerReady {
                view,
                issues,
                selection_policy,
                version_policy,
            } => {
                let status = if view.rows.is_empty() {
                    ManagerPlanningStatus::Empty
                } else {
                    ManagerPlanningStatus::Ready
                };
                self.replace_manager_state(
                    view.manager_id.clone(),
                    version_policy,
                    status,
                    issues,
                    selection_policy,
                    view.rows,
                );
            }
            InteractiveSelectionPlanningEvent::ManagerError { manager_id, detail } => {
                let policy = self
                    .managers
                    .iter()
                    .find(|manager| manager.manager_id == manager_id)
                    .map_or_else(UpdateSelectionPolicy::default, |manager| {
                        manager.state.selection_policy().clone()
                    });
                let version_policy = self
                    .managers
                    .iter()
                    .find(|manager| manager.manager_id == manager_id)
                    .map_or(VersionPolicy::None, |manager| manager.version_policy);
                self.replace_manager_state(
                    manager_id,
                    version_policy,
                    ManagerPlanningStatus::Error { detail },
                    Vec::new(),
                    policy,
                    Vec::new(),
                );
            }
            InteractiveSelectionPlanningEvent::PlanningFailed { detail } => {
                self.planning_finished = true;
                self.planning_failure = Some(detail.clone());
                for manager in &mut self.managers {
                    if matches!(
                        manager.planning_status,
                        ManagerPlanningStatus::Waiting | ManagerPlanningStatus::Planning
                    ) {
                        manager.planning_status = ManagerPlanningStatus::Error {
                            detail: detail.clone(),
                        };
                    }
                }
            }
            InteractiveSelectionPlanningEvent::Finished => {
                self.planning_finished = true;
            }
        }
        self.restore_active_tab(active_tab);
        self.restore_cursor(focused_row);
        self.clamp_cursor();
        self.rebind_or_close_target_picker(open_picker_row);
    }
    pub const fn active_tab(&self) -> usize {
        self.active_tab
    }
    pub const fn cursor(&self) -> Option<usize> {
        self.cursor
    }
    pub const fn show_all(&self) -> bool {
        self.show_all
    }
    pub const fn tab_offset(&self) -> usize {
        self.tab_offset
    }
    pub const fn target_picker_open(&self) -> bool {
        self.target_picker.is_some()
    }
    const fn confirmation_dialog_open(&self) -> bool {
        self.confirmation_dialog.is_some()
    }
    pub fn target_picker_options(&self) -> Vec<TargetOption> {
        let Some(picker) = self.target_picker else {
            return Vec::new();
        };
        let row = self.row(picker.visible_row);
        row.target_options.clone()
    }
    pub fn visible_rows(&self) -> Vec<&SelectionRow> {
        self.visible_row_refs()
            .into_iter()
            .map(|visible| self.row(visible))
            .collect()
    }
    pub fn has_selectable_rows(&self) -> bool {
        self.managers.iter().any(|manager| {
            manager.state.rows().iter().any(|row| {
                row.status == SelectionRowStatus::Update
                    || row
                        .target_options
                        .iter()
                        .any(|option| matches!(option, TargetOption::ForcedCandidate { .. }))
            })
        })
    }
    pub fn placeholder_message(&self) -> Option<String> {
        if !self.visible_row_refs().is_empty() {
            return None;
        }
        if let Some(detail) = &self.planning_failure {
            return Some(detail.clone());
        }
        if let Some(active_manager_idx) = self.active_manager_idx() {
            return self
                .managers
                .get(active_manager_idx)
                .map(manager_placeholder_message);
        }
        if self
            .managers
            .iter()
            .any(|manager| manager.planning_status == ManagerPlanningStatus::Planning)
        {
            return Some("Planning...".to_owned());
        }
        if self
            .managers
            .iter()
            .any(|manager| manager.planning_status == ManagerPlanningStatus::Waiting)
        {
            return Some("Waiting to plan".to_owned());
        }
        for manager in &self.managers {
            if let ManagerPlanningStatus::Error { detail } = &manager.planning_status {
                return Some(format!("{}: {detail}", manager.manager_id));
            }
            if let Some(issue) = manager.issues.first() {
                return Some(format!(
                    "{}: {}",
                    manager.manager_id.as_str(),
                    plan_issue_label(issue)
                ));
            }
        }
        Some("No selectable updates".to_owned())
    }

    /// Applies one input event to the interactive selection state.
    ///
    /// # Errors
    ///
    /// Returns an error if the input tries to select a target that is not available for the
    /// current row, or if the resulting typed selection would not validate against the plan.
    pub fn handle_input(
        &mut self,
        input: SelectionInput,
    ) -> Result<SelectionControl, SelectionStateError> {
        if self.target_picker.is_some() {
            return self.handle_picker_input(input);
        }

        match input {
            SelectionInput::Up => self.move_cursor_up(),
            SelectionInput::Down => self.move_cursor_down(),
            SelectionInput::NextTab => self.next_tab(),
            SelectionInput::PreviousTab => self.previous_tab(),
            SelectionInput::ToggleCurrent => self.toggle_current()?,
            SelectionInput::SelectVisible => self.select_visible(true)?,
            SelectionInput::SelectNoneVisible => self.select_visible(false)?,
            SelectionInput::ToggleViewAll => {
                self.show_all = !self.show_all;
                self.clamp_cursor();
                self.table_offset = 0;
            }
            SelectionInput::OpenTargetPicker => self.open_target_picker(),
            SelectionInput::Confirm if self.planning_finished => {
                if let Some(detail) = self.planning_error_detail() {
                    return Err(SelectionStateError::PlanningFailed(detail));
                }
                self.confirmation_dialog = Some(ConfirmationDialogState);
            }
            SelectionInput::Confirm
            | SelectionInput::Ignore
            | SelectionInput::PickerUp
            | SelectionInput::PickerDown
            | SelectionInput::PickerPreviousRow
            | SelectionInput::PickerNextRow
            | SelectionInput::PickerConfirm
            | SelectionInput::PickerCancel
            | SelectionInput::RecommendedTarget => {}
            SelectionInput::Cancel => return Ok(SelectionControl::Cancel),
            SelectionInput::Interrupt => return Ok(SelectionControl::Interrupt),
        }

        Ok(SelectionControl::Continue)
    }
    pub fn selection_drafts(&self) -> Vec<InteractiveManagerSelectionDraft> {
        self.managers
            .iter()
            .map(|manager| InteractiveManagerSelectionDraft {
                manager_id: manager.manager_id.clone(),
                selected_items: manager.state.selected_items(),
                selection_policy: manager.state.selection_policy().clone(),
            })
            .collect()
    }
    fn confirmation_summary(&self) -> ConfirmationSummary {
        let managers = self
            .managers
            .iter()
            .filter_map(|manager| {
                let selected_count = manager.state.selected_items().len();
                (selected_count > 0).then(|| ConfirmationManagerSummary {
                    manager: manager.manager_id.to_string(),
                    selected_count,
                })
            })
            .collect::<Vec<_>>();
        let selected_total = managers.iter().map(|manager| manager.selected_count).sum();

        ConfirmationSummary {
            selected_total,
            managers,
        }
    }

    fn planning_error_detail(&self) -> Option<String> {
        if let Some(detail) = &self.planning_failure {
            return Some(detail.clone());
        }
        let details = self
            .managers
            .iter()
            .filter_map(|manager| {
                if let ManagerPlanningStatus::Error { detail } = &manager.planning_status {
                    return Some(format!("{}: {detail}", manager.manager_id));
                }
                None
            })
            .collect::<Vec<_>>();
        (!details.is_empty()).then(|| details.join("; "))
    }

    fn replace_manager_state(
        &mut self,
        manager_id: ManagerId,
        version_policy: VersionPolicy,
        planning_status: ManagerPlanningStatus,
        issues: Vec<PlanIssue>,
        selection_policy: UpdateSelectionPolicy,
        rows: Vec<SelectionRow>,
    ) {
        let existing_index = self
            .managers
            .iter()
            .position(|existing| existing.manager_id == manager_id);
        let view = SelectionView {
            manager_id: manager_id.clone(),
            rows,
        };
        let state = InteractiveSelectionState::new(view, selection_policy);
        let manager = ManagerSelectionState {
            manager_id,
            version_policy,
            issues,
            planning_status,
            state,
        };
        if let Some(existing_index) = existing_index {
            self.managers[existing_index] = manager;
        } else {
            self.managers.push(manager);
        }
    }

    fn handle_picker_input(
        &mut self,
        input: SelectionInput,
    ) -> Result<SelectionControl, SelectionStateError> {
        match input {
            SelectionInput::PickerCancel => self.target_picker = None,
            SelectionInput::Cancel => return Ok(SelectionControl::Cancel),
            SelectionInput::Interrupt => return Ok(SelectionControl::Interrupt),
            SelectionInput::PickerUp => self.move_picker_up(),
            SelectionInput::PickerDown => self.move_picker_down(),
            SelectionInput::PickerPreviousRow => self.move_picker_to_row(-1),
            SelectionInput::PickerNextRow => self.move_picker_to_row(1),
            SelectionInput::RecommendedTarget => self.choose_recommended_target()?,
            SelectionInput::PickerConfirm => self.confirm_picker_target()?,
            _ => {}
        }
        Ok(SelectionControl::Continue)
    }

    fn move_cursor_up(&mut self) {
        let row_count = self.visible_row_refs().len();
        if row_count == 0 {
            return;
        }
        self.cursor = Some(match self.cursor {
            None => 0,
            Some(0) => row_count - 1,
            Some(cursor) => cursor - 1,
        });
    }

    fn move_cursor_down(&mut self) {
        let row_count = self.visible_row_refs().len();
        if row_count == 0 {
            return;
        }
        self.cursor = Some(match self.cursor {
            None => 0,
            Some(cursor) if cursor + 1 >= row_count => 0,
            Some(cursor) => cursor + 1,
        });
    }

    fn scroll_table_by(&mut self, delta: isize, visible_height: usize) {
        let max_offset = self.table_max_offset(visible_height);
        if delta == 0 {
            return;
        }
        if self.cursor.is_none() {
            if !self.visible_row_refs().is_empty() {
                self.cursor = Some(0);
            }
            return;
        }

        self.table_offset = if delta.is_positive() {
            self.table_offset
                .saturating_add(delta.unsigned_abs())
                .min(max_offset)
        } else {
            self.table_offset.saturating_sub(delta.unsigned_abs())
        };
        self.clamp_cursor_to_table_view(visible_height);
    }

    fn next_tab(&mut self) {
        let tab_count = self.visible_tab_refs().len();
        if tab_count > 0 {
            self.active_tab = (self.active_tab + 1) % tab_count;
        }
        if self.cursor.is_some() {
            self.cursor = Some(0);
        }
        self.table_offset = 0;
        self.clamp_cursor();
    }

    fn select_tab(&mut self, tab_idx: usize) {
        let tab_count = self.visible_tab_refs().len();
        if tab_idx < tab_count {
            self.active_tab = tab_idx;
        }
        if self.cursor.is_some() {
            self.cursor = Some(0);
        }
        self.table_offset = 0;
        self.clamp_cursor();
    }

    fn previous_tab(&mut self) {
        let tab_count = self.visible_tab_refs().len();
        if tab_count > 0 {
            self.active_tab = if self.active_tab == 0 {
                tab_count - 1
            } else {
                self.active_tab - 1
            };
        }
        if self.cursor.is_some() {
            self.cursor = Some(0);
        }
        self.table_offset = 0;
        self.clamp_cursor();
    }

    fn toggle_current(&mut self) -> Result<(), SelectionStateError> {
        let Some(visible) = self.current_visible_row() else {
            return Ok(());
        };
        let row = self.row(visible).clone();
        let manager = &mut self.managers[visible.manager_idx];
        if manager.state.selected_target(&row.plan_item_id).is_some() {
            manager.state.deselect(&row.plan_item_id)?;
        } else if row.status == SelectionRowStatus::Update {
            manager.state.select_recommended(&row.plan_item_id)?;
        } else if row
            .target_options
            .iter()
            .any(|option| matches!(option, TargetOption::ForcedCandidate { .. }))
        {
            manager.state.force_candidate(&row.plan_item_id)?;
        }
        Ok(())
    }

    fn select_visible(&mut self, selected: bool) -> Result<(), SelectionStateError> {
        for visible in self.visible_row_refs() {
            let row = self.row(visible).clone();
            let manager = &mut self.managers[visible.manager_idx];
            if selected {
                if row.status == SelectionRowStatus::Update {
                    manager.state.select_recommended(&row.plan_item_id)?;
                }
            } else if manager.state.selected_target(&row.plan_item_id).is_some() {
                manager.state.deselect(&row.plan_item_id)?;
            }
        }
        Ok(())
    }

    fn open_target_picker(&mut self) {
        let Some(visible_row) = self.current_visible_row() else {
            return;
        };
        let row = self.row(visible_row);
        if !row.target_options.is_empty() {
            self.target_picker = Some(TargetPickerState {
                visible_row,
                cursor: self.target_picker_initial_cursor(visible_row),
            });
        }
    }

    fn rebind_or_close_target_picker(&mut self, open_picker_row: Option<PlanItemId>) {
        let Some(plan_item_id) = open_picker_row else {
            return;
        };
        let Some(mut picker) = self.target_picker else {
            return;
        };
        let Some(visible_row) = self.visible_row_for_plan_item(&plan_item_id) else {
            self.target_picker = None;
            return;
        };
        let option_count = self.target_option_count(visible_row);
        if option_count == 0 {
            self.target_picker = None;
            return;
        }
        picker.visible_row = visible_row;
        if picker.cursor >= option_count {
            picker.cursor = option_count - 1;
        }
        self.target_picker = Some(picker);
    }

    fn move_picker_up(&mut self) {
        let Some(mut picker) = self.target_picker else {
            return;
        };
        let option_count = self.target_option_count(picker.visible_row);
        if option_count == 0 {
            return;
        }
        picker.cursor = if picker.cursor == 0 {
            option_count - 1
        } else {
            picker.cursor - 1
        };
        self.target_picker = Some(picker);
    }

    fn move_picker_down(&mut self) {
        let Some(mut picker) = self.target_picker else {
            return;
        };
        let option_count = self.target_option_count(picker.visible_row);
        if option_count == 0 {
            return;
        }
        picker.cursor = if picker.cursor + 1 >= option_count {
            0
        } else {
            picker.cursor + 1
        };
        self.target_picker = Some(picker);
    }

    fn move_picker_to_row(&mut self, delta: isize) {
        let Some(picker) = self.target_picker else {
            return;
        };
        let visible_rows = self.visible_row_refs();
        let Some(current_idx) = visible_rows
            .iter()
            .position(|row| *row == picker.visible_row)
        else {
            return;
        };
        if visible_rows.is_empty() {
            return;
        }

        let mut next_idx = current_idx;
        for _ in 0..visible_rows.len() {
            next_idx = match delta.cmp(&0) {
                std::cmp::Ordering::Less => {
                    if next_idx == 0 {
                        visible_rows.len() - 1
                    } else {
                        next_idx - 1
                    }
                }
                std::cmp::Ordering::Equal => next_idx,
                std::cmp::Ordering::Greater => {
                    if next_idx + 1 >= visible_rows.len() {
                        0
                    } else {
                        next_idx + 1
                    }
                }
            };
            let next_row = visible_rows[next_idx];
            if self.target_option_count(next_row) > 0 {
                self.cursor = Some(next_idx);
                self.target_picker = Some(TargetPickerState {
                    visible_row: next_row,
                    cursor: self.target_picker_initial_cursor(next_row),
                });
                return;
            }
        }
    }

    fn choose_recommended_target(&mut self) -> Result<(), SelectionStateError> {
        let Some(picker) = self.target_picker else {
            return Ok(());
        };
        let row = self.row(picker.visible_row).clone();
        if row
            .target_options
            .iter()
            .any(|option| matches!(option, TargetOption::Recommended { .. }))
        {
            self.managers[picker.visible_row.manager_idx]
                .state
                .select_recommended(&row.plan_item_id)?;
        }
        self.target_picker = None;
        Ok(())
    }

    fn confirm_picker_target(&mut self) -> Result<(), SelectionStateError> {
        let Some(picker) = self.target_picker else {
            return Ok(());
        };
        let row = self.row(picker.visible_row).clone();
        let manager = &mut self.managers[picker.visible_row.manager_idx];
        if let Some(option) = row.target_options.get(picker.cursor) {
            match option {
                TargetOption::Recommended { .. } => {
                    manager.state.select_recommended(&row.plan_item_id)?;
                }
                TargetOption::ForcedCandidate { .. } => {
                    manager.state.force_candidate(&row.plan_item_id)?;
                }
                TargetOption::AlternateExact { target_version, .. } => {
                    manager
                        .state
                        .choose_alternate_exact(&row.plan_item_id, target_version.clone())?;
                }
                TargetOption::ManagerResolved { .. } => {
                    manager.state.choose_manager_resolved(&row.plan_item_id)?;
                }
            }
        }

        self.target_picker = None;
        Ok(())
    }

    fn target_option_count(&self, visible: VisibleRow) -> usize {
        let row = self.row(visible);
        row.target_options.len()
    }

    fn target_picker_initial_cursor(&self, visible: VisibleRow) -> usize {
        let row = self.row(visible);
        let selected_target = self.managers[visible.manager_idx]
            .state
            .selected_target(&row.plan_item_id);
        selected_target
            .and_then(|target| {
                row.target_options
                    .iter()
                    .position(|option| target_option_matches_selected(option, target))
            })
            .unwrap_or_else(|| {
                row.target_options
                    .iter()
                    .position(|option| matches!(option, TargetOption::Recommended { .. }))
                    .unwrap_or(0)
            })
    }

    fn clamp_cursor(&mut self) {
        self.clamp_active_tab();
        let row_count = self.visible_row_refs().len();
        if row_count == 0 {
            self.cursor = None;
        } else if let Some(cursor) = self.cursor
            && cursor >= row_count
        {
            self.cursor = Some(row_count - 1);
        }
    }

    fn clamp_table_offset(&mut self, visible_height: usize) {
        self.table_offset = self.table_offset.min(self.table_max_offset(visible_height));
    }

    fn keep_cursor_visible(&mut self, visible_height: usize) {
        if visible_height == 0 {
            self.table_offset = 0;
            return;
        }
        self.clamp_table_offset(visible_height);
        let Some(cursor) = self.cursor else {
            return;
        };
        if cursor < self.table_offset {
            self.table_offset = cursor;
        } else if cursor >= self.table_offset.saturating_add(visible_height) {
            self.table_offset = cursor + 1 - visible_height;
        }
    }

    fn table_max_offset(&self, visible_height: usize) -> usize {
        self.visible_row_refs().len().saturating_sub(visible_height)
    }

    fn clamp_cursor_to_table_view(&mut self, visible_height: usize) {
        if visible_height == 0 {
            self.cursor = None;
            return;
        }
        self.clamp_cursor();
        let Some(cursor) = self.cursor else {
            return;
        };
        if cursor < self.table_offset {
            self.cursor = Some(self.table_offset);
        } else {
            let last_visible = self
                .table_offset
                .saturating_add(visible_height)
                .saturating_sub(1);
            if cursor > last_visible {
                self.cursor =
                    Some(last_visible.min(self.visible_row_refs().len().saturating_sub(1)));
            }
        }
    }

    fn clamp_active_tab(&mut self) {
        let tab_count = self.visible_tab_refs().len();
        if tab_count == 0 {
            self.active_tab = 0;
        } else if self.active_tab >= tab_count {
            self.active_tab = tab_count - 1;
        }
    }

    fn current_visible_row(&self) -> Option<VisibleRow> {
        self.visible_row_refs().get(self.cursor?).copied()
    }

    fn current_visible_row_id(&self) -> Option<PlanItemId> {
        self.current_visible_row()
            .map(|visible| self.row(visible).plan_item_id.clone())
    }

    fn visible_row_for_plan_item(&self, plan_item_id: &PlanItemId) -> Option<VisibleRow> {
        self.visible_row_refs()
            .into_iter()
            .find(|visible| self.row(*visible).plan_item_id == *plan_item_id)
    }

    fn visible_row_refs(&self) -> Vec<VisibleRow> {
        let active_manager_idx = self.active_manager_idx();
        let mut rows = Vec::new();
        for (manager_idx, manager) in self.managers.iter().enumerate() {
            if let Some(active_manager_idx) = active_manager_idx
                && active_manager_idx != manager_idx
            {
                continue;
            }
            for (row_idx, row) in manager.state.rows().iter().enumerate() {
                if self.show_all
                    || row.default_visibility == SelectionRowVisibility::Visible
                    || manager.state.selected_target(&row.plan_item_id).is_some()
                {
                    rows.push(VisibleRow {
                        manager_idx,
                        row_idx,
                    });
                }
            }
        }
        rows
    }

    fn visible_tab_refs(&self) -> Vec<SelectionTabRef> {
        std::iter::once(SelectionTabRef::All)
            .chain(
                self.managers
                    .iter()
                    .enumerate()
                    .filter(|(_, manager)| manager.planning_status != ManagerPlanningStatus::Empty)
                    .map(|(manager_idx, _)| SelectionTabRef::Manager(manager_idx)),
            )
            .collect()
    }

    fn active_manager_idx(&self) -> Option<usize> {
        match self.visible_tab_refs().get(self.active_tab).copied() {
            Some(SelectionTabRef::Manager(manager_idx)) => Some(manager_idx),
            Some(SelectionTabRef::All) | None => None,
        }
    }

    fn active_tab_identity(&self) -> Option<SelectionTabIdentity> {
        self.visible_tab_refs()
            .get(self.active_tab)
            .copied()
            .map(|tab| match tab {
                SelectionTabRef::All => SelectionTabIdentity::All,
                SelectionTabRef::Manager(manager_idx) => {
                    SelectionTabIdentity::Manager(self.managers[manager_idx].manager_id.clone())
                }
            })
    }

    fn restore_active_tab(&mut self, active_tab: Option<SelectionTabIdentity>) {
        let Some(active_tab) = active_tab else {
            return;
        };
        if let Some(tab_idx) =
            self.visible_tab_refs()
                .into_iter()
                .position(|tab| match (&active_tab, tab) {
                    (SelectionTabIdentity::All, SelectionTabRef::All) => true,
                    (
                        SelectionTabIdentity::Manager(manager_id),
                        SelectionTabRef::Manager(manager_idx),
                    ) => self.managers[manager_idx].manager_id == *manager_id,
                    _ => false,
                })
        {
            self.active_tab = tab_idx;
        }
    }

    fn restore_cursor(&mut self, focused_row: Option<PlanItemId>) {
        let Some(focused_row) = focused_row else {
            return;
        };
        if let Some(row_idx) = self
            .visible_row_refs()
            .into_iter()
            .position(|visible| self.row(visible).plan_item_id == focused_row)
        {
            self.cursor = Some(row_idx);
        }
    }

    fn row(&self, visible: VisibleRow) -> &SelectionRow {
        &self.managers[visible.manager_idx].state.rows()[visible.row_idx]
    }

    fn scroll_command_log_by(&mut self, delta: isize, visible_height: usize) {
        let next_scroll = if delta.is_positive() {
            self.command_log_scroll_from_bottom
                .saturating_add(delta.unsigned_abs())
        } else {
            self.command_log_scroll_from_bottom
                .saturating_sub(delta.unsigned_abs())
        };
        self.command_log_scroll_from_bottom =
            clamp_command_log_scroll(next_scroll, self.command_log.len(), visible_height);
    }

    fn clamp_command_log_scroll(&mut self, visible_height: usize) {
        self.command_log_scroll_from_bottom = clamp_command_log_scroll(
            self.command_log_scroll_from_bottom,
            self.command_log.len(),
            visible_height,
        );
    }
}

fn empty_manager_state(
    manager_id: ManagerId,
    planning_status: ManagerPlanningStatus,
) -> ManagerSelectionState {
    let view = SelectionView {
        manager_id: manager_id.clone(),
        rows: Vec::new(),
    };
    ManagerSelectionState {
        manager_id,
        version_policy: VersionPolicy::None,
        issues: Vec::new(),
        planning_status,
        state: InteractiveSelectionState::new(view, UpdateSelectionPolicy::default()),
    }
}

fn manager_placeholder_message(manager: &ManagerSelectionState) -> String {
    match &manager.planning_status {
        ManagerPlanningStatus::Waiting => "Waiting to plan".to_owned(),
        ManagerPlanningStatus::Planning => "Planning...".to_owned(),
        ManagerPlanningStatus::Ready | ManagerPlanningStatus::Empty => manager
            .issues
            .first()
            .map_or_else(|| "No selectable updates".to_owned(), plan_issue_label),
        ManagerPlanningStatus::Error { detail } => detail.clone(),
    }
}

/// Runs the terminal selection UI and returns confirm or cancel.
///
/// # Errors
///
/// Returns an I/O error for terminal setup, rendering, event reading, or typed selection
/// validation failures surfaced by the event loop.
pub fn run_interactive_selection(
    plans: Vec<InteractiveSelectionPlan>,
) -> io::Result<InteractiveSelectionOutcome> {
    run_interactive_selection_screen(InteractiveSelectionScreen::new(plans), None)
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
        Some(&planning_events),
    )
}

fn run_interactive_selection_screen(
    mut screen: InteractiveSelectionScreen,
    planning_events: Option<&Receiver<InteractiveSelectionPlanningEvent>>,
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
    planning_events: Option<&Receiver<InteractiveSelectionPlanningEvent>>,
) -> io::Result<InteractiveSelectionOutcome> {
    loop {
        drain_planning_events(screen, planning_events)?;
        terminal.draw(|frame| draw_selection(frame, screen))?;
        if !event::poll(Duration::from_millis(100))? {
            screen.tick();
            continue;
        }
        drain_planning_events(screen, planning_events)?;
        let size = terminal.size()?;
        let area = Rect::new(0, 0, size.width, size.height);
        let control = handle_selection_ready_events(screen, area)?
            .map_err(|err| io::Error::other(err.to_string()))?;
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

fn handle_selection_ready_events(
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
            screen.confirmation_dialog = None;
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

    if screen.target_picker.is_some() {
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
                screen.cursor = Some(row_idx);
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
    let Some(picker) = screen.target_picker else {
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
        screen.target_picker = Some(TargetPickerState {
            visible_row: picker.visible_row,
            cursor: option_idx,
        });
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
        screen.tab_offset,
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

fn selection_table_visible_height(area: Rect) -> usize {
    usize::from(area.height.saturating_sub(1))
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

fn drain_planning_events(
    screen: &mut InteractiveSelectionScreen,
    planning_events: Option<&Receiver<InteractiveSelectionPlanningEvent>>,
) -> io::Result<()> {
    let Some(planning_events) = planning_events else {
        return Ok(());
    };
    loop {
        match planning_events.try_recv() {
            Ok(event) => screen.apply_planning_event(event),
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => {
                if !screen.planning_finished {
                    screen.apply_planning_event(
                        InteractiveSelectionPlanningEvent::PlanningFailed {
                            detail: "planning stopped before reporting completion".to_owned(),
                        },
                    );
                }
                return Ok(());
            }
        }
    }
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

fn draw_selection(frame: &mut ratatui::Frame<'_>, screen: &mut InteractiveSelectionScreen) {
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

    if let Some(picker) = screen.target_picker {
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
        screen.tab_offset,
        tabs_area.width,
    );
    screen.tab_offset = tabs.start;
    render_tabs(frame, tabs_area, &tabs, theme);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(TAB_KEY_LABEL, theme.keycap))),
        key_area,
    );
}

fn selection_tab_titles(
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
            Span::raw(label.to_owned()),
            Span::raw(" "),
            Span::styled(spinner_frame(spinner_tick), theme.running),
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
        .map(|(idx, row)| selection_table_row(row, screen.cursor == Some(idx), theme))
        .collect::<Vec<_>>();

    let selected = screen.cursor.filter(|cursor| *cursor < render_rows.len());
    render_selection_table(
        frame,
        area,
        table_rows,
        selected,
        screen.table_offset,
        theme,
    );
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
    let style = theme.row_for_selectable_state(highlighted, false);
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
        forced_note_cell(&row.note_parts, style, highlighted, theme)
    } else {
        Cell::new(note_line(&row.note_parts, theme)).style(theme.note_for(style))
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

fn forced_note_cell(
    note_parts: &[CandidateNotePart],
    base_style: Style,
    highlighted: bool,
    theme: &TuiTheme,
) -> Cell<'static> {
    let forced_style = theme.forced_note_for(highlighted);
    let mut spans = vec![Span::styled("forced", forced_style)];
    let note = note_text(note_parts);
    if !note.is_empty() {
        spans.push(Span::styled(", ", theme.note_for(base_style)));
        spans.push(Span::styled(note, theme.note_for(base_style)));
    }

    Cell::new(Line::from(spans)).style(theme.note_for(base_style))
}

fn footer_line(screen: &InteractiveSelectionScreen, width: u16, theme: &TuiTheme) -> Line<'static> {
    if screen.confirmation_dialog_open() {
        return Line::raw("");
    }

    if screen.target_picker.is_some() {
        return picker_footer_line(theme);
    }

    key_footer(selection_footer_bindings(width), theme)
}

const fn selection_footer_bindings(width: u16) -> &'static [KeyBinding<'static>] {
    if width < MINIMAL_FOOTER_WIDTH {
        MINIMAL_FOOTER_KEYS
    } else if width < COMPACT_FOOTER_WIDTH {
        COMPACT_FOOTER_KEYS
    } else {
        FOOTER_KEYS
    }
}

const fn selection_footer_inputs(width: u16) -> &'static [Option<SelectionInput>] {
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
    let style = theme.row_for_selectable_state(highlighted, false);
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

fn target_picker_height(option_count: usize) -> u16 {
    let body = u16::try_from(option_count.min(10)).unwrap_or(10);
    body.saturating_add(13).clamp(14, 23)
}

fn target_picker_width(area: Rect) -> u16 {
    area.width.saturating_sub(4).clamp(62, 96)
}

const fn version_policy_dialog_label(policy: VersionPolicy) -> &'static str {
    match policy {
        VersionPolicy::None => "none",
        VersionPolicy::Stable => "stable",
        VersionPolicy::SameTrack => "same track",
    }
}

fn target_option_matches_selected(option: &TargetOption, target: &SelectedUpdate) -> bool {
    match (option, target) {
        (TargetOption::Recommended { .. }, SelectedUpdate::Recommended)
        | (TargetOption::ForcedCandidate { .. }, SelectedUpdate::ForcePlannedCandidate)
        | (TargetOption::ManagerResolved { .. }, SelectedUpdate::ManagerResolved) => true,
        (
            TargetOption::AlternateExact { target_version, .. },
            SelectedUpdate::Exact {
                target_version: selected,
            },
        ) => target_version == selected,
        _ => false,
    }
}

fn note_line(note_parts: &[CandidateNotePart], theme: &TuiTheme) -> Line<'static> {
    let mut spans = Vec::new();
    for (idx, part) in note_parts.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled("; ", theme.note_for(Style::default())));
        }
        spans.push(Span::styled(
            note_part_text(part),
            theme.note_for(Style::default()),
        ));
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

fn plan_issue_label(issue: &PlanIssue) -> String {
    match issue {
        PlanIssue::DiscoveryFailed { detail } => detail.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use upgate_domain::{PackageName, VersionText};

    fn manager_id(value: &'static str) -> ManagerId {
        ManagerId::new(value).expect("valid manager id")
    }

    fn version(value: &str) -> VersionText {
        VersionText::new(value).expect("valid version")
    }

    fn row(manager: &str, package: &str) -> SelectionRow {
        SelectionRow {
            plan_item_id: PlanItemId::new(format!("{manager}:{package}"))
                .expect("valid plan item id"),
            package_name: PackageName::new(package).expect("valid package name"),
            installed_version: version("1.0.0"),
            target_version: Some(version("2.0.0")),
            status: SelectionRowStatus::Update,
            default_visibility: SelectionRowVisibility::Visible,
            notes: Vec::new(),
            initially_selected: true,
            target_options: vec![TargetOption::Recommended {
                target_version: version("2.0.0"),
                note_parts: Vec::new(),
            }],
        }
    }

    fn plan(manager: &'static str, packages: &[&str]) -> InteractiveSelectionPlan {
        InteractiveSelectionPlan::new(
            SelectionView {
                manager_id: manager_id(manager),
                rows: packages
                    .iter()
                    .map(|package| row(manager, package))
                    .collect(),
            },
            Vec::new(),
            UpdateSelectionPolicy::include_all(),
            VersionPolicy::None,
        )
    }

    fn ready_event(manager: &'static str, packages: &[&str]) -> InteractiveSelectionPlanningEvent {
        InteractiveSelectionPlanningEvent::ManagerReady {
            view: SelectionView {
                manager_id: manager_id(manager),
                rows: packages
                    .iter()
                    .map(|package| row(manager, package))
                    .collect(),
            },
            issues: Vec::new(),
            selection_policy: UpdateSelectionPolicy::include_all(),
            version_policy: VersionPolicy::None,
        }
    }

    #[test]
    fn planning_rows_do_not_create_an_implicit_cursor() {
        let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![manager_id("npm")]);

        screen.apply_planning_event(ready_event("npm", &["alpha"]));
        assert_eq!(screen.cursor(), None);

        screen.apply_planning_event(ready_event("npm", &["beta", "alpha"]));
        assert_eq!(screen.cursor(), None);
    }

    #[test]
    fn first_keyboard_navigation_starts_at_row_zero() {
        let mut screen = InteractiveSelectionScreen::new(vec![plan("npm", &["alpha", "beta"])]);

        screen
            .handle_input(SelectionInput::Down)
            .expect("navigation should succeed");
        assert_eq!(screen.cursor(), Some(0));

        screen
            .handle_input(SelectionInput::Down)
            .expect("navigation should succeed");
        assert_eq!(screen.cursor(), Some(1));

        let mut screen = InteractiveSelectionScreen::new(vec![plan("npm", &["alpha", "beta"])]);
        screen
            .handle_input(SelectionInput::Up)
            .expect("navigation should succeed");
        assert_eq!(screen.cursor(), Some(0));
    }

    #[test]
    fn tab_switch_does_not_select_row_until_navigation_starts() {
        let mut screen = InteractiveSelectionScreen::new(vec![
            plan("npm", &["alpha"]),
            plan("cargo", &["beta"]),
        ]);

        screen
            .handle_input(SelectionInput::NextTab)
            .expect("tab switch should succeed");
        assert_eq!(screen.cursor(), None);

        screen.select_tab(2);
        assert_eq!(screen.cursor(), None);
    }

    #[test]
    fn tab_switch_preserves_existing_reset_behavior_after_navigation_starts() {
        let mut screen = InteractiveSelectionScreen::new(vec![
            plan("npm", &["alpha", "beta"]),
            plan("cargo", &["gamma"]),
        ]);

        screen
            .handle_input(SelectionInput::Down)
            .expect("navigation should succeed");
        screen
            .handle_input(SelectionInput::Down)
            .expect("navigation should succeed");
        assert_eq!(screen.cursor(), Some(1));

        screen
            .handle_input(SelectionInput::NextTab)
            .expect("tab switch should succeed");
        assert_eq!(screen.cursor(), Some(0));
    }

    #[test]
    fn first_mouse_scroll_selects_row_zero_before_scrolling() {
        let mut screen = InteractiveSelectionScreen::new(vec![plan(
            "npm",
            &["alpha", "beta", "gamma", "delta"],
        )]);

        screen.scroll_table_by(1, 2);

        assert_eq!(screen.cursor(), Some(0));
        assert_eq!(screen.table_offset, 0);
    }
}
