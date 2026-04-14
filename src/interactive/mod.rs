pub mod apply;

use crate::config::PIN_ALL;
use crate::managers::common::PlannedUpdate;
use crate::outcome::version_label;
use crate::ui::{output_theme, with_spinner_suspended};
use anyhow::{Result, bail};
use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::Stylize;
use crossterm::terminal::{self, ClearType};
use crossterm::{execute, queue};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::io::{self, IsTerminal, Write};

type Line = (String, String);

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

pub fn confirm_global_manager_apply(manager: &str, default_apply_all: bool) -> Result<bool> {
    with_spinner_suspended(|| run_global_choice_dialog(manager, default_apply_all))
}

fn run_multi_select_dialog(
    manager: &str,
    items: &[PlannedUpdate],
    pinned: &BTreeSet<String>,
) -> Result<Vec<bool>> {
    let color = output_theme().color();
    let mut out = io::stdout();
    terminal::enable_raw_mode()?;
    if let Err(err) = execute!(out, cursor::Hide) {
        let _ = terminal::disable_raw_mode();
        return Err(err.into());
    }

    let mut last_height = 0usize;
    let result = (|| -> Result<Vec<bool>> {
        let mut selected: Vec<bool> = items
            .iter()
            .map(|item| !(pinned.contains(PIN_ALL) || pinned.contains(&item.name)))
            .collect();
        let mut cursor_idx = 0usize;

        loop {
            let title = title_line(manager, color);
            let mut body = Vec::with_capacity(items.len());

            for (idx, item) in items.iter().enumerate() {
                let name = &item.name;
                let marker = if selected[idx] { "[x]" } else { "[ ]" };
                let pointer = if idx == cursor_idx { ">" } else { " " };
                let item_is_pinned = !selected[idx];
                let from_label = version_label(&item.current);
                let to_label = version_label(&item.target);
                let mut plain = format!("{pointer} {marker} {name} {from_label} -> {to_label}");
                if item_is_pinned {
                    plain.push_str(" (pinned)");
                }

                let mut styled = plain.clone();
                if color && item_is_pinned {
                    styled = styled.dark_grey().to_string();
                }
                if color && idx == cursor_idx {
                    styled = styled.black().on_cyan().bold().to_string();
                }

                body.push((plain, styled));
            }

            let footer = key_footer_line(
                color,
                &[
                    ("↑/↓/j/k", "move"),
                    ("space", "toggle"),
                    ("a", "all"),
                    ("n", "none"),
                    ("enter", "confirm"),
                ],
            );

            last_height = draw_dialog_box(&mut out, &title, &body, &footer, last_height)?;

            let Event::Key(key) = event::read()? else {
                continue;
            };

            if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
                continue;
            }

            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    cursor_idx = cursor_idx.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if cursor_idx + 1 < items.len() {
                        cursor_idx += 1;
                    }
                }
                KeyCode::Char(' ') => {
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

fn run_global_choice_dialog(manager: &str, default_apply_all: bool) -> Result<bool> {
    let color = output_theme().color();
    let mut out = io::stdout();
    terminal::enable_raw_mode()?;
    if let Err(err) = execute!(out, cursor::Hide) {
        let _ = terminal::disable_raw_mode();
        return Err(err.into());
    }

    let mut last_height = 0usize;
    let result = (|| -> Result<bool> {
        let mut apply_all = default_apply_all;

        loop {
            let title = title_line(manager, color);

            let mut body = Vec::new();
            body.push((
                "This manager supports global apply only (all-or-none).".to_string(),
                if color {
                    "This manager supports global apply only (all-or-none)."
                        .dark_grey()
                        .to_string()
                } else {
                    "This manager supports global apply only (all-or-none).".to_string()
                },
            ));

            let items = [
                ("apply all updates", apply_all),
                ("apply no updates (skip manager)", !apply_all),
            ];
            for (item, enabled) in items {
                let marker = if enabled { "(x)" } else { "( )" };
                let pointer = if enabled { ">" } else { " " };
                let plain = format!("{pointer} {marker} {item}");
                let styled = if color && enabled {
                    plain.clone().black().on_cyan().bold().to_string()
                } else {
                    plain.clone()
                };
                body.push((plain, styled));
            }

            let footer = key_footer_line(
                color,
                &[
                    ("↑/↓/j/k", "move"),
                    ("a", "all"),
                    ("n", "none"),
                    ("enter", "confirm"),
                ],
            );

            last_height = draw_dialog_box(&mut out, &title, &body, &footer, last_height)?;

            let Event::Key(key) = event::read()? else {
                continue;
            };

            if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
                continue;
            }

            match key.code {
                KeyCode::Up | KeyCode::Down | KeyCode::Char('k' | 'j') => {
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
    last_height: usize,
) -> Result<usize> {
    clear_dialog(out, last_height)?;

    let mut inner_width = width(&title.0).max(width(&footer.0));
    for (plain, _) in body {
        inner_width = inner_width.max(width(plain));
    }

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

    out.flush()?;

    Ok(body.len() + 6)
}

fn write_box_content_line(out: &mut io::Stdout, inner_width: usize, line: &Line) -> Result<()> {
    let pad = inner_width.saturating_sub(width(&line.0));
    write!(out, "│ {}{} │\r\n", line.1, " ".repeat(pad))?;
    Ok(())
}

fn title_line(manager: &str, color: bool) -> Line {
    let plain = format!("Select packages for {manager}");
    let styled = if color {
        format!("Select packages for {}", manager.cyan().bold())
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

fn clear_dialog(out: &mut io::Stdout, lines: usize) -> Result<()> {
    for _ in 0..lines {
        queue!(
            out,
            cursor::MoveUp(1),
            cursor::MoveToColumn(0),
            terminal::Clear(ClearType::CurrentLine)
        )?;
    }

    out.flush()?;
    Ok(())
}

fn cleanup_terminal(out: &mut io::Stdout, last_height: usize) -> Result<()> {
    let mut cleanup_error: Option<anyhow::Error> = clear_dialog(out, last_height).err();
    if let Err(err) = terminal::disable_raw_mode()
        && cleanup_error.is_none()
    {
        cleanup_error = Some(err.into());
    }
    if let Err(err) = execute!(out, cursor::Show)
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
    s.chars().count()
}
