use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row};
use unicode_width::UnicodeWidthStr;
use upnow_domain::{
    ManagerId, PlanIssue, PolicyBlockReason, PolicyWarning, SelectedItem, SelectedTarget,
    SkipReason, UpdateSelectionPolicy,
};
use upnow_planning::{
    CandidateNoteKind, CandidateNotePart, SelectionRow, SelectionRowStatus, SelectionRowVisibility,
    SelectionView, TargetOption,
};

use crate::outcome::version_label;
use crate::tui::components::{
    KeyBinding, TuiTable, app_block, key_footer, render_modal_frame, render_selection_table,
    render_separator, render_table, render_tabs, version_picker_columns, visible_tabs,
};
use crate::tui::layout::app_frame;
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
    PickerPreviousRow,
    PickerNextRow,
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
    tab_offset: usize,
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
    option: String,
    target: String,
    note_parts: Vec<CandidateNotePart>,
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
            tab_offset: 0,
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
    pub fn tab_offset(&self) -> usize {
        self.tab_offset
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
            | SelectionInput::PickerPreviousRow
            | SelectionInput::PickerNextRow
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
        self.cursor = if self.cursor == 0 {
            row_count - 1
        } else {
            self.cursor - 1
        };
    }

    fn move_cursor_down(&mut self) {
        let row_count = self.visible_row_refs().len();
        if row_count == 0 {
            return;
        }
        self.cursor = if self.cursor + 1 >= row_count {
            0
        } else {
            self.cursor + 1
        };
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
                cursor: self.target_picker_initial_cursor(visible_row),
            });
        }
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
                self.cursor = next_idx;
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
            SelectionInput::Cancel if key.code == KeyCode::Esc => SelectionInput::PickerCancel,
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

    if let Some(message) = screen.placeholder_message() {
        draw_centered_placeholder(frame, app_frame.body, &message, theme.muted);
    } else {
        draw_list_content(frame, screen, app_frame.body, theme);
    }

    render_separator(frame, app_frame.footer_separator, theme);
    frame.render_widget(Paragraph::new(footer_line(screen, theme)), app_frame.footer);

    if let Some(picker) = screen.target_picker {
        draw_target_picker(frame, screen, picker, app_frame.outer, theme);
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
    let titles = std::iter::once(Line::raw("All"))
        .chain(
            screen
                .managers
                .iter()
                .map(|manager| Line::raw(manager.manager_id.as_str().to_owned())),
        )
        .collect::<Vec<_>>();

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
    let render_rows = selection_render_rows(screen);
    let table_rows = render_rows
        .iter()
        .enumerate()
        .map(|(idx, row)| selection_table_row(row, idx == screen.cursor, theme))
        .collect::<Vec<_>>();

    let selected = (screen.cursor < render_rows.len()).then_some(screen.cursor);
    render_selection_table(frame, area, table_rows, selected, theme);
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
            let selected = manager.state.selected_target(&row.plan_item_id).is_some();
            let target = row
                .target_version
                .as_ref()
                .map_or_else(|| "-".to_owned(), |version| version_label(version.as_str()));
            let forced = row
                .target_options
                .iter()
                .any(|option| matches!(option, TargetOption::ForcedCandidate { .. }));

            SelectionRenderRow {
                selected,
                manager: manager.manager_id.as_str().to_owned(),
                name: row.package_name.as_str().to_owned(),
                current: version_label(row.installed_version.as_str()),
                target,
                note_parts: row.notes.clone(),
                forced,
            }
        })
        .collect()
}

fn selection_table_row(
    row: &SelectionRenderRow,
    highlighted: bool,
    theme: &TuiTheme,
) -> Row<'static> {
    let style = theme.row_for_selectable_state(highlighted, row.forced && !row.selected);
    let marker = if row.selected { "[x]" } else { "[ ]" };
    let target = if row.target == "-" {
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
        Cell::new(note_line(&row.note_parts, theme.note_for(style), theme))
            .style(theme.note_for(style))
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
    let mut spans = vec![Span::styled("forced", theme.forced_note_for(highlighted))];
    let note = note_text(note_parts);
    if !note.is_empty() {
        spans.push(Span::styled(", ", theme.note_for(base_style)));
        spans.push(Span::styled(note, theme.note_for(base_style)));
    }

    Cell::new(Line::from(spans)).style(theme.note_for(base_style))
}

