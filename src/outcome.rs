use crate::ui::{output_theme, with_spinner_suspended};
use owo_colors::OwoColorize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutcomeStatus {
    Update,
    Delayed,
    Skipped,
    Error,
}

pub(crate) const REASON_TOO_FRESH: &str = "too_fresh";
pub(crate) const REASON_NO_ELIGIBLE_RELEASE: &str = "no_eligible_release";
pub(crate) const REASON_NO_CHANGE: &str = "no_change";
pub(crate) const REASON_PINNED: &str = "pinned";
pub(crate) const REASON_MISSING_METADATA: &str = "missing_metadata";
pub(crate) const REASON_COMMAND_FAILED: &str = "command_failed";

#[derive(Debug, Clone)]
pub(crate) struct ItemOutcome {
    pub(crate) manager: &'static str,
    pub(crate) name: String,
    pub(crate) from_version: String,
    pub(crate) to_version: String,
    pub(crate) status: OutcomeStatus,
    pub(crate) source: String,
    pub(crate) reason_code: Option<&'static str>,
    pub(crate) reason_detail: Option<String>,
    pub(crate) age: Option<String>,
    pub(crate) required_age: Option<String>,
    pub(crate) latest_version: Option<String>,
    pub(crate) latest_age: Option<String>,
}

impl ItemOutcome {
    pub(crate) fn update(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            name: name.into(),
            from_version: from_version.into(),
            to_version: to_version.into(),
            status: OutcomeStatus::Update,
            source: source.into(),
            reason_code: None,
            reason_detail: None,
            age: None,
            required_age: None,
            latest_version: None,
            latest_age: None,
        }
    }

    pub(crate) fn update_with_delayed_latest(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        source: impl Into<String>,
        latest_version: impl Into<String>,
        latest_age: impl Into<String>,
        required_age: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            name: name.into(),
            from_version: from_version.into(),
            to_version: to_version.into(),
            status: OutcomeStatus::Update,
            source: source.into(),
            reason_code: Some(REASON_TOO_FRESH),
            reason_detail: None,
            age: None,
            required_age: Some(required_age.into()),
            latest_version: Some(latest_version.into()),
            latest_age: Some(latest_age.into()),
        }
    }

    pub(crate) fn delayed_no_eligible(
        manager: &'static str,
        name: impl Into<String>,
        version: impl Into<String>,
        source: impl Into<String>,
        required_age: impl Into<String>,
    ) -> Self {
        let version = version.into();
        Self {
            manager,
            name: name.into(),
            from_version: version.clone(),
            to_version: version,
            status: OutcomeStatus::Delayed,
            source: source.into(),
            reason_code: Some(REASON_NO_ELIGIBLE_RELEASE),
            reason_detail: None,
            age: None,
            required_age: Some(required_age.into()),
            latest_version: None,
            latest_age: None,
        }
    }

    pub(crate) fn delayed_too_fresh(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        source: impl Into<String>,
        age: impl Into<String>,
        required_age: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            name: name.into(),
            from_version: from_version.into(),
            to_version: to_version.into(),
            status: OutcomeStatus::Delayed,
            source: source.into(),
            reason_code: Some(REASON_TOO_FRESH),
            reason_detail: None,
            age: Some(age.into()),
            required_age: Some(required_age.into()),
            latest_version: None,
            latest_age: None,
        }
    }

    pub(crate) fn skipped(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        source: impl Into<String>,
        reason_code: &'static str,
        reason_detail: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            name: name.into(),
            from_version: from_version.into(),
            to_version: to_version.into(),
            status: OutcomeStatus::Skipped,
            source: source.into(),
            reason_code: Some(reason_code),
            reason_detail: Some(reason_detail.into()),
            age: None,
            required_age: None,
            latest_version: None,
            latest_age: None,
        }
    }

    pub(crate) fn skipped_no_change(
        manager: &'static str,
        name: impl Into<String>,
        version: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        let version = version.into();
        Self {
            manager,
            name: name.into(),
            from_version: version.clone(),
            to_version: version,
            status: OutcomeStatus::Skipped,
            source: source.into(),
            reason_code: Some(REASON_NO_CHANGE),
            reason_detail: Some("already at selected target".to_string()),
            age: None,
            required_age: None,
            latest_version: None,
            latest_age: None,
        }
    }

    pub(crate) fn error(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        source: impl Into<String>,
        reason_code: &'static str,
        reason_detail: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            name: name.into(),
            from_version: from_version.into(),
            to_version: to_version.into(),
            status: OutcomeStatus::Error,
            source: source.into(),
            reason_code: Some(reason_code),
            reason_detail: Some(reason_detail.into()),
            age: None,
            required_age: None,
            latest_version: None,
            latest_age: None,
        }
    }

    pub(crate) fn to_text_line(&self) -> Option<String> {
        if self.status == OutcomeStatus::Skipped && self.reason_code == Some(REASON_NO_CHANGE) {
            return None;
        }

        let theme = output_theme();
        let arrow = if theme.unicode() { "→" } else { "->" };

        let manager_rendered = if theme.color() {
            format!("[{}]", self.manager.bold())
        } else {
            format!("[{}]", self.manager)
        };

        let name_rendered = if theme.color() {
            self.name.bold().to_string()
        } else {
            self.name.clone()
        };

        let from = version_label(&self.from_version);
        let to = version_label(&self.to_version);
        let to_rendered = render_to_version(&from, &to, theme.color());

        let mut line = format!(
            "{} {} {} {} {arrow} {}",
            status_prefix(self.status, theme.color()),
            manager_rendered,
            name_rendered,
            from,
            to_rendered
        );

        append_status_note(&mut line, self, theme);

        if theme.verbose {
            line.push(' ');
            line.push_str(&meta_segment(
                &format!("(source: {})", self.source),
                theme.color(),
            ));
        }

        Some(line)
    }
}

