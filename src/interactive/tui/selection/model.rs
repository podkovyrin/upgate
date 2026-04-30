use std::collections::BTreeSet;

use unicode_width::UnicodeWidthStr;

use super::{SelectionPlan, SelectionPlanningTask, SelectionResult};
use crate::config::is_pinned;
use crate::managers::ApplyCandidate;

const PLANNING_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    All,
    Manager(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VisibleRow {
    pub(super) manager_idx: usize,
    pub(super) candidate_idx: usize,
}

#[derive(Debug, Clone)]
pub(super) struct ManagerSelection {
    pub(super) manager_id: &'static str,
    pub(super) plan_idx: usize,
    pub(super) selected: Vec<bool>,
    pub(super) selected_version_idx: Vec<usize>,
}

impl ManagerSelection {
    pub(super) fn candidates<'a>(
        &self,
        plans: &'a [SelectionPlan],
    ) -> Option<&'a [ApplyCandidate]> {
        plans
            .get(self.plan_idx)
            .map(|planned| planned.plan.candidates.as_slice())
    }
}

#[derive(Debug, Clone)]
struct ManagerTab {
    manager_id: &'static str,
    content: ManagerTabContent,
}

#[derive(Debug, Clone)]
enum ManagerTabContent {
    Loading { message: String },
    Items(ManagerSelection),
    Error { message: String },
}

#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct SelectionApp {
    tabs: Vec<ManagerTab>,
    pub(super) planning_done: bool,
    pub(super) cancel_requested: bool,
    pub(super) had_manager_failure: bool,
    pub(super) interrupted: bool,
    pub(super) active_tab_idx: usize,
    pub(super) tab_offset: usize,
    pub(super) cursor_idx: usize,
    show_all: bool,
    visible_rows: Vec<VisibleRow>,
    visible_rows_dirty: bool,
    marquee_tick: usize,
    pub(super) version_picker: Option<VersionPickerState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VersionPickerState {
    pub(super) manager: usize,
    pub(super) candidate: usize,
    pub(super) cursor: usize,
}

pub(super) enum SelectionContentState {
    List,
    Placeholder { message: String },
    Error { message: String },
}

impl SelectionApp {
    pub(super) fn new_lazy(tasks: &[SelectionPlanningTask]) -> Self {
        Self {
            tabs: tasks
                .iter()
                .map(|task| ManagerTab {
                    manager_id: task.manager_id,
                    content: ManagerTabContent::Loading {
                        message: "Waiting to plan".to_string(),
                    },
                })
                .collect(),
            planning_done: tasks.is_empty(),
            cancel_requested: false,
            had_manager_failure: false,
            interrupted: false,
            active_tab_idx: 0,
            tab_offset: 0,
            cursor_idx: 0,
            show_all: false,
            visible_rows: Vec::new(),
            visible_rows_dirty: true,
            marquee_tick: 0,
            version_picker: None,
        }
    }

    pub(super) fn finish_manager_plan(
        &mut self,
        manager_id: &'static str,
        plan_idx: usize,
        candidates: &[ApplyCandidate],
        pinned: &BTreeSet<String>,
    ) {
        if candidates.is_empty() {
            self.finish_empty_manager_plan(manager_id);
            return;
        }

        let selection = manager_selection(manager_id, plan_idx, candidates, pinned);
        if let Some(tab) = self.tab_mut(manager_id) {
            tab.content = ManagerTabContent::Items(selection);
        } else {
            self.tabs.push(ManagerTab {
                manager_id,
                content: ManagerTabContent::Items(selection),
            });
        }

        self.invalidate_visible_rows();
        self.clamp_active_tab();
    }

    pub(super) fn finish_empty_manager_plan(&mut self, manager_id: &'static str) {
        self.remove_manager_tab(manager_id);
        self.invalidate_visible_rows();
        self.clamp_active_tab();
    }