fn footer_line(screen: &InteractiveSelectionScreen, theme: &TuiTheme) -> Line<'static> {
    if screen.target_picker.is_some() {
        return picker_footer_line(theme);
    }

    key_footer(FOOTER_KEYS, theme)
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

    if inner.height < 5 || inner.width < 20 {
        frame.render_widget(Paragraph::new("Terminal too small"), inner);
        return;
    }

    let [title_area, _, current_area, _, list_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
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
            Span::styled("Current: ", theme.header),
            Span::raw(version_label(row.installed_version.as_str())),
        ])),
        current_area,
    );

    draw_target_picker_rows(frame, screen, picker, list_area, theme);
    frame.render_widget(
        Paragraph::new(key_footer(PICKER_FOOTER_KEYS, theme)),
        footer_area,
    );
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
        TuiTable::new(table_rows, version_picker_columns())
            .selected(selected)
            .row_highlight_style(theme.selected),
        theme,
    );
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
    let target = version_diff_spans(current, &row.target, style, theme, highlighted);
    let mut target_spans = vec![
        Span::styled(row.option.clone(), theme.emphasis(style)),
        Span::styled(" ", style),
    ];
    target_spans.extend(target);
    let note = note_line(&row.note_parts, theme.note_for(style), theme);

    Row::new(vec![
        Cell::new(marker).style(style),
        Cell::new(Line::from(target_spans)).style(style),
        Cell::new(note).style(theme.note_for(style)),
    ])
    .style(style)
}

fn target_picker_rows(options: &[TargetOption]) -> Vec<TargetPickerRenderRow> {
    options
        .iter()
        .map(|option| TargetPickerRenderRow {
            option: target_option_kind_label(option).to_owned(),
            target: version_label(option.target_version().as_str()),
            note_parts: option.note_parts().to_vec(),
        })
        .collect()
}

fn target_option_kind_label(option: &TargetOption) -> &'static str {
    match option {
        TargetOption::Recommended { .. } => "recommended",
        TargetOption::ForcedCandidate { .. } => "force",
        TargetOption::AlternateExact { .. } => "exact",
    }
}

fn target_picker_height(option_count: usize) -> u16 {
    let body = u16::try_from(option_count.min(10)).unwrap_or(10);
    body.saturating_add(8).clamp(9, 18)
}

fn target_picker_width(area: Rect) -> u16 {
    area.width.saturating_sub(4).clamp(62, 96)
}

fn target_option_matches_selected(option: &TargetOption, target: &SelectedTarget) -> bool {
    match (option, target) {
        (TargetOption::Recommended { .. }, SelectedTarget::Recommended)
        | (TargetOption::ForcedCandidate { .. }, SelectedTarget::ForcedCandidate) => true,
        (
            TargetOption::AlternateExact { target_version, .. },
            SelectedTarget::AlternateExact {
                target_version: selected,
            },
        ) => target_version == selected,
        _ => false,
    }
}

fn note_line(note_parts: &[CandidateNotePart], style: Style, theme: &TuiTheme) -> Line<'static> {
    let mut spans = Vec::new();
    for (idx, part) in note_parts.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled("; ", style));
        }
        let part_style = if part.violation {
            style.patch(theme.forced)
        } else {
            style
        };
        spans.push(Span::styled(note_part_text(part), part_style));
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

