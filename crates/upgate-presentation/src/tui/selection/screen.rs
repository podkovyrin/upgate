use upgate_domain::{ManagerId, PlanItemId, SelectedUpdate, UpdateSelectionPolicy, VersionPolicy};

use super::{
    InteractiveManagerSelectionDraft, InteractiveSelectionPlanningEvent, SelectionControl,
    SelectionInput,
};
use crate::tui::components::clamp_command_log_scroll;
use crate::tui::selection_state::{InteractiveSelectionState, SelectionStateError};
use crate::{
    SelectionRow, SelectionRowStatus, SelectionRowVisibility, SelectionView, TargetOption,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InteractiveSelectionScreen {
    pub(super) managers: Vec<ManagerSelectionState>,
    pub(super) command_log: Vec<String>,
    pub(super) command_log_scroll_from_bottom: usize,
    pub(super) trace_commands: bool,
    planning_finished: bool,
    planning_failure: Option<String>,
    pub(super) spinner_tick: usize,
    pub(super) active_tab: usize,
    tab_offset: usize,
    cursor: Option<usize>,
    pub(super) table_offset: usize,
    show_all: bool,
    target_picker: Option<TargetPickerState>,
    confirmation_dialog: Option<ConfirmationDialogState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagerSelectionState {
    pub(super) manager_id: ManagerId,
    pub(super) version_policy: VersionPolicy,
    pub(super) planning_status: ManagerPlanningStatus,
    pub(super) state: InteractiveSelectionState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ManagerPlanningStatus {
    Waiting,
    Planning,
    Ready,
    Empty,
    Error { detail: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SelectionTabStatus {
    Loading,
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SelectionTabRef {
    All,
    Manager(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectionTabIdentity {
    All,
    Manager(ManagerId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VisibleRow {
    pub(super) manager_idx: usize,
    pub(super) row_idx: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TargetPickerState {
    pub(super) visible_row: VisibleRow,
    pub(super) cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ConfirmationDialogState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConfirmationManagerSummary {
    pub(super) manager: String,
    pub(super) selected_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConfirmationSummary {
    pub(super) selected_total: usize,
    pub(super) managers: Vec<ConfirmationManagerSummary>,
}

impl InteractiveSelectionScreen {
    pub(super) fn from_manager_ids(manager_ids: Vec<ManagerId>) -> Self {
        let managers = manager_ids
            .into_iter()
            .map(|manager_id| {
                let view = SelectionView {
                    manager_id: manager_id.clone(),
                    rows: Vec::new(),
                };
                ManagerSelectionState {
                    manager_id,
                    version_policy: VersionPolicy::None,
                    planning_status: ManagerPlanningStatus::Waiting,
                    state: InteractiveSelectionState::new(view, UpdateSelectionPolicy::default()),
                }
            })
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
    pub(super) const fn trace_commands(mut self, trace_commands: bool) -> Self {
        self.trace_commands = trace_commands;
        self
    }

    pub(super) const fn tick(&mut self) {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
    }

    pub(super) fn apply_planning_event(&mut self, event: InteractiveSelectionPlanningEvent) {
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
                    selection_policy,
                    view.rows,
                );
            }
            InteractiveSelectionPlanningEvent::ManagerError { manager_id, detail } => {
                let existing = self
                    .managers
                    .iter()
                    .find(|manager| manager.manager_id == manager_id);
                let policy = existing.map_or_else(UpdateSelectionPolicy::default, |manager| {
                    manager.state.selection_policy().clone()
                });
                let version_policy =
                    existing.map_or(VersionPolicy::None, |manager| manager.version_policy);
                self.replace_manager_state(
                    manager_id,
                    version_policy,
                    ManagerPlanningStatus::Error { detail },
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
    /// Marks the planning event source as gone; planning that never reported
    /// completion is surfaced as a planning failure.
    pub(super) fn planning_events_disconnected(&mut self) {
        if !self.planning_finished {
            self.apply_planning_event(InteractiveSelectionPlanningEvent::PlanningFailed {
                detail: "planning stopped before reporting completion".to_owned(),
            });
        }
    }
    pub(super) const fn target_picker_open(&self) -> bool {
        self.target_picker.is_some()
    }
    pub(super) const fn target_picker(&self) -> Option<TargetPickerState> {
        self.target_picker
    }
    pub(super) fn set_target_picker_cursor(&mut self, cursor: usize) {
        let Some(picker) = self.target_picker else {
            return;
        };
        if cursor < self.target_option_count(picker.visible_row) {
            self.target_picker = Some(TargetPickerState {
                visible_row: picker.visible_row,
                cursor,
            });
        }
    }
    pub(super) const fn confirmation_dialog_open(&self) -> bool {
        self.confirmation_dialog.is_some()
    }
    pub(super) const fn close_confirmation_dialog(&mut self) {
        self.confirmation_dialog = None;
    }
    pub(super) const fn cursor(&self) -> Option<usize> {
        self.cursor
    }
    pub(super) fn set_cursor_row(&mut self, row_idx: usize) {
        self.cursor = Some(row_idx);
        self.clamp_cursor();
    }
    pub(super) const fn tab_offset(&self) -> usize {
        self.tab_offset
    }
    pub(super) const fn sync_tab_offset(&mut self, tab_offset: usize) {
        self.tab_offset = tab_offset;
    }
    pub(super) fn placeholder_message(&self) -> Option<String> {
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
        }
        Some("No selectable updates".to_owned())
    }

    /// Applies one input event to the interactive selection state.
    ///
    /// # Errors
    ///
    /// Returns an error if the input tries to select a target that is not available for the
    /// current row, or if the resulting typed selection would not validate against the plan.
    pub(super) fn handle_input(
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
    pub(super) fn selection_drafts(&self) -> Vec<InteractiveManagerSelectionDraft> {
        self.managers
            .iter()
            .map(|manager| InteractiveManagerSelectionDraft {
                manager_id: manager.manager_id.clone(),
                selected_items: manager.state.selected_items(),
                selection_policy: manager.state.selection_policy().clone(),
            })
            .collect()
    }
    pub(super) fn confirmation_summary(&self) -> ConfirmationSummary {
        let managers = self
            .managers
            .iter()
            .filter_map(|manager| {
                let selected_count = manager.state.selected_count();
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
            SelectionInput::Up
            | SelectionInput::Down
            | SelectionInput::NextTab
            | SelectionInput::PreviousTab
            | SelectionInput::ToggleCurrent
            | SelectionInput::SelectVisible
            | SelectionInput::SelectNoneVisible
            | SelectionInput::ToggleViewAll
            | SelectionInput::OpenTargetPicker
            | SelectionInput::Confirm
            | SelectionInput::Ignore => {}
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

    pub(super) fn scroll_table_by(&mut self, delta: isize, visible_height: usize) {
        if delta == 0 {
            return;
        }
        if self.cursor.is_none() {
            if !self.visible_row_refs().is_empty() {
                self.cursor = Some(0);
            }
            return;
        }

        let max_offset = self.table_max_offset(visible_height);
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

    pub(super) fn select_tab(&mut self, tab_idx: usize) {
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
        let row = self.row(visible);
        let plan_item_id = row.plan_item_id.clone();
        let is_update = row.status == SelectionRowStatus::Update;
        let has_forced_candidate = row
            .target_options
            .iter()
            .any(|option| matches!(option, TargetOption::ForcedCandidate { .. }));
        let manager = &mut self.managers[visible.manager_idx];
        if manager.state.selected_target(&plan_item_id).is_some() {
            manager.state.deselect(&plan_item_id)?;
        } else if is_update {
            manager.state.select_recommended(&plan_item_id)?;
        } else if has_forced_candidate {
            manager.state.force_candidate(&plan_item_id)?;
        }
        Ok(())
    }

    fn select_visible(&mut self, selected: bool) -> Result<(), SelectionStateError> {
        for visible in self.visible_row_refs() {
            let row = self.row(visible);
            let plan_item_id = row.plan_item_id.clone();
            let is_update = row.status == SelectionRowStatus::Update;
            let manager = &mut self.managers[visible.manager_idx];
            if selected {
                if is_update {
                    manager.state.select_recommended(&plan_item_id)?;
                }
            } else if manager.state.selected_target(&plan_item_id).is_some() {
                manager.state.deselect(&plan_item_id)?;
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
        let row = self.row(picker.visible_row);
        let plan_item_id = row.plan_item_id.clone();
        let has_recommended = row
            .target_options
            .iter()
            .any(|option| matches!(option, TargetOption::Recommended { .. }));
        if has_recommended {
            self.managers[picker.visible_row.manager_idx]
                .state
                .select_recommended(&plan_item_id)?;
        }
        self.target_picker = None;
        Ok(())
    }

    fn confirm_picker_target(&mut self) -> Result<(), SelectionStateError> {
        let Some(picker) = self.target_picker else {
            return Ok(());
        };
        let row = self.row(picker.visible_row);
        let plan_item_id = row.plan_item_id.clone();
        let option = row.target_options.get(picker.cursor).cloned();
        let manager = &mut self.managers[picker.visible_row.manager_idx];
        if let Some(option) = option {
            match option {
                TargetOption::Recommended { .. } => {
                    manager.state.select_recommended(&plan_item_id)?;
                }
                TargetOption::ForcedCandidate { .. } => {
                    manager.state.force_candidate(&plan_item_id)?;
                }
                TargetOption::AlternateExact { target_version, .. } => {
                    manager
                        .state
                        .choose_alternate_exact(&plan_item_id, target_version)?;
                }
                TargetOption::ManagerResolved { .. } => {
                    manager.state.choose_manager_resolved(&plan_item_id)?;
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

    pub(super) fn clamp_cursor(&mut self) {
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

    pub(super) fn keep_cursor_visible(&mut self, visible_height: usize) {
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

    pub(super) fn visible_row_refs(&self) -> Vec<VisibleRow> {
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

    pub(super) fn visible_tab_refs(&self) -> Vec<SelectionTabRef> {
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

    pub(super) fn row(&self, visible: VisibleRow) -> &SelectionRow {
        &self.managers[visible.manager_idx].state.rows()[visible.row_idx]
    }

    pub(super) fn scroll_command_log_by(&mut self, delta: isize, visible_height: usize) {
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

    pub(super) fn clamp_command_log_scroll(&mut self, visible_height: usize) {
        self.command_log_scroll_from_bottom = clamp_command_log_scroll(
            self.command_log_scroll_from_bottom,
            self.command_log.len(),
            visible_height,
        );
    }
}

pub(super) fn target_option_matches_selected(
    option: &TargetOption,
    target: &SelectedUpdate,
) -> bool {
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

fn manager_placeholder_message(manager: &ManagerSelectionState) -> String {
    match &manager.planning_status {
        ManagerPlanningStatus::Waiting => "Waiting to plan".to_owned(),
        ManagerPlanningStatus::Planning => "Planning...".to_owned(),
        ManagerPlanningStatus::Ready | ManagerPlanningStatus::Empty => {
            "No selectable updates".to_owned()
        }
        ManagerPlanningStatus::Error { detail } => detail.clone(),
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

    fn ready_event(manager: &'static str, packages: &[&str]) -> InteractiveSelectionPlanningEvent {
        InteractiveSelectionPlanningEvent::ManagerReady {
            view: SelectionView {
                manager_id: manager_id(manager),
                rows: packages
                    .iter()
                    .map(|package| row(manager, package))
                    .collect(),
            },
            selection_policy: UpdateSelectionPolicy::include_all(),
            version_policy: VersionPolicy::None,
        }
    }

    #[test]
    fn planning_rows_do_not_create_an_implicit_cursor() {
        let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![manager_id("npm")]);

        screen.apply_planning_event(ready_event("npm", &["alpha"]));
        assert_eq!(screen.cursor, None);

        screen.apply_planning_event(ready_event("npm", &["beta", "alpha"]));
        assert_eq!(screen.cursor, None);
    }

    #[test]
    fn first_keyboard_navigation_starts_at_row_zero() {
        let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![manager_id("npm")]);
        screen.apply_planning_event(ready_event("npm", &["alpha", "beta"]));

        screen
            .handle_input(SelectionInput::Down)
            .expect("navigation should succeed");
        assert_eq!(screen.cursor, Some(0));

        screen
            .handle_input(SelectionInput::Down)
            .expect("navigation should succeed");
        assert_eq!(screen.cursor, Some(1));

        let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![manager_id("npm")]);
        screen.apply_planning_event(ready_event("npm", &["alpha", "beta"]));
        screen
            .handle_input(SelectionInput::Up)
            .expect("navigation should succeed");
        assert_eq!(screen.cursor, Some(0));
    }

    #[test]
    fn tab_switch_does_not_select_row_until_navigation_starts() {
        let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![
            manager_id("npm"),
            manager_id("cargo"),
        ]);
        screen.apply_planning_event(ready_event("npm", &["alpha"]));
        screen.apply_planning_event(ready_event("cargo", &["beta"]));

        screen
            .handle_input(SelectionInput::NextTab)
            .expect("tab switch should succeed");
        assert_eq!(screen.cursor, None);

        screen.select_tab(2);
        assert_eq!(screen.cursor, None);
    }

    #[test]
    fn tab_switch_preserves_existing_reset_behavior_after_navigation_starts() {
        let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![
            manager_id("npm"),
            manager_id("cargo"),
        ]);
        screen.apply_planning_event(ready_event("npm", &["alpha", "beta"]));
        screen.apply_planning_event(ready_event("cargo", &["gamma"]));

        screen
            .handle_input(SelectionInput::Down)
            .expect("navigation should succeed");
        screen
            .handle_input(SelectionInput::Down)
            .expect("navigation should succeed");
        assert_eq!(screen.cursor, Some(1));

        screen
            .handle_input(SelectionInput::NextTab)
            .expect("tab switch should succeed");
        assert_eq!(screen.cursor, Some(0));
    }

    #[test]
    fn first_mouse_scroll_selects_row_zero_before_scrolling() {
        let mut screen = InteractiveSelectionScreen::from_manager_ids(vec![manager_id("npm")]);
        screen.apply_planning_event(ready_event("npm", &["alpha", "beta", "gamma", "delta"]));

        screen.scroll_table_by(1, 2);

        assert_eq!(screen.cursor, Some(0));
        assert_eq!(screen.table_offset, 0);
    }
}
