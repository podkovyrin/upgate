use owo_colors::OwoColorize;

use super::ReasonCode;
use super::item::ItemOutcome;
use super::types::OutcomeStatus;
use crate::ui::{OutputTheme, output_theme, with_spinner_suspended};
use crate::util::text::strip_v_prefix;

impl ItemOutcome {
    pub fn to_text_line(&self) -> Option<String> {
        let theme = output_theme();

        if should_skip_outcome_line(self) {
            return None;
        }

        let mut line = base_outcome_line(self, theme);
        append_status_note(&mut line, self, theme);
        append_current_age_note(&mut line, self, theme);

        Some(line)
    }
}

fn should_skip_outcome_line(item: &ItemOutcome) -> bool {
    if item.status != OutcomeStatus::Skipped {
        return false;
    }

    item.reason_code == Some(ReasonCode::NoChange)
}

fn base_outcome_line(item: &ItemOutcome, theme: OutputTheme) -> String {
    let manager_rendered = render_manager(item.manager, theme.color());
    let name_rendered = render_name(&item.name, theme.color());
    let from = version_label(&item.from_version);

    if item.status == OutcomeStatus::Current {
        return format!(
            "{} {} {} {}",
            status_prefix(item.status, theme.color()),
            manager_rendered,
            name_rendered,
            from
        );
    }

    let arrow = if theme.unicode() { "→" } else { "->" };
    let to = version_label(&item.to_version);
    let pinned_skip =
        item.status == OutcomeStatus::Skipped && item.reason_code == Some(ReasonCode::Pinned);
    let from_rendered = render_from_version(&from, theme.color(), pinned_skip);
    let to_rendered = render_to_version(&from, &to, theme.color(), !pinned_skip);

    format!(
        "{} {} {} {} {arrow} {}",
        status_prefix(item.status, theme.color()),
        manager_rendered,
        name_rendered,
        from_rendered,
        to_rendered
    )
}

fn append_current_age_note(line: &mut String, item: &ItemOutcome, theme: OutputTheme) {
    if !theme.verbose || item.status != OutcomeStatus::Current {
        return;
    }

    if let Some(age) = item.scan_age.as_deref() {
        line.push(' ');
        line.push_str(&current_age_segment(age, item.scan_is_old, theme.color()));
    }
}

fn render_manager(manager: &str, color: bool) -> String {
    if color {
        format!("[{}]", manager.bold())
    } else {
        format!("[{manager}]")
    }
}

fn render_name(name: &str, color: bool) -> String {
    if color {
        name.bold().to_string()
    } else {
        name.to_string()
    }
}

fn append_status_note(line: &mut String, item: &ItemOutcome, theme: OutputTheme) {
    match item.status {
        OutcomeStatus::Current => {
            append_current_policy_note(line, item, theme);
            append_policy_warning_note(line, item, theme);
        }
        OutcomeStatus::Update => {
            append_update_note(line, item, theme);
            append_policy_block_note(line, item, theme);
            append_policy_warning_note(line, item, theme);
        }
        OutcomeStatus::Delayed => {
            let note = delayed_note(item);
            line.push(' ');
            line.push_str(&note_segment(&note, theme.color()));
            append_policy_block_note(line, item, theme);
            append_policy_warning_note(line, item, theme);
        }
        OutcomeStatus::Skipped | OutcomeStatus::Error => {
            if let Some(reason) = &item.reason_detail {
                line.push(' ');
                line.push_str(&note_segment(&format!("({reason})"), theme.color()));
            }
        }
    }
}

fn append_update_note(line: &mut String, item: &ItemOutcome, theme: OutputTheme) {
    if !theme.verbose {
        return;
    }

    if let (Some(latest), Some(latest_age), Some(required_age)) = (
        item.latest_version.as_deref(),
        item.latest_age.as_deref(),
        item.required_age.as_deref(),
    ) {
        let latest_note = format!(
            "(latest {} delayed: {} < {})",
            version_label(latest),
            latest_age,
            required_age
        );
        line.push(' ');
        line.push_str(&meta_segment(&latest_note, theme.color()));
    }
}

fn append_current_policy_note(line: &mut String, item: &ItemOutcome, theme: OutputTheme) {
    let Some(policy) = item.version_policy.as_deref() else {
        return;
    };

    let note = item
        .latest_blocked_by_policy_version
        .as_deref()
        .map_or_else(
            || format!("(newer versions blocked by version policy: {policy})"),
            |latest| {
                format!(
                    "(latest {} blocked by version policy: {policy})",
                    version_label(latest)
                )
            },
        );

    line.push(' ');
    line.push_str(&note_segment(&note, theme.color()));
}

