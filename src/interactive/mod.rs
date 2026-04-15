pub mod apply;

use crate::config::PIN_ALL;
use crate::managers::common::PlannedUpdate;
use crate::outcome::{render_to_version, version_label};
use crate::ui::{output_theme, with_spinner_suspended};
use anyhow::{Result, bail};
use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::Stylize;
use crossterm::terminal::{self, BeginSynchronizedUpdate, ClearType, EndSynchronizedUpdate};
use crossterm::{execute, queue};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::io::{self, IsTerminal, Write};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

type Line = (String, String);
const DIALOG_TITLE_PREFIX: &str = "Select updates for";
const GLOBAL_APPLY_ONLY_NOTE: &str = "This manager supports global apply only (all-or-none).";
const PINNED_LABEL: &str = " (pinned)";
const DIALOG_BOX_OVERHEAD: usize = 4;
const ELLIPSIS: char = '…';

const MULTI_SELECT_KEYBINDS: &[(&str, &str)] = &[
    ("↑/↓/j/k", "move"),
    ("space/x", "toggle"),
    ("a", "all"),
    ("n", "none"),
    ("enter", "confirm"),
];
const GLOBAL_SELECT_KEYBINDS: &[(&str, &str)] = &[
    ("space/x", "toggle"),
    ("a", "all"),
    ("n", "none"),
    ("enter", "confirm"),
];

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

pub fn confirm_global_manager_apply(
    manager: &str,
    items: &[PlannedUpdate],
    default_apply_all: bool,
) -> Result<bool> {
    with_spinner_suspended(|| run_global_choice_dialog(manager, items, default_apply_all))
}

fn run_multi_select_dialog(
    manager: &str,
    items: &[PlannedUpdate],
    pinned: &BTreeSet<String>,
) -> Result<Vec<bool>> {
    let theme = output_theme();
    let color = theme.color();
    let arrow = version_arrow(theme.unicode());
    let mut out = io::stdout();
    terminal::enable_raw_mode()?;
    if let Err(err) = execute!(out, cursor::Hide) {
        let _ = terminal::disable_raw_mode();
        return Err(err.into());
    }

    let title = title_line(manager, color);
    let footer = key_footer_line(color, MULTI_SELECT_KEYBINDS);
    let desired_inner_width = multi_select_desired_inner_width(&title.0, &footer.0, items, arrow);

    let mut last_height = 0usize;
    let force_full_clear = false;
    let result = (|| -> Result<Vec<bool>> {
        let mut selected: Vec<bool> = items
            .iter()
            .map(|item| !(pinned.contains(PIN_ALL) || pinned.contains(&item.name)))
            .collect();
        let mut cursor_idx = 0usize;

        loop {
            let mut body = Vec::with_capacity(items.len());

            for (idx, item) in items.iter().enumerate() {
                let marker = selection_marker(selected[idx]);
                let pointer = if idx == cursor_idx { ">" } else { " " };
                let prefix = format!("{pointer} {marker}");
                let mut line = update_row_line(item, &prefix, selected[idx], color, arrow);
                if color && idx == cursor_idx {
                    line.1 = line.0.clone().black().on_cyan().bold().to_string();
                }

                body.push(line);
            }

            last_height = draw_dialog_box(
                &mut out,
                &title,
                &body,
                &footer,
                desired_inner_width,
                last_height,
                force_full_clear,
            )?;

            let event = event::read()?;
            if let Event::Resize(_, _) = event {
                continue;
            }

            let Event::Key(key) = event else {
                continue;
            };

            if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
                continue;
            }

            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    cursor_idx = if cursor_idx == 0 {
                        items.len() - 1
                    } else {
                        cursor_idx - 1
                    };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    cursor_idx = if cursor_idx + 1 >= items.len() {
                        0
                    } else {
                        cursor_idx + 1
                    };
                }
                KeyCode::Char(' ' | 'x') => {
                    selected[cursor_idx] = !selected[cursor_idx];
                }
                KeyCode::Char('a') => {
                    selected.fill(true);
                }
                KeyCode::Char('n') => {
                    selected.fill(false);
                }
                KeyCode::Enter => {
                    return Ok(selected);
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(anyhow::Error::new(InteractiveCancelled));
                }
                _ => {}
            }
        }
    })();

    cleanup_terminal(&mut out, last_height)?;

    let selected = result?;

    Ok(selected)
}

fn run_global_choice_dialog(
    manager: &str,
    items: &[PlannedUpdate],
    default_apply_all: bool,
) -> Result<bool> {
    let theme = output_theme();
    let color = theme.color();
    let arrow = version_arrow(theme.unicode());
    let mut out = io::stdout();
    terminal::enable_raw_mode()?;
    if let Err(err) = execute!(out, cursor::Hide) {
        let _ = terminal::disable_raw_mode();
        return Err(err.into());
    }

    let title = title_line(manager, color);
    let footer = key_footer_line(color, GLOBAL_SELECT_KEYBINDS);
    let desired_inner_width = global_choice_desired_inner_width(&title.0, &footer.0, items, arrow);

    let mut last_height = 0usize;
    let force_full_clear = false;
    let result = (|| -> Result<bool> {
        let mut apply_all = default_apply_all;

        loop {
            let mut body = Vec::with_capacity(items.len() + 2);
            body.push(dimmed_line(GLOBAL_APPLY_ONLY_NOTE, color));
            body.push(blank_line());

            let marker = selection_marker(apply_all);
            for item in items {
                body.push(update_row_line(item, marker, apply_all, color, arrow));
            }

            last_height = draw_dialog_box(
                &mut out,
                &title,
                &body,
                &footer,
                desired_inner_width,
                last_height,
                force_full_clear,
            )?;

            let event = event::read()?;
            if let Event::Resize(_, _) = event {
                continue;
            }

            let Event::Key(key) = event else {
                continue;
            };

            if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
                continue;
            }

            match key.code {
                KeyCode::Char(' ' | 'x') => {
                    apply_all = !apply_all;
                }
                KeyCode::Char('a') => {
                    apply_all = true;
                }
                KeyCode::Char('n') => {
                    apply_all = false;
                }
                KeyCode::Enter => {
                    return Ok(apply_all);
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(anyhow::Error::new(InteractiveCancelled));
                }
                _ => {}
            }
        }
    })();

    cleanup_terminal(&mut out, last_height)?;
    result
}