    pub(super) fn set_manager_loading(
        &mut self,
        manager_id: &'static str,
        message: impl Into<String>,
    ) {
        if let Some(tab) = self.tab_mut(manager_id) {
            tab.content = ManagerTabContent::Loading {
                message: message.into(),
            };
        }
    }

    pub(super) fn set_manager_error(
        &mut self,
        manager_id: &'static str,
        message: impl Into<String>,
    ) {
        let message = message.into();
        if let Some(tab) = self.tab_mut(manager_id) {
            tab.content = ManagerTabContent::Error { message };
        } else {
            self.tabs.push(ManagerTab {
                manager_id,
                content: ManagerTabContent::Error { message },
            });
        }
        self.invalidate_visible_rows();
        self.clamp_active_tab();
    }

    pub(super) fn mark_loading_cancelled(&mut self) {
        for tab in &mut self.tabs {
            if matches!(tab.content, ManagerTabContent::Loading { .. }) {
                tab.content = ManagerTabContent::Loading {
                    message: "Planning cancelled".to_string(),
                };
            }
        }
    }

    fn remove_manager_tab(&mut self, manager_id: &'static str) {
        self.tabs.retain(|tab| tab.manager_id != manager_id);
    }

    fn tab_mut(&mut self, manager_id: &'static str) -> Option<&mut ManagerTab> {
        self.tabs
            .iter_mut()
            .find(|tab| tab.manager_id == manager_id)
    }

    fn clamp_active_tab(&mut self) {
        if self.active_tab_idx >= self.tab_count() {
            self.active_tab_idx = self.tab_count().saturating_sub(1);
        }
    }

    fn active_tab(&self) -> ActiveTab {
        if self.active_tab_idx == 0 {
            ActiveTab::All
        } else {
            ActiveTab::Manager(self.active_tab_idx - 1)
        }
    }

    pub(super) fn tab_count(&self) -> usize {
        self.tabs.len() + 1
    }

    fn invalidate_visible_rows(&mut self) {
        self.visible_rows_dirty = true;
    }

    fn refresh_visible_rows(&mut self, plans: &[SelectionPlan]) {
        if self.visible_rows_dirty {
            self.visible_rows = self.build_visible_rows(plans);
            self.visible_rows_dirty = false;
        }
    }

    fn build_visible_rows(&self, plans: &[SelectionPlan]) -> Vec<VisibleRow> {
        let mut rows = Vec::new();
        for (manager_idx, tab) in self.tabs.iter().enumerate() {
            let manager_visible = match self.active_tab() {
                ActiveTab::All => true,
                ActiveTab::Manager(idx) => idx == manager_idx,
            };
            if !manager_visible {
                continue;
            }

            let ManagerTabContent::Items(manager) = &tab.content else {
                continue;
            };
            let Some(candidates) = manager.candidates(plans) else {
                continue;
            };

            for (candidate_idx, candidate) in candidates.iter().enumerate() {
                if candidate.is_visible_by_default()
                    || self.show_all
                    || manager.selected[candidate_idx]
                {
                    rows.push(VisibleRow {
                        manager_idx,
                        candidate_idx,
                    });
                }
            }
        }
        rows
    }

    pub(super) fn visible_row_count(&mut self, plans: &[SelectionPlan]) -> usize {
        self.refresh_visible_rows(plans);
        self.visible_rows.len()
    }

    pub(super) fn visible_row_at(
        &mut self,
        idx: usize,
        plans: &[SelectionPlan],
    ) -> Option<VisibleRow> {
        self.refresh_visible_rows(plans);
        self.visible_rows.get(idx).copied()
    }

    pub(super) fn clamp_cursor(&mut self, plans: &[SelectionPlan]) {
        let row_count = self.visible_row_count(plans);
        if row_count == 0 {
            self.cursor_idx = 0;
        } else if self.cursor_idx >= row_count {
            self.cursor_idx = row_count - 1;
        }
    }

    pub(super) fn move_cursor_up(&mut self, plans: &[SelectionPlan]) {
        let row_count = self.visible_row_count(plans);
        if row_count == 0 {
            return;
        }
        self.cursor_idx = if self.cursor_idx == 0 {
            row_count - 1
        } else {
            self.cursor_idx - 1
        };
    }

