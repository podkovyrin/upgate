use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::tui::theme::TuiTheme;

pub struct KeyBinding<'a> {
    pub key: &'a str,
    pub label: &'a str,
}

// The primary action renders with a "| " separator prefix before its button;
// key_footer and key_footer_hit must agree on this extra width.
fn is_primary_action(binding: &KeyBinding<'_>) -> bool {
    binding.key == "C" && binding.label == "confirm"
}

pub fn key_footer(bindings: &[KeyBinding<'_>], theme: &TuiTheme) -> Line<'static> {
    let mut spans = Vec::new();
    for (idx, binding) in bindings.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw(" "));
        }
        if is_primary_action(binding) {
            spans.push(Span::styled("| ", theme.separator));
            spans.push(Span::styled(
                format!(" {} {} ", binding.key, binding.label),
                theme.primary_keycap,
            ));
        } else {
            spans.push(Span::styled(format!(" {} ", binding.key), theme.keycap));
            spans.push(Span::raw(format!(" {}", binding.label)));
        }
    }
    Line::from(spans)
}

pub fn key_footer_hit(bindings: &[KeyBinding<'_>], column: u16) -> Option<usize> {
    let column = usize::from(column);
    let mut cursor = 0;
    for (idx, binding) in bindings.iter().enumerate() {
        if idx > 0 {
            cursor += 1;
        }
        if is_primary_action(binding) {
            // The "| " separator prefix is not part of the button.
            cursor += 2;
        }
        let button_width =
            UnicodeWidthStr::width(binding.key) + UnicodeWidthStr::width(binding.label) + 3;
        if column >= cursor && column < cursor + button_width {
            return Some(idx);
        }
        cursor += button_width;
    }
    None
}
