use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

use super::SelectionPlan;
use super::model::SelectionApp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SelectionControl {
    Continue,
    Confirm,
    Cancel,
}

pub(super) fn handle_event(
    event: &Event,
    app: &mut SelectionApp,
    plans: &[SelectionPlan],
) -> SelectionControl {
    let Event::Key(key) = event else {
        return SelectionControl::Continue;
    };

    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return SelectionControl::Continue;
    }

    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return SelectionControl::Cancel;
    }

    if app.version_picker.is_some() {
        return handle_version_picker_key(key.code, key.modifiers, app, plans);
    }

    if key.code == KeyCode::Char('C') {
        return SelectionControl::Confirm;
    }

    match key.code {
        KeyCode::Char('q') => SelectionControl::Cancel,
        KeyCode::Up | KeyCode::Char('k') => {
            app.move_cursor_up(plans);
            SelectionControl::Continue
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.move_cursor_down(plans);
            SelectionControl::Continue
        }
        KeyCode::BackTab => {
            app.previous_tab();
            SelectionControl::Continue
        }
        KeyCode::Tab => {
            app.next_tab();
            SelectionControl::Continue
        }
        KeyCode::Char(' ' | 'x') => {
            app.toggle_current(plans);
            SelectionControl::Continue
        }
        KeyCode::Char('a') => {
            app.select_visible(true, plans);
            SelectionControl::Continue
        }
        KeyCode::Char('n') => {
            app.select_visible(false, plans);
            SelectionControl::Continue
        }
        KeyCode::Char('v') => {
            app.toggle_show_all(plans);
            SelectionControl::Continue
        }
        KeyCode::Enter => {
            app.open_version_picker_for_current(plans);
            SelectionControl::Continue
        }
        _ => SelectionControl::Continue,
    }
}

fn handle_version_picker_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    app: &mut SelectionApp,
    plans: &[SelectionPlan],
) -> SelectionControl {
    if modifiers.contains(KeyModifiers::SHIFT) {
        match code {
            KeyCode::Up | KeyCode::Char('K' | 'k') => {
                app.move_version_picker_to_previous_item(plans);
                return SelectionControl::Continue;
            }
            KeyCode::Down | KeyCode::Char('J' | 'j') => {
                app.move_version_picker_to_next_item(plans);
                return SelectionControl::Continue;
            }
            _ => {}
        }
    }

    match code {
        KeyCode::Esc => {
            app.cancel_version_picker();
            SelectionControl::Continue
        }
        KeyCode::Char('K') => {
            app.move_version_picker_to_previous_item(plans);
            SelectionControl::Continue
        }
        KeyCode::Char('J') => {
            app.move_version_picker_to_next_item(plans);
            SelectionControl::Continue
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.move_version_cursor_up(plans);
            SelectionControl::Continue
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.move_version_cursor_down(plans);
            SelectionControl::Continue
        }
        KeyCode::Char('r') => {
            app.choose_recommended_version(plans);
            SelectionControl::Continue
        }
        KeyCode::Enter => {
            app.confirm_version_picker();
            SelectionControl::Continue
        }
        _ => SelectionControl::Continue,
    }
}