fn note_part_text(part: &CandidateNotePart) -> String {
    match &part.kind {
        CandidateNoteKind::Released { age } => format!("released {}", human_age(*age)),
        CandidateNoteKind::TooFresh { age, required_age } => match age {
            Some(age) => format!(
                "too fresh: {} old, need {}",
                human_age(*age),
                human_age(*required_age)
            ),
            None => format!("too fresh: need {}", human_age(*required_age)),
        },
        CandidateNoteKind::VersionPolicyBlocked(reason) => policy_block_reason_text(reason),
        CandidateNoteKind::PolicyWarning(warning) => policy_warning_text(*warning).to_owned(),
        CandidateNoteKind::MissingReleaseMetadata => "missing release metadata".to_owned(),
        CandidateNoteKind::ReleaseLookupFailed { error } => error.as_ref().map_or_else(
            || "release lookup failed".to_owned(),
            |error| format!("release lookup failed: {}", error.detail),
        ),
        CandidateNoteKind::Skipped(reason) => skip_reason_text(reason),
        CandidateNoteKind::ResolverError { message } => message.clone(),
    }
}

fn policy_block_reason_text(reason: &PolicyBlockReason) -> String {
    match reason {
        PolicyBlockReason::PreReleaseBlocked => "pre-release blocked by policy".to_owned(),
        PolicyBlockReason::TrackRegression => "track regression blocked by policy".to_owned(),
        PolicyBlockReason::UnknownStability => "unknown stability blocked by policy".to_owned(),
    }
}

fn policy_warning_text(warning: PolicyWarning) -> &'static str {
    match warning {
        PolicyWarning::InstalledTrackUnknownFallbackStable => {
            "same-track fell back to stable because installed track is unknown"
        }
    }
}

fn skip_reason_text(reason: &SkipReason) -> String {
    match reason {
        SkipReason::Pinned => "pinned".to_owned(),
        SkipReason::ManagerRule(detail) => detail.clone(),
    }
}