    pub(super) fn move_cursor_down(&mut self, plans: &[SelectionPlan]) {
        let row_count = self.visible_row_count(plans);
        if row_count == 0 {
            return;
        }
        self.cursor_idx = if self.cursor_idx + 1 >= row_count {
            0
        } else {
            self.cursor_idx + 1
        };
    }

    pub(super) fn next_tab(&mut self) {
        self.active_tab_idx = (self.active_tab_idx + 1) % self.tab_count();
        self.cursor_idx = 0;
        self.invalidate_visible_rows();
        self.ensure_tab_visible(usize::MAX);
    }

    pub(super) fn previous_tab(&mut self) {
        self.active_tab_idx = if self.active_tab_idx == 0 {
            self.tab_count() - 1
        } else {
            self.active_tab_idx - 1
        };
        self.cursor_idx = 0;
        self.invalidate_visible_rows();
        self.ensure_tab_visible(usize::MAX);
    }

    pub(super) fn toggle_current(&mut self, plans: &[SelectionPlan]) {
        let Some(row) = self.visible_row_at(self.cursor_idx, plans) else {
            return;
        };

        if let Some(manager) = self.manager_selection_mut(row.manager_idx) {
            let selected = &mut manager.selected[row.candidate_idx];
            *selected = !*selected;
            self.invalidate_visible_rows();
        }
    }

    pub(super) fn open_version_picker_for_current(&mut self, plans: &[SelectionPlan]) {
        let Some(row) = self.visible_row_at(self.cursor_idx, plans) else {
            return;
        };

        let Some(manager) = self.manager_selection(row.manager_idx) else {
            return;
        };

        self.version_picker = Some(VersionPickerState {
            manager: row.manager_idx,
            candidate: row.candidate_idx,
            cursor: manager.selected_version_idx[row.candidate_idx],
        });
    }

    pub(super) fn move_version_cursor_up(&mut self, plans: &[SelectionPlan]) {
        let Some(mut picker) = self.version_picker else {
            return;
        };
        let version_count = self.version_picker_version_count(picker, plans);
        if version_count == 0 {
            return;
        }

        picker.cursor = if picker.cursor == 0 {
            version_count - 1
        } else {
            picker.cursor - 1
        };
        self.version_picker = Some(picker);
    }

    pub(super) fn move_version_cursor_down(&mut self, plans: &[SelectionPlan]) {
        let Some(mut picker) = self.version_picker else {
            return;
        };
        let version_count = self.version_picker_version_count(picker, plans);
        if version_count == 0 {
            return;
        }

        picker.cursor = if picker.cursor + 1 >= version_count {
            0
        } else {
            picker.cursor + 1
        };
        self.version_picker = Some(picker);
    }

    pub(super) fn move_version_picker_to_previous_item(&mut self, plans: &[SelectionPlan]) {
        self.move_version_picker_to_item(-1, plans);
    }

    pub(super) fn move_version_picker_to_next_item(&mut self, plans: &[SelectionPlan]) {
        self.move_version_picker_to_item(1, plans);
    }

    fn move_version_picker_to_item(&mut self, delta: isize, plans: &[SelectionPlan]) {
        let Some(picker) = self.version_picker else {
            return;
        };
        self.refresh_visible_rows(plans);
        let Some(current_idx) = self.visible_rows.iter().position(|row| {
            row.manager_idx == picker.manager && row.candidate_idx == picker.candidate
        }) else {
            return;
        };
        if self.visible_rows.is_empty() {
            return;
        }

        let next_idx = match delta.cmp(&0) {
            std::cmp::Ordering::Less => {
                if current_idx == 0 {
                    self.visible_rows.len() - 1
                } else {
                    current_idx - 1
                }
            }
            std::cmp::Ordering::Equal => current_idx,
            std::cmp::Ordering::Greater => {
                if current_idx + 1 >= self.visible_rows.len() {
                    0
                } else {
                    current_idx + 1
                }
            }
        };
        let next_row = self.visible_rows[next_idx];
        let Some(manager) = self.manager_selection(next_row.manager_idx) else {
            return;
        };
        let version_cursor = manager.selected_version_idx[next_row.candidate_idx];

        self.cursor_idx = next_idx;
        self.version_picker = Some(VersionPickerState {
            manager: next_row.manager_idx,
            candidate: next_row.candidate_idx,
            cursor: version_cursor,
        });
    }

