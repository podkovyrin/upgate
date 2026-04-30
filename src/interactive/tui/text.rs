use ratatui::text::Span;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::theme::TuiTheme;
use crate::util::text::strip_v_prefix;

const ELLIPSIS: char = '…';

pub(super) fn version_diff_spans(
    from: &str,
    to: &str,
    base_style: ratatui::style::Style,
    theme: &TuiTheme,
    selected: bool,
) -> Vec<Span<'static>> {
    let from_core = strip_v_prefix(from);
    let to_core = strip_v_prefix(to);
    let from_parts: Vec<&str> = from_core.split('.').collect();
    let to_parts: Vec<&str> = to_core.split('.').collect();
    let changed_from = first_changed_part_index(&from_parts, &to_parts);
    let changed_style = base_style.patch(theme.version_changed_for(selected));

    let mut spans = Vec::new();
    if to.starts_with('v') {
        spans.push(Span::styled("v".to_string(), base_style));
    }

    for (idx, part) in to_parts.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(".".to_string(), base_style));
        }
        let style = if changed_from.is_some_and(|first| idx >= first) {
            changed_style
        } else {
            base_style
        };
        spans.push(Span::styled((*part).to_string(), style));
    }

    spans
}

pub(super) fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    if UnicodeWidthStr::width(text) <= max_width {
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

    clipped.push(ELLIPSIS);
    clipped
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
