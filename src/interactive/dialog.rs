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
use crate::managers::PlannedUpdate;
use crate::outcome::{render_to_version, version_label};
use crate::ui::{OutputTheme, output_theme, with_spinner_suspended};

struct Line {
    plain: String,
    styled: String,
}

const DIALOG_TITLE_PREFIX: &str = "Select updates for";
const PINNED_LABEL: &str = " (pinned)";
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

struct MultiSelectState {
    selected: Vec<bool>,
    cursor_idx: usize,
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
    name: usize,
    current: usize,
    target: usize,
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

pub fn choose_items_for_manager(
    manager: &str,
    items: &[PlannedUpdate],
    pinned: &BTreeSet<String>,
) -> Result<Vec<bool>> {
    if items.is_empty() {
        return Ok(Vec::new());
    }

    with_spinner_suspended(|| run_multi_select_dialog(manager, items, pinned))
}

fn run_multi_select_dialog(
    manager: &str,
    items: &[PlannedUpdate],
    pinned: &BTreeSet<String>,
) -> Result<Vec<bool>> {
    let theme = output_theme();
    let color = theme.color();
    let title = title_line(manager, theme);
    let footer = key_footer_line(color, MULTI_SELECT_KEYBINDS);
    let table_widths = interactive_table_widths(items);
    let desired_inner_width =
        multi_select_desired_inner_width(&title.plain, &footer.plain, table_widths);

    with_dialog_terminal(|out, last_height| {
        let mut state = MultiSelectState {
            selected: items
                .iter()
                .map(|item| !is_pinned(&item.name, pinned))
                .collect(),
            cursor_idx: 0,
        };

        let style = DialogStyle {
            title: &title,
            footer: &footer,
            desired_inner_width,
            color,
            table_widths,
        };

        run_dialog_loop(out, last_height, &mut state, items, &style)
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
    items: &[PlannedUpdate],
    style: &DialogStyle<'_>,
) -> Result<Vec<bool>> {
    loop {
        let mut body = Vec::with_capacity(items.len());

        for (idx, item) in items.iter().enumerate() {
            let marker = selection_marker(state.selected[idx]);
            let pointer = if idx == state.cursor_idx { ">" } else { " " };
            let prefix = format!("{pointer} {marker}");
            let mut line = update_row_line(
                item,
                &prefix,
                state.selected[idx],
                style.color,
                style.table_widths,
            );
            if style.color && idx == state.cursor_idx {
                line.styled = line.styled.as_str().black().on_cyan().bold().to_string();
            }

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
                state.cursor_idx = if state.cursor_idx == 0 {
                    items.len() - 1
                } else {
                    state.cursor_idx - 1
                };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.cursor_idx = if state.cursor_idx + 1 >= items.len() {
                    0
                } else {
                    state.cursor_idx + 1
                };
            }
            KeyCode::Enter => {
                return Ok(std::mem::take(&mut state.selected));
            }
            _ => {
                if let Some(action) = selection_action_for_key(key_code) {
                    apply_selection_action_to_list(&mut state.selected, state.cursor_idx, action);
                }
            }
        }
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
        _ => None,
    }
}

fn apply_selection_action_to_list(
    selected: &mut [bool],
    cursor_idx: usize,
    action: SelectionAction,
) {
    match action {
        SelectionAction::Toggle => {
            selected[cursor_idx] = !selected[cursor_idx];
        }
        SelectionAction::SelectAll => selected.fill(true),
        SelectionAction::SelectNone => selected.fill(false),
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

fn interactive_table_widths(items: &[PlannedUpdate]) -> InteractiveTableWidths {
    items
        .iter()
        .fold(
            InteractiveTableWidths {
                prefix: width("> [x]"),
                name: 0,
                current: 0,
                target: 0,
            },
            |mut widths, item| {
                widths.name = widths.name.max(width(&item.name));
                widths.current = widths.current.max(width(&version_label(&item.current)));
                widths.target = widths.target.max(width(&version_label(&item.target)));
                widths
            },
        )
}

fn update_row_line(
    item: &PlannedUpdate,
    prefix: &str,
    selected: bool,
    color: bool,
    widths: InteractiveTableWidths,
) -> Line {
    let from_label = version_label(&item.current);
    let to_label = version_label(&item.target);
    let pinned = !selected;

    let plain = update_row_text(
        RenderedCell::plain(prefix),
        RenderedCell::plain(&item.name),
        RenderedCell::plain(&from_label),
        RenderedCell::plain(&to_label),
        pinned,
        widths,
    );

    let styled = if color && pinned {
        plain.as_str().dark_grey().to_string()
    } else {
        let name_rendered = if color {
            item.name.as_str().bold().to_string()
        } else {
            item.name.clone()
        };
        let to_rendered = render_to_version(&from_label, &to_label, color, false);
        update_row_text(
            RenderedCell::plain(prefix),
            RenderedCell::styled(&name_rendered, width(&item.name)),
            RenderedCell::plain(&from_label),
            RenderedCell::styled(&to_rendered, width(&to_label)),
            pinned,
            widths,
        )
    };

    Line { plain, styled }
}

fn update_row_text(
    prefix: RenderedCell<'_>,
    name: RenderedCell<'_>,
    from: RenderedCell<'_>,
    to: RenderedCell<'_>,
    pinned: bool,
    widths: InteractiveTableWidths,
) -> String {
    let mut line = format!(
        "{}{gap}{}{gap}{}{gap}{}",
        padded_cell(prefix, widths.prefix),
        padded_cell(name, widths.name),
        padded_cell(from, widths.current),
        padded_cell(to, widths.target),
        gap = INTERACTIVE_TABLE_GAP,
    );
    if pinned {
        line.push_str(PINNED_LABEL);
    }

    line
}

fn interactive_table_body_width(widths: InteractiveTableWidths) -> usize {
    widths.prefix
        + widths.name
        + widths.current
        + widths.target
        + (3 * width(INTERACTIVE_TABLE_GAP))
        + width(PINNED_LABEL)
}

fn padded_cell(cell: RenderedCell<'_>, target_width: usize) -> String {
    let padding = target_width.saturating_sub(cell.width);
    format!("{}{}", cell.text, " ".repeat(padding))
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
        }
    }

    #[test]
    fn interactive_rows_align_as_headerless_table() {
        let items = [
            planned_update("short", "1.0.0", "1.2.0"),
            planned_update("much-longer-name", "2.0.0", "2.1.0"),
        ];
        let widths = interactive_table_widths(&items);

        let first = update_row_line(&items[0], "> [x]", true, false, widths);
        let second = update_row_line(&items[1], "  [x]", true, false, widths);

        assert!(!first.plain.contains("Status"));
        assert_eq!(first.plain.find("v1.0.0"), second.plain.find("v2.0.0"));
        assert_eq!(first.plain.find("v1.2.0"), second.plain.find("v2.1.0"));
    }

    #[test]
    fn interactive_rows_keep_name_bold_when_color_is_enabled() {
        let item = planned_update("tool", "1.0.0", "1.2.0");
        let widths = interactive_table_widths(std::slice::from_ref(&item));

        let line = update_row_line(&item, "> [x]", true, true, widths);

        assert!(line.styled.contains("\u{1b}[1m"));
    }
}
