use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::io::{self, IsTerminal, Write};

use anyhow::{Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::Stylize;
use crossterm::terminal::{self, BeginSynchronizedUpdate, ClearType, EndSynchronizedUpdate};
use crossterm::{cursor, execute, queue};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::config::is_pinned;
use crate::managers::{ApplyCandidate, PlannedUpdate};
use crate::outcome::{render_to_version, version_label};
use crate::ui::{OutputTheme, output_theme, with_spinner_suspended};
use crate::util::text::strip_v_prefix;

struct Line {
    plain: String,
    styled: String,
}

const DIALOG_TITLE_PREFIX: &str = "Select updates for";
const PINNED_LABEL: &str = "(pinned)";
const FORCED_LABEL: &str = "forced";
const DIALOG_BOX_OVERHEAD: usize = 4;
const INTERACTIVE_TABLE_GAP: &str = "  ";
const ELLIPSIS: char = '…';

const MULTI_SELECT_KEYBINDS: &[(&str, &str)] = &[
    ("↑/↓/j/k", "move"),
    ("space/x", "toggle"),
    ("a", "all"),
    ("n", "none"),
    ("enter", "confirm"),
];

const ADVANCED_KEYBINDS: &[(&str, &str)] = &[("v", "tree"), ("←/h", "collapse"), ("→/l", "expand")];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DialogView {
    List,
    Tree,
}

struct MultiSelectState {
    selected: Vec<bool>,
    selected_version_idx: Vec<usize>,
    expanded: Vec<bool>,
    cursor_idx: usize,
    view: DialogView,
}

struct DialogStyle<'a> {
    title: &'a Line,
    footer: &'a Line,
    desired_inner_width: usize,
    color: bool,
    table_widths: InteractiveTableWidths,
}

#[derive(Debug, Clone, Copy)]
struct InteractiveTableWidths {
    prefix: usize,
    tree: usize,
    name: usize,
    current: usize,
    target: usize,
    status: usize,
    note: usize,
}

#[derive(Debug, Clone, Copy)]
struct RenderedCell<'a> {
    text: &'a str,
    width: usize,
}

impl<'a> RenderedCell<'a> {
    fn plain(text: &'a str) -> Self {
        Self {
            text,
            width: width(text),
        }
    }

    const fn styled(text: &'a str, width: usize) -> Self {
        Self { text, width }
    }
}

#[derive(Clone, Copy)]
enum SelectionAction {
    Toggle,
    SelectAll,
    SelectNone,
    ToggleView,
    Expand,
    Collapse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisibleRow {
    Parent(usize),
    Version {
        parent_idx: usize,
        version_idx: usize,
    },
}

#[derive(Debug)]
pub struct InteractiveCancelled;

impl fmt::Display for InteractiveCancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("interactive selection cancelled")
    }
}

impl Error for InteractiveCancelled {}

pub fn ensure_tty_available() -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!("--interactive requires an interactive terminal on stdin");
    }

    if !std::io::stdout().is_terminal() {
        bail!("--interactive requires an interactive terminal on stdout");
    }

    Ok(())
}

pub fn choose_apply_candidates_for_manager(
    manager: &str,
    candidates: &[ApplyCandidate],
    pinned: &BTreeSet<String>,
) -> Result<Vec<Option<PlannedUpdate>>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    with_spinner_suspended(|| run_multi_select_dialog(manager, candidates, pinned))
}

fn run_multi_select_dialog(
    manager: &str,
    candidates: &[ApplyCandidate],
    pinned: &BTreeSet<String>,
) -> Result<Vec<Option<PlannedUpdate>>> {
    let theme = output_theme();
    let color = theme.color();
    let title = title_line(manager, theme);
    let has_advanced = has_advanced_view(candidates);
    let footer = footer_line(color, has_advanced);
    let table_widths = interactive_table_widths(candidates);
    let desired_inner_width =
        multi_select_desired_inner_width(&title.plain, &footer.plain, table_widths);

    with_dialog_terminal(|out, last_height| {
        let mut state = MultiSelectState {
            selected: candidates
                .iter()
                .map(|candidate| {
                    candidate.is_selected_by_default()
                        && !is_pinned(&candidate.update().name, pinned)
                })
                .collect(),
            selected_version_idx: default_selected_version_indices(candidates),
            expanded: vec![false; candidates.len()],
            cursor_idx: 0,
            view: DialogView::List,
        };

        let style = DialogStyle {
            title: &title,
            footer: &footer,
            desired_inner_width,
            color,
            table_widths,
        };

        run_dialog_loop(out, last_height, &mut state, candidates, &style)
    })
}