fn draw_dialog_box(
    out: &mut io::Stdout,
    title: &Line,
    body: &[Line],
    footer: &Line,
    desired_inner_width: usize,
    last_height: usize,
    force_full_clear: bool,
) -> Result<usize> {
    clear_dialog(out, last_height, force_full_clear)?;
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
    if width(&line.0) <= inner_width {
        let pad = inner_width.saturating_sub(width(&line.0));
        write!(out, "│ {}{} │\r\n", line.1, " ".repeat(pad))?;
        return Ok(());
    }

    let clipped = truncate_with_ellipsis(&line.0, inner_width);
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
    items: &[PlannedUpdate],
    arrow: &str,
) -> usize {
    let mut desired_width = width(title).max(width(footer));
    for item in items {
        let from_label = version_label(&item.current);
        let to_label = version_label(&item.target);
        let row = update_row_text("> [x]", &item.name, &from_label, &to_label, arrow, true);
        desired_width = desired_width.max(width(&row));
    }

    desired_width
}

fn global_choice_desired_inner_width(
    title: &str,
    footer: &str,
    items: &[PlannedUpdate],
    arrow: &str,
) -> usize {
    let mut desired_width = width(title)
        .max(width(footer))
        .max(width(GLOBAL_APPLY_ONLY_NOTE));

    for item in items {
        let from_label = version_label(&item.current);
        let to_label = version_label(&item.target);
        let row = update_row_text(
            selection_marker(true),
            &item.name,
            &from_label,
            &to_label,
            arrow,
            true,
        );
        desired_width = desired_width.max(width(&row));
    }

    desired_width
}

fn selection_marker(selected: bool) -> &'static str {
    if selected { "[x]" } else { "[ ]" }
}

fn version_arrow(unicode: bool) -> &'static str {
    if unicode { "→" } else { "->" }
}

fn blank_line() -> Line {
    (String::new(), String::new())
}

fn dimmed_line(text: &str, color: bool) -> Line {
    let plain = text.to_string();
    let styled = if color {
        text.dark_grey().to_string()
    } else {
        plain.clone()
    };

    (plain, styled)
}

fn update_row_line(
    item: &PlannedUpdate,
    prefix: &str,
    selected: bool,
    color: bool,
    arrow: &str,
) -> Line {
    let from_label = version_label(&item.current);
    let to_label = version_label(&item.target);
    let pinned = !selected;

    let plain = update_row_text(prefix, &item.name, &from_label, &to_label, arrow, pinned);

    let styled = if color && pinned {
        plain.clone().dark_grey().to_string()
    } else {
        let to_rendered = render_to_version(&from_label, &to_label, color, false);
        update_row_text(prefix, &item.name, &from_label, &to_rendered, arrow, pinned)
    };

    (plain, styled)
}

fn update_row_text(
    prefix: &str,
    name: &str,
    from: &str,
    to: &str,
    arrow: &str,
    pinned: bool,
) -> String {
    let mut line = format!("{prefix} {name} {from} {arrow} {to}");
    if pinned {
        line.push_str(PINNED_LABEL);
    }

    line
}

fn title_line(manager: &str, color: bool) -> Line {
    let plain = format!("{DIALOG_TITLE_PREFIX} {manager}");
    let styled = if color {
        format!("{DIALOG_TITLE_PREFIX} {}", manager.cyan().bold())
    } else {
        plain.clone()
    };

    (plain, styled)
}

fn key_footer_line(color: bool, labels: &[(&str, &str)]) -> Line {
    let plain = labels
        .iter()
        .map(|(k, l)| format!("{k} {l}"))
        .collect::<Vec<_>>()
        .join(" | ");

    if !color {
        return (plain.clone(), plain);
    }

    let mut styled_parts = Vec::with_capacity(labels.len());
    for (key, label) in labels {
        styled_parts.push(format!("{} {}", key, label.dim()));
    }

    (plain, styled_parts.join(&format!(" {} ", "|".dim())))
}

fn clear_dialog(out: &mut io::Stdout, lines: usize, force_full_clear: bool) -> Result<()> {
    if force_full_clear {
        queue!(
            out,
            cursor::MoveTo(0, 0),
            terminal::Clear(ClearType::All),
            cursor::MoveTo(0, 0)
        )?;
        return Ok(());
    }

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
        let step = lines.min(usize::from(u16::MAX)) as u16;
        queue!(out, cursor::MoveUp(step))?;
        lines -= usize::from(step);
    }

    Ok(())
}

fn cleanup_terminal(out: &mut io::Stdout, last_height: usize) -> Result<()> {
    let mut cleanup_error: Option<anyhow::Error> = clear_dialog(out, last_height, false)
        .and_then(|_| out.flush().map_err(anyhow::Error::from))
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