fn human_age(age: Duration) -> String {
    let seconds = age.as_secs();
    let days = seconds / (24 * 60 * 60);
    if days > 0 {
        return format!("{days}d");
    }
    let hours = seconds / (60 * 60);
    if hours > 0 {
        return format!("{hours}h");
    }
    let minutes = seconds / 60;
    if minutes > 0 {
        return format!("{minutes}m");
    }
    format!("{seconds}s")
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

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use upnow_domain::{
        DelayReason, ExecutionEligibility, ManagerId, PackageName, PlanItem, PlanItemId, ToolId,
        UpdateCandidate, UpdatePlan, VersionScheme, VersionText,
    };
    use upnow_planning::selection_view;

    use super::*;

    #[test]
    fn renders_old_style_selection_shell_from_typed_rows() {
        let plan = plan(
            "pnpm",
            vec![update(
                "pnpm:alpha",
                "alpha",
                ExecutionEligibility::NativeOrExact,
            )],
        );
        let mut screen = screen_from_plan(&plan);

        let rendered = render_to_string(&mut screen, 96, 18);

        assert!(rendered.contains("All"));
        assert!(rendered.contains("pnpm"));
        assert!(rendered.contains("Manager"));
        assert!(rendered.contains("Name"));
        assert!(rendered.contains("Current"));
        assert!(rendered.contains("Target"));
        assert!(rendered.contains("Note"));
        assert!(rendered.contains("[x]"));
        assert!(rendered.contains("alpha"));
        assert!(rendered.contains("v1.0.0"));
        assert!(rendered.contains("v1.2.0"));
        assert!(rendered.contains("space x"));
        assert!(rendered.contains("confirm"));
        assert!(rendered.contains("─"));
        assert!(rendered.contains("⇥"));
    }

    #[test]
    fn renders_forced_note_from_typed_note_parts() {
        let plan = plan(
            "pnpm",
            vec![delayed(
                "pnpm:alpha",
                "alpha",
                ExecutionEligibility::ExactOnly,
            )],
        );
        let policy = UpdateSelectionPolicy::default();
        let mut view = selection_view(&plan, &policy);
        view.rows[0].notes = vec![CandidateNotePart::violation(
            CandidateNoteKind::MissingReleaseMetadata,
        )];
        let mut screen = InteractiveSelectionScreen::new(vec![InteractiveSelectionPlan::new(
            view,
            Vec::new(),
            policy,
        )]);

        let rendered = render_to_string(&mut screen, 140, 18);

        assert!(rendered.contains("[ ]"));
        assert!(rendered.contains("forced"), "{rendered}");
        assert!(rendered.contains("missing"), "{rendered}");
    }

    #[test]
    fn renders_small_terminal_placeholder() {
        let plan = plan(
            "pnpm",
            vec![update(
                "pnpm:alpha",
                "alpha",
                ExecutionEligibility::NativeOrExact,
            )],
        );
        let mut screen = screen_from_plan(&plan);

        let rendered = render_to_string(&mut screen, 30, 4);

        assert!(rendered.contains("Terminal too small"));
    }

    #[test]
    fn picker_modal_renders_typed_options() {
        let plan = plan(
            "pnpm",
            vec![update(
                "pnpm:alpha",
                "alpha",
                ExecutionEligibility::ExactOnly,
            )],
        );
        let mut screen = screen_from_plan(&plan);

        screen
            .handle_input(SelectionInput::OpenTargetPicker)
            .expect("picker should open");
        let rendered = render_to_string(&mut screen, 140, 18);

        assert!(rendered.contains("pnpm: alpha"));
        assert!(rendered.contains("Current:"));
        assert!(rendered.contains("[x]"));
        assert!(rendered.contains("[ ]"));
        assert!(rendered.contains("recommended"));
        assert!(rendered.contains("exact"));
        assert!(rendered.contains("v1.2.0"));
        assert!(rendered.contains("shift+up/down J/K"));
        assert!(rendered.contains("esc"));
    }

    #[test]
    fn picker_open_event_mapping_keeps_q_local_to_picker() {
        let confirm = selection_input_from_event(
            &Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('C'),
                KeyModifiers::NONE,
            )),
            true,
        );
        let quit = selection_input_from_event(
            &Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
            )),
            true,
        );
        let escape = selection_input_from_event(
            &Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )),
            true,
        );

        assert_eq!(confirm, SelectionInput::Confirm);
        assert_eq!(quit, SelectionInput::Ignore);
        assert_eq!(escape, SelectionInput::PickerCancel);
    }

    fn render_to_string(
        screen: &mut InteractiveSelectionScreen,
        width: u16,
        height: u16,
    ) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| draw_selection_with_theme(frame, screen, &TuiTheme::plain()))
            .expect("draw should succeed");
        buffer_to_string(terminal.backend().buffer())
    }

    fn buffer_to_string(buffer: &Buffer) -> String {
        let area = buffer.area;
        let mut output = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        output
    }

    fn screen_from_plan(plan: &UpdatePlan) -> InteractiveSelectionScreen {
        let policy = UpdateSelectionPolicy::default();
        let view = selection_view(plan, &policy);
        InteractiveSelectionScreen::new(vec![InteractiveSelectionPlan::new(
            view,
            plan.issues.clone(),
            policy,
        )])
    }

    fn plan(manager: &str, items: Vec<PlanItem>) -> UpdatePlan {
        UpdatePlan::new(ManagerId::new(manager).expect("valid manager"), items).expect("valid plan")
    }

    fn update(id: &str, name: &str, execution_eligibility: ExecutionEligibility) -> PlanItem {
        PlanItem::Update {
            id: plan_item_id(id),
            candidate: candidate(id, name, execution_eligibility),
        }
    }

    fn delayed(id: &str, name: &str, execution_eligibility: ExecutionEligibility) -> PlanItem {
        PlanItem::Delayed {
            id: plan_item_id(id),
            candidate: candidate(id, name, execution_eligibility),
            reason: DelayReason::ReleaseTooFresh,
        }
    }

    fn candidate(
        id: &str,
        name: &str,
        execution_eligibility: ExecutionEligibility,
    ) -> UpdateCandidate {
        UpdateCandidate::new(
            ToolId::new(id).expect("valid tool"),
            package(name),
            VersionText::new("1.0.0").expect("valid installed version"),
            VersionText::new("1.2.0").expect("valid target version"),
            VersionScheme::SemVer,
            execution_eligibility,
        )
    }

    fn package(name: &str) -> PackageName {
        PackageName::new(name).expect("valid package")
    }

    fn plan_item_id(id: &str) -> PlanItemId {
        PlanItemId::new(id).expect("valid plan item id")
    }
}