fn with_dialog_terminal<T, F>(dialog: F) -> Result<T>
where
    F: FnOnce(&mut io::Stdout, &mut usize) -> Result<T>,
{
    let mut out = io::stdout();
    terminal::enable_raw_mode()?;
    if let Err(err) = execute!(out, cursor::Hide) {
        let _ = terminal::disable_raw_mode();
        return Err(err.into());
    }

    let mut last_height = 0usize;
    let dialog_result = dialog(&mut out, &mut last_height);
    let cleanup_result = cleanup_terminal(&mut out, last_height);

    match (dialog_result, cleanup_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(dialog_err), Ok(())) => Err(dialog_err),
        (Ok(_), Err(cleanup_err)) => {
            Err(cleanup_err.context("failed to cleanup interactive terminal"))
        }
        (Err(dialog_err), Err(cleanup_err)) => Err(dialog_err.context(format!(
            "interactive terminal cleanup failed: {cleanup_err:#}"
        ))),
    }
}

fn run_dialog_loop(
    out: &mut io::Stdout,
    last_height: &mut usize,
    state: &mut MultiSelectState,
    candidates: &[ApplyCandidate],
    style: &DialogStyle<'_>,
) -> Result<Vec<Option<PlannedUpdate>>> {
    loop {
        let visible_rows = visible_rows(candidates, state);
        clamp_cursor(state, visible_rows.len());
        let mut body = Vec::with_capacity(visible_rows.len());

        for (visible_idx, row) in visible_rows.iter().copied().enumerate() {
            let pointer = if visible_idx == state.cursor_idx {
                ">"
            } else {
                " "
            };
            let line = row_line(
                candidates,
                state,
                row,
                pointer,
                visible_idx == state.cursor_idx,
                style,
            );

            body.push(line);
        }

        *last_height = draw_dialog_box(
            out,
            style.title,
            &body,
            style.footer,
            style.desired_inner_width,
            *last_height,
        )?;

        let Some(key_code) = read_dialog_key_code()? else {
            continue;
        };

        match key_code {
            KeyCode::Up | KeyCode::Char('k') => {
                if visible_rows.is_empty() {
                    continue;
                }
                state.cursor_idx = if state.cursor_idx == 0 {
                    visible_rows.len() - 1
                } else {
                    state.cursor_idx - 1
                };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if visible_rows.is_empty() {
                    continue;
                }
                state.cursor_idx = if state.cursor_idx + 1 >= visible_rows.len() {
                    0
                } else {
                    state.cursor_idx + 1
                };
            }
            KeyCode::Enter => {
                return Ok(selected_updates(candidates, state));
            }
            _ => {
                if let Some(action) = selection_action_for_key(key_code) {
                    apply_selection_action_to_list(state, candidates, &visible_rows, action);
                }
            }
        }
    }
}

fn visible_rows(candidates: &[ApplyCandidate], state: &MultiSelectState) -> Vec<VisibleRow> {
    let mut rows = Vec::new();
    for (idx, candidate) in candidates.iter().enumerate() {
        let visible = candidate.is_visible_by_default()
            || state.view == DialogView::Tree
            || state.selected[idx];
        if !visible {
            continue;
        }

        rows.push(VisibleRow::Parent(idx));

        if state.view == DialogView::Tree
            && state.expanded[idx]
            && candidate.has_selectable_versions()
        {
            rows.extend(
                candidate
                    .versions()
                    .iter()
                    .enumerate()
                    .map(|(version_idx, _)| VisibleRow::Version {
                        parent_idx: idx,
                        version_idx,
                    }),
            );
        }
    }

    rows
}

fn clamp_cursor(state: &mut MultiSelectState, visible_len: usize) {
    if visible_len == 0 {
        state.cursor_idx = 0;
    } else if state.cursor_idx >= visible_len {
        state.cursor_idx = visible_len - 1;
    }
}