fn append_policy_block_note(line: &mut String, item: &ItemOutcome, theme: OutputTheme) {
    let Some(policy) = item.version_policy.as_deref() else {
        return;
    };
    let Some(latest) = item.latest_blocked_by_policy_version.as_deref() else {
        return;
    };

    let note = format!(
        "(latest {} blocked by version policy: {policy})",
        version_label(latest)
    );
    line.push(' ');
    line.push_str(&note_segment(&note, theme.color()));
}

fn append_policy_warning_note(line: &mut String, item: &ItemOutcome, theme: OutputTheme) {
    let Some(warning) = item.version_policy_warning.as_deref() else {
        return;
    };

    let note = format!("(version policy warning: {warning})");
    line.push(' ');
    line.push_str(&note_segment(&note, theme.color()));
}

fn delayed_note(item: &ItemOutcome) -> String {
    if item.reason_code == Some(ReasonCode::NoEligibleRelease) {
        let required_age = item.required_age.as_deref().unwrap_or("unknown");
        if let Some(latest_age) = item.latest_age.as_deref() {
            return format!("(latest too fresh: {latest_age} < {required_age})");
        }

        return format!("(no eligible release >= current within {required_age} window)");
    }

    if let (Some(age), Some(required_age)) = (item.age.as_deref(), item.required_age.as_deref()) {
        return format!("({age} < {required_age})");
    }

    "(delayed)".to_string()
}

fn render_from_version(version: &str, color: bool, emphasize: bool) -> String {
    if color && emphasize {
        return version.bold().to_string();
    }

    version.to_string()
}

pub fn render_to_version(from: &str, to: &str, color: bool, emphasize: bool) -> String {
    if !color {
        return to.to_string();
    }

    let from_core = strip_v_prefix(from);
    let to_core = strip_v_prefix(to);

    let from_parts: Vec<&str> = from_core.split('.').collect();
    let to_parts: Vec<&str> = to_core.split('.').collect();

    let changed_from = first_changed_part_index(&from_parts, &to_parts);

    let mut out = String::new();
    if to.starts_with('v') {
        out.push_str(&style_version_part("v", false, emphasize));
    }

    for (idx, part) in to_parts.iter().enumerate() {
        if idx > 0 {
            out.push('.');
        }

        let changed = changed_from.is_some_and(|first| idx >= first);
        out.push_str(&style_version_part(part, changed, emphasize));
    }

    out
}