    pub(super) fn choose_recommended_version(&mut self, plans: &[SelectionPlan]) {
        let Some(mut picker) = self.version_picker else {
            return;
        };
        picker.cursor = self.default_version_idx(picker.manager, picker.candidate, plans);
        self.version_picker = Some(picker);
        self.confirm_version_picker();
    }

    pub(super) fn confirm_version_picker(&mut self) {
        let Some(picker) = self.version_picker.take() else {
            return;
        };
        if let Some(manager) = self.manager_selection_mut(picker.manager) {
            manager.selected_version_idx[picker.candidate] = picker.cursor;
            manager.selected[picker.candidate] = true;
            self.invalidate_visible_rows();
        }
    }

    pub(super) fn cancel_version_picker(&mut self) {
        self.version_picker = None;
    }

    pub(super) fn select_visible(&mut self, selected: bool, plans: &[SelectionPlan]) {
        self.refresh_visible_rows(plans);
        for idx in 0..self.visible_rows.len() {
            let row = self.visible_rows[idx];
            if let Some(manager) = self.manager_selection_mut(row.manager_idx) {
                manager.selected[row.candidate_idx] = selected;
            }
        }
        self.invalidate_visible_rows();
    }

    pub(super) fn toggle_show_all(&mut self, plans: &[SelectionPlan]) {
        self.show_all = !self.show_all;
        self.invalidate_visible_rows();
        self.clamp_cursor(plans);
    }

    pub(super) fn tick(&mut self) {
        self.marquee_tick = self.marquee_tick.wrapping_add(1);
    }

    pub(super) fn results(&self) -> Vec<SelectionResult> {
        self.tabs
            .iter()
            .filter_map(|tab| {
                let ManagerTabContent::Items(manager) = &tab.content else {
                    return None;
                };
                let chosen_versions = manager
                    .selected
                    .iter()
                    .enumerate()
                    .map(|(idx, selected)| selected.then_some(manager.selected_version_idx[idx]))
                    .collect();

                Some(SelectionResult {
                    manager_id: manager.manager_id,
                    chosen_versions,
                })
            })
            .collect()
    }

    fn version_picker_version_count(
        &self,
        picker: VersionPickerState,
        plans: &[SelectionPlan],
    ) -> usize {
        let Some(manager) = self.manager_selection(picker.manager) else {
            return 0;
        };
        let Some(candidates) = manager.candidates(plans) else {
            return 0;
        };

        candidates[picker.candidate].versions().len().max(1)
    }

    fn default_version_idx(
        &self,
        manager_idx: usize,
        candidate_idx: usize,
        plans: &[SelectionPlan],
    ) -> usize {
        let Some(manager) = self.manager_selection(manager_idx) else {
            return 0;
        };
        let Some(candidates) = manager.candidates(plans) else {
            return 0;
        };

        let candidate = &candidates[candidate_idx];
        candidate
            .versions()
            .iter()
            .position(|version| version.update().target == candidate.update().target)
            .unwrap_or(0)
    }

    pub(super) fn ensure_tab_visible(&mut self, max_width: usize) {
        if self.active_tab_idx < self.tab_offset {
            self.tab_offset = self.active_tab_idx;
            return;
        }

        if max_width == usize::MAX {
            return;
        }

        while self.tab_offset < self.active_tab_idx
            && tab_widths(self)
                .iter()
                .skip(self.tab_offset)
                .take(self.active_tab_idx - self.tab_offset + 1)
                .sum::<usize>()
                > max_width
        {
            self.tab_offset += 1;
        }
    }