fn read_dialog_key_code() -> Result<Option<KeyCode>> {
    let event = event::read()?;
    if matches!(event, Event::Resize(_, _)) {
        return Ok(None);
    }

    let Event::Key(key) = event else {
        return Ok(None);
    };

    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return Ok(None);
    }

    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Err(anyhow::Error::new(InteractiveCancelled));
    }

    Ok(Some(key.code))
}

const fn selection_action_for_key(code: KeyCode) -> Option<SelectionAction> {
    match code {
        KeyCode::Char(' ' | 'x') => Some(SelectionAction::Toggle),
        KeyCode::Char('a') => Some(SelectionAction::SelectAll),
        KeyCode::Char('n') => Some(SelectionAction::SelectNone),
        KeyCode::Char('v') => Some(SelectionAction::ToggleView),
        KeyCode::Right | KeyCode::Char('l') => Some(SelectionAction::Expand),
        KeyCode::Left | KeyCode::Char('h') => Some(SelectionAction::Collapse),
        _ => None,
    }
}

fn apply_selection_action_to_list(
    state: &mut MultiSelectState,
    candidates: &[ApplyCandidate],
    rows: &[VisibleRow],
    action: SelectionAction,
) {
    match action {
        SelectionAction::Toggle => {
            toggle_current_row(state, candidates, rows);
        }
        SelectionAction::SelectAll => {
            for row in rows {
                if let VisibleRow::Parent(idx) = *row {
                    state.selected[idx] = true;
                }
            }
        }
        SelectionAction::SelectNone => {
            for row in rows {
                if let VisibleRow::Parent(idx) = *row {
                    state.selected[idx] = false;
                }
            }
        }
        SelectionAction::ToggleView => {
            if has_advanced_view(candidates) {
                state.view = match state.view {
                    DialogView::List => DialogView::Tree,
                    DialogView::Tree => DialogView::List,
                };
                let visible_len = visible_rows(candidates, state).len();
                clamp_cursor(state, visible_len);
            }
        }
        SelectionAction::Expand => {
            if let Some(parent_idx) = current_parent_index(rows, state.cursor_idx)
                && candidates[parent_idx].has_selectable_versions()
            {
                state.expanded[parent_idx] = true;
            }
        }
        SelectionAction::Collapse => {
            if let Some(parent_idx) = current_parent_index(rows, state.cursor_idx) {
                state.expanded[parent_idx] = false;
            }
        }
    }
}

fn toggle_current_row(
    state: &mut MultiSelectState,
    candidates: &[ApplyCandidate],
    visible_rows: &[VisibleRow],
) {
    match visible_rows.get(state.cursor_idx).copied() {
        Some(VisibleRow::Parent(idx)) => {
            state.selected[idx] = !state.selected[idx];
        }
        Some(VisibleRow::Version {
            parent_idx,
            version_idx,
        }) => {
            state.selected[parent_idx] = true;
            state.selected_version_idx[parent_idx] = version_idx;
            if !candidates[parent_idx].is_visible_by_default() {
                state.expanded[parent_idx] = true;
            }
        }
        None => {}
    }
}

fn current_parent_index(visible_rows: &[VisibleRow], cursor_idx: usize) -> Option<usize> {
    match visible_rows.get(cursor_idx).copied()? {
        VisibleRow::Parent(idx) => Some(idx),
        VisibleRow::Version { parent_idx, .. } => Some(parent_idx),
    }
}

