use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs};
use upnow_domain::{ManagerId, PlanIssue, SelectedItem, UpdateSelectionPolicy};
use upnow_planning::{
    SelectionRow, SelectionRowStatus, SelectionRowVisibility, SelectionView, TargetOption,
};

use crate::tui::{InteractiveSelectionState, SelectionStateError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveSelectionPlan {
    pub view: SelectionView,
    pub issues: Vec<PlanIssue>,
    pub selection_policy: UpdateSelectionPolicy,
}

impl InteractiveSelectionPlan {
    #[must_use]
    pub const fn new(
        view: SelectionView,
        issues: Vec<PlanIssue>,
        selection_policy: UpdateSelectionPolicy,
    ) -> Self {
        Self {
            view,
            issues,
            selection_policy,
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
    PickerConfirm,
    PickerCancel,
    RecommendedTarget,
    Confirm,
    Cancel,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionControl {
    Continue,
    Confirm,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveSelectionScreen {
    managers: Vec<ManagerSelectionState>,
    active_tab: usize,
    cursor: usize,
    show_all: bool,
    target_picker: Option<TargetPickerState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagerSelectionState {
    manager_id: ManagerId,
    issues: Vec<PlanIssue>,
    state: InteractiveSelectionState,
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

impl InteractiveSelectionScreen {
    #[must_use]
    pub fn new(plans: Vec<InteractiveSelectionPlan>) -> Self {
        let managers = plans
            .into_iter()
            .map(|plan| {
                let manager_id = plan.view.manager_id.clone();
                let state =
                    InteractiveSelectionState::new(plan.view, plan.selection_policy.clone());
                ManagerSelectionState {
                    manager_id,
                    issues: plan.issues,
                    state,
                }
            })
            .collect();

        let mut screen = Self {
            managers,
            active_tab: 0,
            cursor: 0,
            show_all: false,
            target_picker: None,
        };
        screen.clamp_cursor();
        screen
    }

    #[must_use]
    pub fn active_tab(&self) -> usize {
        self.active_tab
    }

    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    #[must_use]
    pub fn show_all(&self) -> bool {
        self.show_all
    }

    #[must_use]
    pub fn target_picker_open(&self) -> bool {
        self.target_picker.is_some()
    }

    #[must_use]
    pub fn target_picker_options(&self) -> Vec<TargetOption> {
        let Some(picker) = self.target_picker else {
            return Vec::new();
        };
        let row = self.row(picker.visible_row);
        row.target_options.clone()
    }

    #[must_use]
    pub fn visible_rows(&self) -> Vec<&SelectionRow> {
        self.visible_row_refs()
            .into_iter()
            .map(|visible| self.row(visible))
            .collect()
    }

    #[must_use]
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

    #[must_use]
    pub fn placeholder_message(&self) -> Option<String> {
        if !self.visible_row_refs().is_empty() {
            return None;
        }
        for manager in &self.managers {
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
            }
            SelectionInput::OpenTargetPicker => self.open_target_picker(),
            SelectionInput::Confirm => return Ok(SelectionControl::Confirm),
            SelectionInput::Cancel => return Ok(SelectionControl::Cancel),
            SelectionInput::Ignore
            | SelectionInput::PickerUp
            | SelectionInput::PickerDown
            | SelectionInput::PickerConfirm
            | SelectionInput::PickerCancel
            | SelectionInput::RecommendedTarget => {}
        }

        Ok(SelectionControl::Continue)
    }

    #[must_use]
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

    fn handle_picker_input(
        &mut self,
        input: SelectionInput,
    ) -> Result<SelectionControl, SelectionStateError> {
        match input {
            SelectionInput::PickerCancel => self.target_picker = None,
            SelectionInput::Cancel => return Ok(SelectionControl::Cancel),
            SelectionInput::PickerUp => self.move_picker_up(),
            SelectionInput::PickerDown => self.move_picker_down(),
            SelectionInput::RecommendedTarget => self.choose_recommended_target()?,
            SelectionInput::PickerConfirm => self.confirm_picker_target()?,
            SelectionInput::Confirm => return Ok(SelectionControl::Confirm),
            _ => {}
        }
        Ok(SelectionControl::Continue)
    }

    fn move_cursor_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_cursor_down(&mut self) {
        let row_count = self.visible_row_refs().len();
        if self.cursor + 1 < row_count {
            self.cursor += 1;
        }
    }

    fn next_tab(&mut self) {
        let tab_count = self.managers.len() + 1;
        if tab_count > 0 {
            self.active_tab = (self.active_tab + 1) % tab_count;
        }
        self.cursor = 0;
        self.clamp_cursor();
    }

    fn previous_tab(&mut self) {
        let tab_count = self.managers.len() + 1;
        if tab_count > 0 {
            self.active_tab = if self.active_tab == 0 {
                tab_count - 1
            } else {
                self.active_tab - 1
            };
        }
        self.cursor = 0;
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
            if row.status != SelectionRowStatus::Update {
                continue;
            }
            let manager = &mut self.managers[visible.manager_idx];
            if selected {
                manager.state.select_recommended(&row.plan_item_id)?;
            } else {
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
                cursor: 0,
            });
        }
    }

    fn move_picker_up(&mut self) {
        if let Some(picker) = &mut self.target_picker {
            picker.cursor = picker.cursor.saturating_sub(1);
        }
    }

    fn move_picker_down(&mut self) {
        let Some(picker) = self.target_picker else {
            return;
        };
        let option_count = self.target_option_count(picker.visible_row);
        if picker.cursor + 1 < option_count {
            self.target_picker = Some(TargetPickerState {
                cursor: picker.cursor + 1,
                ..picker
            });
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
            }
        }

        self.target_picker = None;
        Ok(())
    }

    fn target_option_count(&self, visible: VisibleRow) -> usize {
        let row = self.row(visible);
        row.target_options.len()
    }

    fn clamp_cursor(&mut self) {
        let row_count = self.visible_row_refs().len();
        if row_count == 0 {
            self.cursor = 0;
        } else if self.cursor >= row_count {
            self.cursor = row_count - 1;
        }
    }

    fn current_visible_row(&self) -> Option<VisibleRow> {
        self.visible_row_refs().get(self.cursor).copied()
    }

    fn visible_row_refs(&self) -> Vec<VisibleRow> {
        let mut rows = Vec::new();
        for (manager_idx, manager) in self.managers.iter().enumerate() {
            if self.active_tab != 0 && self.active_tab != manager_idx + 1 {
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

    fn row(&self, visible: VisibleRow) -> &SelectionRow {
        &self.managers[visible.manager_idx].state.rows()[visible.row_idx]
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
    let mut screen = InteractiveSelectionScreen::new(plans);

    let result = run_selection_loop(&mut terminal, &mut screen);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_selection_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    screen: &mut InteractiveSelectionScreen,
) -> io::Result<InteractiveSelectionOutcome> {
    loop {
        terminal.draw(|frame| draw_selection(frame, screen))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let input = selection_input_from_event(&event::read()?, screen.target_picker_open());
        match screen
            .handle_input(input)
            .map_err(|err| io::Error::other(err.to_string()))?
        {
            SelectionControl::Continue => {}
            SelectionControl::Confirm => {
                return Ok(InteractiveSelectionOutcome::Confirmed(
                    screen.selection_drafts(),
                ));
            }
            SelectionControl::Cancel => return Ok(InteractiveSelectionOutcome::Cancelled),
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
        return SelectionInput::Cancel;
    }

    let input = match key.code {
        KeyCode::Char('q') | KeyCode::Esc => SelectionInput::Cancel,
        KeyCode::Char('C') => SelectionInput::Confirm,
        KeyCode::Up | KeyCode::Char('k') => SelectionInput::Up,
        KeyCode::Down | KeyCode::Char('j') => SelectionInput::Down,
        KeyCode::Tab => SelectionInput::NextTab,
        KeyCode::BackTab => SelectionInput::PreviousTab,
        KeyCode::Char(' ' | 'x') => SelectionInput::ToggleCurrent,
        KeyCode::Char('a') => SelectionInput::SelectVisible,
        KeyCode::Char('n') => SelectionInput::SelectNoneVisible,
        KeyCode::Char('v') => SelectionInput::ToggleViewAll,
        KeyCode::Char('r') => SelectionInput::RecommendedTarget,
        KeyCode::Enter => SelectionInput::OpenTargetPicker,
        _ => SelectionInput::Ignore,
    };

    if target_picker_open {
        match input {
            SelectionInput::Up => SelectionInput::PickerUp,
            SelectionInput::Down => SelectionInput::PickerDown,
            SelectionInput::OpenTargetPicker => SelectionInput::PickerConfirm,
            SelectionInput::Cancel if key.code != KeyCode::Char('c') => {
                SelectionInput::PickerCancel
            }
            _ => input,
        }
    } else {
        input
    }
}

fn draw_selection(frame: &mut ratatui::Frame<'_>, screen: &InteractiveSelectionScreen) {
    let area = frame.area();
    let [tabs_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(area);

    let titles = std::iter::once(Line::from("All"))
        .chain(
            screen
                .managers
                .iter()
                .map(|manager| Line::from(manager.manager_id.as_str().to_owned())),
        )
        .collect::<Vec<_>>();
    let tabs = Tabs::new(titles)
        .select(screen.active_tab)
        .block(Block::new().borders(Borders::ALL).title("upnow apply"));
    frame.render_widget(tabs, tabs_area);

    if let Some(message) = screen.placeholder_message() {
        frame.render_widget(
            Paragraph::new(message).block(Block::new().borders(Borders::ALL)),
            body_area,
        );
    } else {
        let rows = screen
            .visible_row_refs()
            .into_iter()
            .map(|visible| {
                let manager = &screen.managers[visible.manager_idx];
                let row = screen.row(visible);
                let selected = manager.state.selected_target(&row.plan_item_id).is_some();
                let target = row
                    .target_version
                    .as_ref()
                    .map_or("-", upnow_domain::VersionText::as_str);
                Row::new([
                    Cell::from(if selected { "x" } else { " " }),
                    Cell::from(manager.manager_id.as_str().to_owned()),
                    Cell::from(row.package_name.as_str().to_owned()),
                    Cell::from(row.installed_version.as_str().to_owned()),
                    Cell::from(target.to_owned()),
                    Cell::from(status_label(row)),
                ])
            })
            .collect::<Vec<_>>();

        let table = Table::new(
            rows,
            [
                Constraint::Length(3),
                Constraint::Length(10),
                Constraint::Percentage(30),
                Constraint::Length(14),
                Constraint::Length(14),
                Constraint::Length(12),
            ],
        )
        .header(Row::new([
            "", "manager", "package", "current", "target", "status",
        ]))
        .block(Block::new().borders(Borders::ALL))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol(">");

        let mut table_state = ratatui::widgets::TableState::default();
        if !screen.visible_row_refs().is_empty() {
            table_state.select(Some(screen.cursor));
        }
        frame.render_stateful_widget(table, body_area, &mut table_state);
    }

    let footer = if screen.target_picker.is_some() {
        let options = screen
            .target_picker_options()
            .iter()
            .map(target_option_label)
            .collect::<Vec<_>>()
            .join(" | ");
        format!(
            "target: {options}\nup/down choose target  enter confirm  r recommended  esc cancel"
        )
    } else if screen.has_selectable_rows() {
        "up/down move  tab manager  space toggle  a all  n none  v view all  enter target  C confirm  q quit".to_owned()
    } else {
        "v view all  C confirm  q quit".to_owned()
    };
    frame.render_widget(Paragraph::new(footer), footer_area);
}

fn plan_issue_label(issue: &PlanIssue) -> String {
    match issue {
        PlanIssue::DiscoveryFailed { detail } => detail.clone(),
        PlanIssue::UnsupportedManagerVersion {
            installed_version,
            reason,
        } => format!(
            "unsupported manager version {} {reason:?}",
            installed_version.as_str()
        ),
    }
}

fn target_option_label(option: &TargetOption) -> String {
    match option {
        TargetOption::Recommended { .. } => "recommended".to_owned(),
        TargetOption::ForcedCandidate { .. } => "force candidate".to_owned(),
        TargetOption::AlternateExact { target_version, .. } => {
            format!("exact {}", target_version.as_str())
        }
    }
}

fn status_label(row: &SelectionRow) -> String {
    let suffix = if row
        .target_options
        .iter()
        .any(|option| matches!(option, TargetOption::ForcedCandidate { .. }))
    {
        " force"
    } else {
        ""
    };
    match row.status {
        SelectionRowStatus::Update => "update".to_owned(),
        SelectionRowStatus::Current => "current".to_owned(),
        SelectionRowStatus::Delayed => format!("delayed{suffix}"),
        SelectionRowStatus::Blocked => "blocked".to_owned(),
        SelectionRowStatus::Skipped => "skipped".to_owned(),
        SelectionRowStatus::ResolverError => "error".to_owned(),
    }
}