fn style_version_part(part: &str, changed: bool, emphasize: bool) -> String {
    if changed {
        if emphasize {
            return part.blue().bold().to_string();
        }
        return part.blue().to_string();
    }

    if emphasize {
        return part.bold().to_string();
    }

    part.to_string()
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

fn status_prefix(status: OutcomeStatus, color: bool) -> String {
    match status {
        OutcomeStatus::Current => {
            if color {
                format!("{} {}", "=".cyan().bold(), "Current".cyan().bold())
            } else {
                "= Current".to_string()
            }
        }
        OutcomeStatus::Update => {
            if color {
                format!("{} {}", "+".green().bold(), "Update".green().bold())
            } else {
                "+ Update".to_string()
            }
        }
        OutcomeStatus::Delayed => {
            if color {
                format!("{} {}", "~".yellow().bold(), "Delayed".yellow().bold())
            } else {
                "~ Delayed".to_string()
            }
        }
        OutcomeStatus::Skipped => {
            if color {
                format!("{} {}", "-".blue().bold(), "Skipped".blue().bold())
            } else {
                "- Skipped".to_string()
            }
        }
        OutcomeStatus::Error => {
            if color {
                format!("{} {}", "!".red().bold(), "Error".red().bold())
            } else {
                "! Error".to_string()
            }
        }
    }
}

fn meta_segment(text: &str, color: bool) -> String {
    if color {
        text.dimmed().to_string()
    } else {
        text.to_string()
    }
}

fn current_age_segment(age: &str, is_old: bool, color: bool) -> String {
    if !color {
        return format!("(released: {age})");
    }

    if is_old {
        format!("(released: {})", age.red().bold())
    } else {
        format!("(released: {age})").dimmed().to_string()
    }
}

fn note_segment(text: &str, color: bool) -> String {
    if color {
        text.italic().to_string()
    } else {
        text.to_string()
    }
}

pub fn emit_text_outcome(outcome: &ItemOutcome) {
    if let Some(line) = outcome.to_text_line() {
        with_spinner_suspended(|| {
            println!("{line}");
        });
    }
}

pub fn version_label(version: &str) -> String {
    if version.starts_with('v') {
        return version.to_string();
    }

    match version.chars().next() {
        Some(c) if c.is_ascii_digit() => format!("v{version}"),
        _ => version.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_part_index_none_when_equal() {
        let from = ["1", "2", "3"];
        let to = ["1", "2", "3"];
        assert_eq!(first_changed_part_index(&from, &to), None);
    }

    #[test]
    fn changed_part_index_middle_when_minor_differs() {
        let from = ["1", "2", "0"];
        let to = ["1", "3", "0"];
        assert_eq!(first_changed_part_index(&from, &to), Some(1));
    }

    #[test]
    fn changed_part_index_zero_when_major_differs() {
        let from = ["1", "9", "9"];
        let to = ["2", "0", "0"];
        assert_eq!(first_changed_part_index(&from, &to), Some(0));
    }

    #[test]
    fn changed_part_index_new_suffix_part() {
        let from = ["1", "2"];
        let to = ["1", "2", "1"];
        assert_eq!(first_changed_part_index(&from, &to), Some(2));
    }

    #[test]
    fn render_to_version_plain_mode_is_identity() {
        let rendered = render_to_version("v1.2.3", "v1.3.0", false, false);
        assert_eq!(rendered, "v1.3.0");
    }

    #[test]
    fn render_to_version_color_mode_contains_all_digits_when_equal() {
        let rendered = render_to_version("v1.2.3", "v1.2.3", true, false);
        assert!(rendered.contains('1'));
        assert!(rendered.contains('2'));
        assert!(rendered.contains('3'));
    }

    #[test]
    fn render_from_version_emphasizes_when_requested() {
        let rendered = render_from_version("v1.2.3", true, true);
        assert!(rendered.contains("\u{1b}[1m"));
    }

    #[test]
    fn render_to_version_non_bold_highlights_changed_segments() {
        let rendered = render_to_version("v1.2.3", "v1.3.0", true, false);
        assert!(rendered.contains("\u{1b}[34m"));
        assert!(!rendered.contains("\u{1b}[1m"));
    }

    #[test]
    fn current_with_policy_note_includes_blocked_latest() {
        let mut item = ItemOutcome::current("npm", "foo", "1.2.0");
        item.version_policy = Some("stable".to_string());
        item.latest_blocked_by_policy_version = Some("1.3.0-beta.1".to_string());

        let rendered = item.to_text_line().expect("line should render");
        assert!(rendered.contains("blocked by version policy: stable"));
        assert!(rendered.contains("v1.3.0-beta.1"));
    }

    #[test]
    fn current_with_policy_note_without_blocked_latest_uses_generic_text() {
        let mut item = ItemOutcome::current("pipx", "bar", "2.0.0rc1");
        item.version_policy = Some("stable".to_string());

        let rendered = item.to_text_line().expect("line should render");
        assert!(rendered.contains("newer versions blocked by version policy: stable"));
    }

    #[test]
    fn update_with_policy_note_includes_blocked_latest() {
        let mut item = ItemOutcome::update("npm", "baz", "1.2.0", "1.2.5");
        item.version_policy = Some("stable".to_string());
        item.latest_blocked_by_policy_version = Some("1.3.0-beta.1".to_string());

        let rendered = item.to_text_line().expect("line should render");
        assert!(rendered.contains("blocked by version policy: stable"));
        assert!(rendered.contains("v1.3.0-beta.1"));
    }

    #[test]
    fn delayed_with_policy_note_includes_blocked_latest() {
        let mut item = ItemOutcome::delayed_too_fresh("npm", "qux", "3.1.0", "3.1.1", "3d", "7d");
        item.version_policy = Some("stable".to_string());
        item.latest_blocked_by_policy_version = Some("4.0.0-beta.2".to_string());

        let rendered = item.to_text_line().expect("line should render");
        assert!(rendered.contains("(3d < 7d)"));
        assert!(rendered.contains("blocked by version policy: stable"));
        assert!(rendered.contains("v4.0.0-beta.2"));
    }

    #[test]
    fn policy_warning_note_is_rendered_when_present() {
        let mut item = ItemOutcome::current("npm", "foo", "1.2.0");
        item.version_policy = Some("same-track".to_string());
        item.version_policy_warning =
            Some("same-track fell back to stable because installed track is unknown".to_string());

        let rendered = item.to_text_line().expect("line should render");
        assert!(rendered.contains("version policy warning"));
        assert!(rendered.contains("same-track fell back to stable"));
    }

    #[test]
    fn skipped_missing_metadata_is_visible_without_verbose() {
        let item = ItemOutcome::skipped(
            "mise",
            "nometa-tool",
            "1.0.0",
            "1.1.0",
            ReasonCode::MissingMetadata,
            "no publish-date metadata",
        );

        let rendered = item.to_text_line().expect("line should render");
        assert!(rendered.contains("- Skipped [mise] nometa-tool v1.0.0 -> v1.1.0"));
        assert!(rendered.contains("(no publish-date metadata)"));
    }
}