fn selected_updates(
    candidates: &[ApplyCandidate],
    state: &MultiSelectState,
) -> Vec<Option<PlannedUpdate>> {
    candidates
        .iter()
        .enumerate()
        .map(|(idx, candidate)| {
            state
                .selected
                .get(idx)
                .copied()
                .unwrap_or(false)
                .then(|| candidate.clone_selected_update(state.selected_version_idx[idx]))
        })
        .collect()
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

fn has_advanced_view(candidates: &[ApplyCandidate]) -> bool {
    candidates
        .iter()
        .any(|candidate| !candidate.is_visible_by_default() || candidate.has_selectable_versions())
}

fn row_line(
    candidates: &[ApplyCandidate],
    state: &MultiSelectState,
    row: VisibleRow,
    pointer: &str,
    highlighted: bool,
    style: &DialogStyle<'_>,
) -> Line {
    match row {
        VisibleRow::Parent(idx) => {
            let candidate = &candidates[idx];
            let item = candidate.clone_selected_update(state.selected_version_idx[idx]);
            let selected = state.selected[idx];
            let tree_marker = tree_marker(candidate, state, idx, style.color);
            let prefix = format!("{pointer} {}", selection_marker(selected));
            update_row_line(
                &item,
                &prefix,
                tree_marker,
                candidate.note(),
                selected,
                candidate.is_force_candidate(),
                style.color,
                style.table_widths,
                highlighted,
                RowContent::Full,
            )
        }
        VisibleRow::Version {
            parent_idx,
            version_idx,
        } => {
            let candidate = &candidates[parent_idx];
            let version = &candidate.versions()[version_idx];
            let selected =
                state.selected[parent_idx] && state.selected_version_idx[parent_idx] == version_idx;
            let prefix = format!("{pointer} {}", selection_marker(selected));
            update_row_line(
                version.update(),
                &prefix,
                "  ",
                version.note(),
                selected,
                version.is_force(),
                style.color,
                style.table_widths,
                highlighted,
                RowContent::TargetOnly,
            )
        }
    }
}

fn tree_marker(
    candidate: &ApplyCandidate,
    state: &MultiSelectState,
    idx: usize,
    color: bool,
) -> &'static str {
    if state.view != DialogView::Tree || !candidate.has_selectable_versions() {
        return "  ";
    }

    if state.expanded[idx] {
        if color { "▼" } else { "v" }
    } else if color {
        "▶"
    } else {
        ">"
    }
}

fn draw_dialog_box(
    out: &mut io::Stdout,
    title: &Line,
    body: &[Line],
    footer: &Line,
    desired_inner_width: usize,
    last_height: usize,
) -> Result<usize> {
    clear_dialog(out, last_height)?;
    queue!(out, BeginSynchronizedUpdate)?;

    let render_result = (|| -> Result<usize> {
        let term_columns = terminal_columns()?;
        if term_columns < DIALOG_BOX_OVERHEAD {
            let fallback = truncate_with_ellipsis("Terminal too narrow", term_columns);
            write!(out, "{fallback}\r\n")?;
            return Ok(1);
        }

        let max_inner_width = term_columns.saturating_sub(DIALOG_BOX_OVERHEAD);
        let inner_width = desired_inner_width.min(max_inner_width);

        let hline = "─".repeat(inner_width + 2);
        write!(out, "┌{hline}┐\r\n")?;
        write_box_content_line(out, inner_width, title)?;
        write!(out, "├{hline}┤\r\n")?;

        for line in body {
            write_box_content_line(out, inner_width, line)?;
        }

        write!(out, "├{hline}┤\r\n")?;
        write_box_content_line(out, inner_width, footer)?;
        write!(out, "└{hline}┘\r\n")?;

        Ok(body.len() + 6)
    })();

    let end_sync_result = queue!(out, EndSynchronizedUpdate).map_err(anyhow::Error::from);
    let flush_result = out.flush().map_err(anyhow::Error::from);

    let rendered_height = render_result?;
    end_sync_result?;
    flush_result?;

    Ok(rendered_height)
}

fn terminal_columns() -> Result<usize> {
    Ok(usize::from(terminal::size()?.0))
}

fn write_box_content_line(out: &mut io::Stdout, inner_width: usize, line: &Line) -> Result<()> {
    if width(&line.plain) <= inner_width {
        let pad = inner_width.saturating_sub(width(&line.plain));
        write!(out, "│ {}{} │\r\n", line.styled, " ".repeat(pad))?;
        return Ok(());
    }

    let clipped = truncate_with_ellipsis(&line.plain, inner_width);
    let pad = inner_width.saturating_sub(width(&clipped));
    write!(out, "│ {}{} │\r\n", clipped, " ".repeat(pad))?;
    Ok(())
}

fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    if width(text) <= max_width {
        return text.to_string();
    }

    if max_width == 1 {
        return ELLIPSIS.to_string();
    }

    let mut clipped = String::new();
    let mut used = 0usize;
    let keep_width = max_width - 1;

    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width > keep_width {
            break;
        }

        clipped.push(ch);
        used += ch_width;
    }

    if clipped.is_empty() {
        ELLIPSIS.to_string()
    } else {
        clipped.push(ELLIPSIS);
        clipped
    }
}

