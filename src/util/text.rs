pub fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

pub fn strip_v_prefix(text: &str) -> &str {
    text.strip_prefix('v').unwrap_or(text)
}

pub fn trim_non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

pub const fn read_non_empty<'a>(first: &'a str, second: &'a str) -> &'a str {
    if first.is_empty() { second } else { first }
}