fn append_status_note(line: &mut String, item: &ItemOutcome, theme: crate::ui::OutputTheme) {
    match item.status {
        OutcomeStatus::Update => {
            if theme.verbose
                && let (Some(latest), Some(latest_age), Some(required_age)) = (
                    item.latest_version.as_deref(),
                    item.latest_age.as_deref(),
                    item.required_age.as_deref(),
                )
            {
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
        OutcomeStatus::Delayed => {
            let note = delayed_note(item);
            line.push(' ');
            line.push_str(&note_segment(&note, theme.color()));
        }
        OutcomeStatus::Skipped | OutcomeStatus::Error => {
            if let Some(reason) = &item.reason_detail {
                line.push(' ');
                line.push_str(&note_segment(&format!("({reason})"), theme.color()));
            }
        }
    }
}

fn delayed_note(item: &ItemOutcome) -> String {
    if item.reason_code == Some(REASON_NO_ELIGIBLE_RELEASE) {
        let required_age = item.required_age.as_deref().unwrap_or("unknown");
        return format!("(no eligible release >= current within {required_age} window)");
    }

    if let (Some(age), Some(required_age)) = (item.age.as_deref(), item.required_age.as_deref()) {
        return format!("({age} < {required_age})");
    }

    "(delayed)".to_string()
}

fn render_to_version(from: &str, to: &str, color: bool) -> String {
    if !color {
        return to.to_string();
    }

    let from_core = from.strip_prefix('v').unwrap_or(from);
    let to_core = to.strip_prefix('v').unwrap_or(to);

    let from_parts: Vec<&str> = from_core.split('.').collect();
    let to_parts: Vec<&str> = to_core.split('.').collect();

    if to_parts.is_empty() {
        return to.bold().to_string();
    }

    let changed_from = first_changed_part_index(&from_parts, &to_parts);

    let mut out = String::new();
    if to.starts_with('v') {
        out.push_str(&"v".bold().to_string());
    }

    for (idx, part) in to_parts.iter().enumerate() {
        if idx > 0 {
            out.push('.');
        }

        if let Some(changed_from) = changed_from {
            if idx >= changed_from {
                out.push_str(&part.blue().bold().to_string());
            } else {
                out.push_str(&part.bold().to_string());
            }
        } else {
            out.push_str(&part.bold().to_string());
        }
    }

    out
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

fn note_segment(text: &str, color: bool) -> String {
    if color {
        text.italic().to_string()
    } else {
        text.to_string()
    }
}

pub(crate) fn emit_text_outcome(outcome: &ItemOutcome) {
    if let Some(line) = outcome.to_text_line() {
        with_spinner_suspended(|| {
            println!("{line}");
        });
    }
}

pub(crate) fn version_label(version: &str) -> String {
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
    use super::{first_changed_part_index, render_to_version};

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
        let rendered = render_to_version("v1.2.3", "v1.3.0", false);
        assert_eq!(rendered, "v1.3.0");
    }

    #[test]
    fn render_to_version_color_mode_contains_all_digits_when_equal() {
        let rendered = render_to_version("v1.2.3", "v1.2.3", true);
        assert!(rendered.contains('1'));
        assert!(rendered.contains('2'));
        assert!(rendered.contains('3'));
    }
}
