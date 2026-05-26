use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::tui::theme::TuiTheme;

pub struct KeyBinding<'a> {
    pub key: &'a str,
    pub label: &'a str,
}

pub fn key_footer(bindings: &[KeyBinding<'_>], theme: &TuiTheme) -> Line<'static> {
    let mut spans = Vec::new();
    for (idx, binding) in bindings.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(format!(" {} ", binding.key), theme.keycap));
        spans.push(Span::raw(format!(" {}", binding.label)));
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
        let button_width =
            UnicodeWidthStr::width(binding.key) + UnicodeWidthStr::width(binding.label) + 3;
        if column >= cursor && column < cursor + button_width {
            return Some(idx);
        }
        cursor += button_width;
    }
    None
}
