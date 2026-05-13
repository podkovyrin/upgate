use ratatui::text::{Line, Span};

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