fn multi_select_desired_inner_width(
    title: &str,
    footer: &str,
    widths: InteractiveTableWidths,
) -> usize {
    width(title)
        .max(width(footer))
        .max(interactive_table_body_width(widths))
}

const fn selection_marker(selected: bool) -> &'static str {
    if selected { "[x]" } else { "[ ]" }
}

fn interactive_table_widths(candidates: &[ApplyCandidate]) -> InteractiveTableWidths {
    candidates.iter().fold(
        InteractiveTableWidths {
            prefix: width("> [x]"),
            tree: width("  "),
            name: 0,
            current: 0,
            target: 0,
            status: width(PINNED_LABEL).max(width(FORCED_LABEL)),
            note: 0,
        },
        |mut widths, candidate| {
            let item = candidate.update();
            widths.name = widths.name.max(width(&item.name));
            widths.current = widths.current.max(width(&version_label(&item.current)));
            widths.target = widths.target.max(width(&version_label(&item.target)));
            widths.note = widths.note.max(width(candidate.note()));
            for version in candidate.versions() {
                let item = version.update();
                widths.current = widths.current.max(width(&version_label(&item.current)));
                widths.target = widths.target.max(width(&version_label(&item.target)));
                widths.note = widths.note.max(width(version.note()));
            }
            widths
        },
    )
}

fn update_row_line(
    item: &PlannedUpdate,
    prefix: &str,
    tree: &str,
    note: &str,
    selected: bool,
    force_candidate: bool,
    color: bool,
    widths: InteractiveTableWidths,
    highlighted: bool,
    content: RowContent,
) -> Line {
    let from_label = version_label(&item.current);
    let to_label = version_label(&item.target);
    let name = match content {
        RowContent::Full => item.name.as_str(),
        RowContent::TargetOnly => "",
    };
    let from_display = match content {
        RowContent::Full => from_label.as_str(),
        RowContent::TargetOnly => "",
    };
    let status = match content {
        RowContent::Full => row_status(selected, force_candidate),
        RowContent::TargetOnly => "",
    };

    let plain = update_row_text(
        RenderedCell::plain(prefix),
        RenderedCell::plain(tree),
        RenderedCell::plain(name),
        RenderedCell::plain(from_display),
        RenderedCell::plain(&to_label),
        RenderedCell::plain(status),
        RenderedCell::plain(note),
        widths,
        false,
    );

    let styled = if color && highlighted {
        let prefix_rendered = selected_text(prefix);
        let tree_rendered = selected_text(tree);
        let name_rendered = selected_text(name);
        let from_rendered = selected_text(from_display);
        let to_rendered = selected_to_version(&from_label, &to_label);
        let status_rendered = selected_status_text(status);
        let note_rendered = selected_text(note);
        update_row_text(
            RenderedCell::styled(&prefix_rendered, width(prefix)),
            RenderedCell::styled(&tree_rendered, width(tree)),
            RenderedCell::styled(&name_rendered, width(name)),
            RenderedCell::styled(&from_rendered, width(from_display)),
            RenderedCell::styled(&to_rendered, width(&to_label)),
            RenderedCell::styled(&status_rendered, width(status)),
            RenderedCell::styled(&note_rendered, width(note)),
            widths,
            true,
        )
    } else if color && status == PINNED_LABEL {
        plain.as_str().dark_grey().to_string()
    } else {
        let name_rendered = if color {
            name.bold().to_string()
        } else {
            name.to_string()
        };
        let to_rendered = render_to_version(&from_label, &to_label, color, false);
        let status_rendered = if color && status == FORCED_LABEL {
            status.red().bold().to_string()
        } else {
            status.to_string()
        };
        update_row_text(
            RenderedCell::plain(prefix),
            RenderedCell::plain(tree),
            RenderedCell::styled(&name_rendered, width(name)),
            RenderedCell::plain(from_display),
            RenderedCell::styled(&to_rendered, width(&to_label)),
            RenderedCell::styled(&status_rendered, width(status)),
            RenderedCell::plain(note),
            widths,
            false,
        )
    };

    Line { plain, styled }
}