    pub(super) fn manager_selection(&self, tab_idx: usize) -> Option<&ManagerSelection> {
        match self.tabs.get(tab_idx).map(|tab| &tab.content) {
            Some(ManagerTabContent::Items(manager)) => Some(manager),
            Some(ManagerTabContent::Loading { .. } | ManagerTabContent::Error { .. }) | None => {
                None
            }
        }
    }

    fn manager_selection_mut(&mut self, tab_idx: usize) -> Option<&mut ManagerSelection> {
        match self.tabs.get_mut(tab_idx).map(|tab| &mut tab.content) {
            Some(ManagerTabContent::Items(manager)) => Some(manager),
            Some(ManagerTabContent::Loading { .. } | ManagerTabContent::Error { .. }) | None => {
                None
            }
        }
    }

    pub(super) fn content_state(&mut self, plans: &[SelectionPlan]) -> SelectionContentState {
        match self.active_tab() {
            ActiveTab::All => {
                if self.visible_row_count(plans) > 0 {
                    return SelectionContentState::List;
                }

                if self
                    .tabs
                    .iter()
                    .any(|tab| matches!(tab.content, ManagerTabContent::Loading { .. }))
                {
                    return SelectionContentState::Placeholder {
                        message: "Planning updates...".to_string(),
                    };
                }

                SelectionContentState::Placeholder {
                    message: "No selectable updates".to_string(),
                }
            }
            ActiveTab::Manager(idx) => match self.tabs.get(idx).map(|tab| &tab.content) {
                Some(ManagerTabContent::Loading { message }) => {
                    SelectionContentState::Placeholder {
                        message: message.clone(),
                    }
                }
                Some(ManagerTabContent::Error { message }) => SelectionContentState::Error {
                    message: message.clone(),
                },
                Some(ManagerTabContent::Items(_)) => {
                    if self.visible_row_count(plans) == 0 {
                        SelectionContentState::Placeholder {
                            message: "No selectable updates".to_string(),
                        }
                    } else {
                        SelectionContentState::List
                    }
                }
                None => SelectionContentState::Placeholder {
                    message: "No selectable updates".to_string(),
                },
            },
        }
    }
}

pub(super) fn tab_label(app: &SelectionApp, idx: usize) -> String {
    if idx == 0 {
        return "All".to_string();
    }

    let Some(tab) = app.tabs.get(idx - 1) else {
        return String::new();
    };

    match &tab.content {
        ManagerTabContent::Loading { .. } => {
            format!(
                "{} {}",
                PLANNING_SPINNER[app.marquee_tick % PLANNING_SPINNER.len()],
                tab.manager_id
            )
        }
        ManagerTabContent::Error { .. } => format!("{} !", tab.manager_id),
        ManagerTabContent::Items(_) => tab.manager_id.to_string(),
    }
}

fn tab_widths(app: &SelectionApp) -> Vec<usize> {
    (0..app.tab_count())
        .map(|idx| UnicodeWidthStr::width(tab_label(app, idx).as_str()) + 2)
        .collect()
}

fn manager_selection(
    manager_id: &'static str,
    plan_idx: usize,
    candidates: &[ApplyCandidate],
    pinned: &BTreeSet<String>,
) -> ManagerSelection {
    let selected = candidates
        .iter()
        .map(|candidate| {
            candidate.is_selected_by_default() && !is_pinned(&candidate.update().name, pinned)
        })
        .collect();
    let selected_version_idx = default_selected_version_indices(candidates);

    ManagerSelection {
        manager_id,
        plan_idx,
        selected,
        selected_version_idx,
    }
}

fn default_selected_version_indices(candidates: &[ApplyCandidate]) -> Vec<usize> {
    candidates
        .iter()
        .map(|candidate| {
            candidate
                .versions()
                .iter()
                .position(|version| version.update().target == candidate.update().target)
                .unwrap_or(0)
        })
        .collect()
}
