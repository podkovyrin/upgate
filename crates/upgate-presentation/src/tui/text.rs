use ratatui::text::Span;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::outcome::changed_version_segment_index;
use crate::tui::theme::TuiTheme;

const ELLIPSIS: char = '…';

pub(super) fn version_diff_spans(
    from: &str,
    to: &str,
    base_style: ratatui::style::Style,
    theme: &TuiTheme,
    selected: bool,
) -> Vec<Span<'static>> {
    let changed_from = changed_version_segment_index(from, to);
    let changed_style = base_style.patch(theme.version_changed_for(selected));
    let to_core = to.strip_prefix('v').unwrap_or(to);

    let mut spans = Vec::new();
    if to.starts_with('v') {
        spans.push(Span::styled("v", base_style));
    }

    for (idx, part) in to_core.split('.').enumerate() {
        if idx > 0 {
            spans.push(Span::styled(".", base_style));
        }
        let style = if changed_from.is_some_and(|first| idx >= first) {
            changed_style
        } else {
            base_style
        };
        spans.push(Span::styled(part.to_owned(), style));
    }

    spans
}

pub(super) fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_owned();
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

    clipped.push(ELLIPSIS);
    clipped
}