#[derive(Debug, Clone, Copy)]
enum RowContent {
    Full,
    TargetOnly,
}

fn row_status(selected: bool, force_candidate: bool) -> &'static str {
    if selected && force_candidate {
        FORCED_LABEL
    } else if selected || force_candidate {
        ""
    } else {
        PINNED_LABEL
    }
}

fn update_row_text(
    prefix: RenderedCell<'_>,
    tree: RenderedCell<'_>,
    name: RenderedCell<'_>,
    from: RenderedCell<'_>,
    to: RenderedCell<'_>,
    status: RenderedCell<'_>,
    note: RenderedCell<'_>,
    widths: InteractiveTableWidths,
    highlighted: bool,
) -> String {
    let gap = if highlighted {
        selected_text(INTERACTIVE_TABLE_GAP)
    } else {
        INTERACTIVE_TABLE_GAP.to_string()
    };

    format!(
        "{}{}{}{}{}{}{}{}{}{}{}{}{}",
        padded_cell(prefix, widths.prefix, highlighted),
        gap,
        padded_cell(tree, widths.tree, highlighted),
        gap,
        padded_cell(name, widths.name, highlighted),
        gap,
        padded_cell(from, widths.current, highlighted),
        gap,
        padded_cell(to, widths.target, highlighted),
        gap,
        padded_cell(status, widths.status, highlighted),
        gap,
        padded_cell(note, widths.note, highlighted),
    )
}

fn interactive_table_body_width(widths: InteractiveTableWidths) -> usize {
    widths.prefix
        + widths.tree
        + widths.name
        + widths.current
        + widths.target
        + widths.status
        + widths.note
        + (6 * width(INTERACTIVE_TABLE_GAP))
}

fn padded_cell(cell: RenderedCell<'_>, target_width: usize, highlighted: bool) -> String {
    let padding = target_width.saturating_sub(cell.width);
    let pad = " ".repeat(padding);
    if highlighted {
        format!("{}{}", cell.text, selected_text(&pad))
    } else {
        format!("{}{}", cell.text, pad)
    }
}

fn selected_text(text: &str) -> String {
    text.black().on_cyan().bold().to_string()
}

fn selected_status_text(text: &str) -> String {
    if text == FORCED_LABEL {
        text.red().on_cyan().bold().to_string()
    } else {
        selected_text(text)
    }
}

fn selected_to_version(from: &str, to: &str) -> String {
    let from_core = strip_v_prefix(from);
    let to_core = strip_v_prefix(to);
    let from_parts: Vec<&str> = from_core.split('.').collect();
    let to_parts: Vec<&str> = to_core.split('.').collect();
    let changed_from = first_changed_part_index(&from_parts, &to_parts);

    let mut out = String::new();
    if to.starts_with('v') {
        out.push_str(&selected_version_part("v", false));
    }

    for (idx, part) in to_parts.iter().enumerate() {
        if idx > 0 {
            out.push_str(&selected_text("."));
        }

        let changed = changed_from.is_some_and(|first| idx >= first);
        out.push_str(&selected_version_part(part, changed));
    }

    out
}

fn selected_version_part(part: &str, changed: bool) -> String {
    if changed {
        part.blue().on_cyan().bold().to_string()
    } else {
        selected_text(part)
    }
}

fn first_changed_part_index(from_parts: &[&str], to_parts: &[&str]) -> Option<usize> {
    let max_len = from_parts.len().max(to_parts.len());
    for idx in 0..max_len {
        let a = from_parts.get(idx).copied();
        let b = to_parts.get(idx).copied();
        if a != b {
            return Some(idx);
        }
    }

    None
}

fn title_line(manager: &str, theme: OutputTheme) -> Line {
    let plain = format!("{DIALOG_TITLE_PREFIX} {manager}");
    let styled = if theme.color() {
        format!("{DIALOG_TITLE_PREFIX} {}", manager.cyan().bold())
    } else {
        plain.clone()
    };

    Line { plain, styled }
}

fn footer_line(color: bool, has_advanced: bool) -> Line {
    if !has_advanced {
        return key_footer_line(color, MULTI_SELECT_KEYBINDS);
    }

    let labels = MULTI_SELECT_KEYBINDS
        .iter()
        .chain(ADVANCED_KEYBINDS.iter())
        .copied()
        .collect::<Vec<_>>();
    key_footer_line(color, &labels)
}

fn key_footer_line(color: bool, labels: &[(&str, &str)]) -> Line {
    let plain = labels
        .iter()
        .map(|(k, l)| format!("{k} {l}"))
        .collect::<Vec<_>>()
        .join(" | ");

    if !color {
        return Line {
            styled: plain.clone(),
            plain,
        };
    }

    let mut styled_parts = Vec::with_capacity(labels.len());
    for (key, label) in labels {
        styled_parts.push(format!("{} {}", key, label.dim()));
    }

    Line {
        plain,
        styled: styled_parts.join(&format!(" {} ", "|".dim())),
    }
}

fn clear_dialog(out: &mut io::Stdout, lines: usize) -> Result<()> {
    out.flush()?;
    move_up_lines(out, lines)?;
    queue!(
        out,
        cursor::MoveToColumn(0),
        terminal::Clear(ClearType::FromCursorDown)
    )?;
    Ok(())
}

fn move_up_lines(out: &mut io::Stdout, mut lines: usize) -> Result<()> {
    while lines > 0 {
        let capped = lines.min(usize::from(u16::MAX));
        let step = u16::try_from(capped).unwrap_or(u16::MAX);
        queue!(out, cursor::MoveUp(step))?;
        lines -= usize::from(step);
    }

    Ok(())
}

fn cleanup_terminal(out: &mut io::Stdout, last_height: usize) -> Result<()> {
    let mut cleanup_error: Option<anyhow::Error> = clear_dialog(out, last_height)
        .and_then(|()| out.flush().map_err(anyhow::Error::from))
        .err();

    if let Err(err) = execute!(out, cursor::Show)
        && cleanup_error.is_none()
    {
        cleanup_error = Some(err.into());
    }

    if let Err(err) = terminal::disable_raw_mode()
        && cleanup_error.is_none()
    {
        cleanup_error = Some(err.into());
    }

    if let Some(err) = cleanup_error {
        return Err(err);
    }

    Ok(())
}

fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned_update(name: &str, current: &str, target: &str) -> PlannedUpdate {
        PlannedUpdate {
            manager: "test",
            name: name.to_string(),
            current: current.to_string(),
            target: target.to_string(),
            delayed_latest: None,
            version_policy: None,
            apply_spec_base: None,
            gate_bypass: Default::default(),
        }
    }

    fn recommended_candidates(items: &[PlannedUpdate]) -> Vec<ApplyCandidate> {
        items
            .iter()
            .cloned()
            .map(ApplyCandidate::recommended)
            .collect()
    }

    #[test]
    fn interactive_rows_align_as_headerless_table() {
        let items = [
            planned_update("short", "1.0.0", "1.2.0"),
            planned_update("much-longer-name", "2.0.0", "2.1.0"),
        ];
        let candidates = recommended_candidates(&items);
        let widths = interactive_table_widths(&candidates);

        let first = update_row_line(
            &items[0],
            "> [x]",
            "  ",
            "",
            true,
            false,
            false,
            widths,
            false,
            RowContent::Full,
        );
        let second = update_row_line(
            &items[1],
            "  [x]",
            "  ",
            "",
            true,
            false,
            false,
            widths,
            false,
            RowContent::Full,
        );

        assert!(!first.plain.contains("Status"));
        assert_eq!(first.plain.find("v1.0.0"), second.plain.find("v2.0.0"));
        assert_eq!(first.plain.find("v1.2.0"), second.plain.find("v2.1.0"));
    }

    #[test]
    fn interactive_rows_keep_name_bold_when_color_is_enabled() {
        let item = planned_update("tool", "1.0.0", "1.2.0");
        let candidates = recommended_candidates(std::slice::from_ref(&item));
        let widths = interactive_table_widths(&candidates);

        let line = update_row_line(
            &item,
            "> [x]",
            "  ",
            "",
            true,
            false,
            true,
            widths,
            false,
            RowContent::Full,
        );

        assert!(line.styled.contains("\u{1b}[1m"));
    }

    #[test]
    fn highlighted_rows_keep_cell_formatting_and_cover_notes() {
        let item = planned_update("tool", "1.0.0", "1.2.0");
        let candidates = recommended_candidates(std::slice::from_ref(&item));
        let widths = interactive_table_widths(&candidates);

        let line = update_row_line(
            &item,
            "> [x]",
            "  ",
            "latest v1.3.0 too fresh",
            true,
            false,
            true,
            widths,
            true,
            RowContent::Full,
        );

        assert!(line.styled.contains(&selected_text("tool")));
        assert!(
            line.styled
                .contains(&selected_to_version("v1.0.0", "v1.2.0"))
        );
        assert!(
            line.styled
                .contains(&selected_text("latest v1.3.0 too fresh"))
        );
        assert!(line.styled.contains(&selected_text(INTERACTIVE_TABLE_GAP)));
    }

    #[test]
    fn target_only_rows_hide_name_current_and_status() {
        let item = planned_update("tool", "1.0.0", "1.2.0");
        let candidates = recommended_candidates(std::slice::from_ref(&item));
        let widths = interactive_table_widths(&candidates);

        let line = update_row_line(
            &item,
            "  [ ]",
            "  ",
            "released: 2d",
            false,
            false,
            false,
            widths,
            false,
            RowContent::TargetOnly,
        );

        assert!(!line.plain.contains("tool"));
        assert!(!line.plain.contains("v1.0.0"));
        assert!(!line.plain.contains(PINNED_LABEL));
        assert!(line.plain.contains("v1.2.0"));
        assert!(line.plain.contains("released: 2d"));
    }

    #[test]
    fn tree_marker_column_keeps_names_aligned() {
        let items = [
            planned_update("expandable", "1.0.0", "1.2.0"),
            planned_update("plain", "2.0.0", "2.1.0"),
        ];
        let candidates = recommended_candidates(&items);
        let widths = interactive_table_widths(&candidates);

        let expandable = update_row_line(
            &items[0],
            "> [x]",
            "▶",
            "",
            true,
            false,
            false,
            widths,
            false,
            RowContent::Full,
        );
        let plain = update_row_line(
            &items[1],
            "  [x]",
            "  ",
            "",
            true,
            false,
            false,
            widths,
            false,
            RowContent::Full,
        );

        let expandable_col =
            width(&expandable.plain[..expandable.plain.find("expandable").unwrap()]);
        let plain_col = width(&plain.plain[..plain.plain.find("plain").unwrap()]);

        assert_eq!(expandable_col, plain_col);
    }

    #[test]
    fn forced_rows_render_forced_status_in_red() {
        let item = planned_update("tool", "1.0.0", "1.2.0");
        let candidates = vec![ApplyCandidate::force_candidate(item.clone())];
        let widths = interactive_table_widths(&candidates);

        let line = update_row_line(
            &item,
            "> [x]",
            "  ",
            "",
            true,
            true,
            true,
            widths,
            false,
            RowContent::Full,
        );

        assert!(line.plain.contains(FORCED_LABEL));
        assert!(line.styled.contains("\u{1b}["));
    }

    #[test]
    fn unselected_force_rows_do_not_render_as_pinned() {
        assert_eq!(row_status(false, true), "");
    }

    #[test]
    fn hidden_force_candidate_stays_visible_only_when_selected() {
        let items = [planned_update("tool", "1.0.0", "1.2.0")];
        let candidates = vec![ApplyCandidate::force_candidate(items[0].clone())];
        let mut state = MultiSelectState {
            selected: vec![false],
            selected_version_idx: vec![0],
            expanded: vec![false],
            cursor_idx: 0,
            view: DialogView::List,
        };

        assert!(visible_rows(&candidates, &state).is_empty());

        state.view = DialogView::Tree;
        assert_eq!(
            visible_rows(&candidates, &state),
            vec![VisibleRow::Parent(0)]
        );

        state.selected[0] = true;
        state.view = DialogView::List;
        assert_eq!(
            visible_rows(&candidates, &state),
            vec![VisibleRow::Parent(0)]
        );

        state.view = DialogView::Tree;
        state.selected[0] = false;
        state.view = DialogView::List;
        assert!(visible_rows(&candidates, &state).is_empty());
    }
}
