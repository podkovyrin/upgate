use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use owo_colors::OwoColorize;
use unicode_width::UnicodeWidthStr;

use super::item::ItemOutcome;
use super::types::{
    AgeGateDiagnostic, DelayedReason, OutcomeReason, OutcomeStatus, OutcomeSubject,
    OutcomeVersions, OutcomeVisibility, SkippedReason,
};
use crate::ui::{OutputTheme, output_theme, with_spinner_suspended};
use crate::util::text::strip_v_prefix;

static OUTCOME_BUFFER: OnceLock<Mutex<Vec<ItemOutcome>>> = OnceLock::new();

fn should_skip_outcome_line(item: &ItemOutcome, theme: OutputTheme) -> bool {
    item.visibility == OutcomeVisibility::VerboseOnly && !theme.verbose
}

fn append_current_age_note(line: &mut String, item: &ItemOutcome, theme: OutputTheme) {
    if !theme.verbose || item.status != OutcomeStatus::Current {
        return;
    }

    let age = item
        .diagnostics
        .release_age
        .as_ref()
        .map(|release_age| (release_age.age.as_str(), release_age.is_old));

    if let Some((age, is_old)) = age {
        line.push(' ');
        line.push_str(&current_age_segment(age, is_old, theme.color()));
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
            append_verbose_detail_note(line, item, theme);
        }
        OutcomeStatus::Update => {
            append_update_note(line, item, theme);
            append_policy_block_note(line, item, theme);
            append_policy_warning_note(line, item, theme);
        }
        OutcomeStatus::Delayed => {
            let note = delayed_note(item, theme);
            line.push(' ');
            line.push_str(&note_segment(&note, theme.color()));
            append_policy_block_note(line, item, theme);
            append_policy_warning_note(line, item, theme);
        }
        OutcomeStatus::Skipped | OutcomeStatus::Error => {
            if let Some(reason) = item.diagnostics.detail.as_deref() {
                line.push(' ');
                line.push_str(&note_segment(&format!("({reason})"), theme.color()));
            }
        }
    }
}

fn append_update_note(line: &mut String, item: &ItemOutcome, theme: OutputTheme) {
    if let Some(latest) = latest_too_fresh(item) {
        let version = latest.version.as_deref().map(version_label);
        let latest_note = if theme.verbose {
            version.map_or_else(
                || {
                    format!(
                        "(latest too fresh: {} < {})",
                        latest.age, latest.required_age
                    )
                },
                |version| {
                    format!(
                        "(latest {version} too fresh: {} < {})",
                        latest.age, latest.required_age
                    )
                },
            )
        } else {
            version.map_or_else(
                || "(latest too fresh)".to_string(),
                |version| format!("(latest {version} too fresh)"),
            )
        };
        line.push(' ');
        line.push_str(&meta_segment(&latest_note, theme.color()));
    }
}

fn append_current_policy_note(line: &mut String, item: &ItemOutcome, theme: OutputTheme) {
    let Some((policy, latest, _warning)) = version_policy_note_parts(item) else {
        return;
    };

    let note = latest.map_or_else(
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
    let Some((policy, Some(latest), _warning)) = version_policy_note_parts(item) else {
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
    let Some((_policy, _latest, Some(warning))) = version_policy_note_parts(item) else {
        return;
    };

    let note = format!("(version policy warning: {warning})");
    line.push(' ');
    line.push_str(&note_segment(&note, theme.color()));
}

fn delayed_note(item: &ItemOutcome, theme: OutputTheme) -> String {
    match item.reason {
        OutcomeReason::Delayed(DelayedReason::NoAgeEligibleRelease) => {
            if let Some(latest) = latest_too_fresh(item) {
                if theme.verbose {
                    return latest.version.as_deref().map_or_else(
                        || {
                            format!(
                                "(no eligible release yet; latest too fresh: {} < {})",
                                latest.age, latest.required_age
                            )
                        },
                        |version| {
                            format!(
                                "(no eligible release yet; latest {} too fresh: {} < {})",
                                version_label(version),
                                latest.age,
                                latest.required_age
                            )
                        },
                    );
                }

                if let Some(version) = latest.version.as_deref() {
                    return format!(
                        "(no eligible release yet; latest {} too fresh)",
                        version_label(version)
                    );
                }
                return "(no eligible release yet; latest too fresh)".to_string();
            }

            let required_age = item
                .diagnostics
                .required_age
                .as_deref()
                .unwrap_or("unknown");
            format!("(no eligible release yet; required age {required_age})")
        }
        OutcomeReason::Delayed(DelayedReason::NoPolicyAndAgeEligibleRelease) => {
            if let Some(latest) = latest_too_fresh(item) {
                if theme.verbose {
                    return latest.version.as_deref().map_or_else(
                        || {
                            format!(
                                "(no eligible release yet; latest too fresh: {} < {})",
                                latest.age, latest.required_age
                            )
                        },
                        |version| {
                            format!(
                                "(no eligible release yet; latest {} too fresh: {} < {})",
                                version_label(version),
                                latest.age,
                                latest.required_age
                            )
                        },
                    );
                }

                if let Some(version) = latest.version.as_deref() {
                    return format!(
                        "(no eligible release yet; latest {} too fresh)",
                        version_label(version)
                    );
                }
            }

            "(no eligible release yet)".to_string()
        }
        OutcomeReason::Delayed(DelayedReason::TargetTooFresh) => target_too_fresh(item)
            .map_or_else(
                || "(too fresh)".to_string(),
                |target| format!("(too fresh: {} < {})", target.age, target.required_age),
            ),
        _ => "(delayed)".to_string(),
    }
}

fn append_verbose_detail_note(line: &mut String, item: &ItemOutcome, theme: OutputTheme) {
    if !theme.verbose {
        return;
    }

    if let Some(detail) = item.diagnostics.detail.as_deref() {
        line.push(' ');
        line.push_str(&meta_segment(&format!("({detail})"), theme.color()));
    }
}

const fn latest_too_fresh(item: &ItemOutcome) -> Option<&AgeGateDiagnostic> {
    item.diagnostics.latest_too_fresh.as_ref()
}

const fn target_too_fresh(item: &ItemOutcome) -> Option<&AgeGateDiagnostic> {
    item.diagnostics.target_too_fresh.as_ref()
}

fn version_policy_note_parts(item: &ItemOutcome) -> Option<(&str, Option<&str>, Option<&str>)> {
    item.diagnostics.version_policy.as_ref().map(|policy| {
        (
            policy.policy.as_str(),
            policy.latest_blocked_version.as_deref(),
            policy.warning.as_deref(),
        )
    })
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

#[derive(Debug, Clone)]
struct TableCell {
    rendered: String,
    plain: String,
}

impl TableCell {
    fn new(rendered: impl Into<String>) -> Self {
        let rendered = rendered.into();
        let plain = strip_ansi_codes(&rendered);
        Self { rendered, plain }
    }

    fn header(label: &'static str, color: bool) -> Self {
        let rendered = if color {
            label.magenta().bold().to_string()
        } else {
            label.to_string()
        };

        Self {
            rendered,
            plain: label.to_string(),
        }
    }

    fn visible_width(&self) -> usize {
        UnicodeWidthStr::width(self.plain.as_str())
    }
}

#[derive(Debug, Clone)]
struct OutcomeTableRow {
    status: TableCell,
    manager: TableCell,
    name: TableCell,
    current: TableCell,
    target: TableCell,
    note: TableCell,
}

impl OutcomeTableRow {
    const fn cell(&self, column: OutcomeTableColumn) -> &TableCell {
        match column {
            OutcomeTableColumn::Status => &self.status,
            OutcomeTableColumn::Manager => &self.manager,
            OutcomeTableColumn::Name => &self.name,
            OutcomeTableColumn::Current => &self.current,
            OutcomeTableColumn::Target => &self.target,
            OutcomeTableColumn::Note => &self.note,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutcomeTableColumn {
    Status,
    Manager,
    Name,
    Current,
    Target,
    Note,
}

impl OutcomeTableColumn {
    fn header(self, color: bool) -> TableCell {
        match self {
            Self::Status => TableCell::header("Status", color),
            Self::Manager => TableCell::header("Manager", color),
            Self::Name => TableCell::header("Name", color),
            Self::Current => TableCell::header("Current", color),
            Self::Target => TableCell::header("Target", color),
            Self::Note => TableCell::header("Note", color),
        }
    }
}

pub fn emit_text_outcome(outcome: &ItemOutcome) {
    lock_outcome_buffer().push(outcome.clone());
}

pub fn flush_text_outcomes() {
    let outcomes = {
        let mut buffer = lock_outcome_buffer();
        if buffer.is_empty() {
            return;
        }
        std::mem::take(&mut *buffer)
    };

    let theme = output_theme();
    let rows: Vec<_> = outcomes
        .iter()
        .filter_map(|outcome| outcome_table_row(outcome, theme))
        .collect();

    if rows.is_empty() {
        return;
    }

    with_spinner_suspended(|| {
        for line in render_outcome_table(&rows, theme.color()) {
            println!("{line}");
        }
    });
}

fn lock_outcome_buffer() -> MutexGuard<'static, Vec<ItemOutcome>> {
    OUTCOME_BUFFER
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn outcome_table_row(item: &ItemOutcome, theme: OutputTheme) -> Option<OutcomeTableRow> {
    if should_skip_outcome_line(item, theme) {
        return None;
    }

    let status = TableCell::new(status_prefix(item.status, theme.color()));
    let manager = TableCell::new(render_manager(item.manager, theme.color()));
    let name = match item.subject {
        OutcomeSubject::Manager => TableCell::new(String::new()),
        OutcomeSubject::Item => TableCell::new(render_name(&item.name, theme.color())),
    };

    let (current, target) = version_cells(item, theme);

    Some(OutcomeTableRow {
        status,
        manager,
        name,
        current,
        target,
        note: TableCell::new(outcome_note(item, theme)),
    })
}

fn version_cells(item: &ItemOutcome, theme: OutputTheme) -> (TableCell, TableCell) {
    match &item.versions {
        OutcomeVersions::None => (TableCell::new(String::new()), TableCell::new(String::new())),
        OutcomeVersions::Current { version } => (
            TableCell::new(version_label(version)),
            TableCell::new(String::new()),
        ),
        OutcomeVersions::Change { from, to } => {
            let from = version_label(from);
            let to = version_label(to);
            let pinned_skip = matches!(item.reason, OutcomeReason::Skipped(SkippedReason::Pinned));
            let from_rendered = render_from_version(&from, theme.color(), pinned_skip);
            let to_rendered = render_to_version(&from, &to, theme.color(), !pinned_skip);
            (TableCell::new(from_rendered), TableCell::new(to_rendered))
        }
    }
}

pub(crate) fn outcome_note(item: &ItemOutcome, theme: OutputTheme) -> String {
    let mut note = String::new();
    append_status_note(&mut note, item, theme);
    append_current_age_note(&mut note, item, theme);
    note.trim_start().to_string()
}

fn render_outcome_table(rows: &[OutcomeTableRow], color: bool) -> Vec<String> {
    let columns = outcome_table_columns(rows);
    let headers: Vec<_> = columns.iter().map(|column| column.header(color)).collect();
    let widths = table_widths(rows, &headers, &columns);

    std::iter::once(render_table_cells(headers.iter(), &widths))
        .chain(
            rows.iter().map(|row| {
                render_table_cells(columns.iter().map(|column| row.cell(*column)), &widths)
            }),
        )
        .collect()
}

fn outcome_table_columns(rows: &[OutcomeTableRow]) -> Vec<OutcomeTableColumn> {
    let mut columns = vec![OutcomeTableColumn::Status, OutcomeTableColumn::Manager];

    if rows.iter().any(|row| !row.name.plain.is_empty()) {
        columns.push(OutcomeTableColumn::Name);
    }
    if rows.iter().any(|row| !row.current.plain.is_empty()) {
        columns.push(OutcomeTableColumn::Current);
    }
    if rows.iter().any(|row| !row.target.plain.is_empty()) {
        columns.push(OutcomeTableColumn::Target);
    }
    if rows.iter().any(|row| !row.note.plain.is_empty()) {
        columns.push(OutcomeTableColumn::Note);
    }

    columns
}

fn table_widths(
    rows: &[OutcomeTableRow],
    headers: &[TableCell],
    columns: &[OutcomeTableColumn],
) -> Vec<usize> {
    columns
        .iter()
        .zip(headers)
        .map(|(column, header)| {
            rows.iter()
                .map(|row| row.cell(*column).visible_width())
                .max()
                .unwrap_or(0)
                .max(header.visible_width())
        })
        .collect()
}

fn render_table_cells<'a, I>(cells: I, widths: &[usize]) -> String
where
    I: IntoIterator<Item = &'a TableCell>,
{
    let mut line = String::new();

    for (idx, (cell, width)) in cells.into_iter().zip(widths).enumerate() {
        if idx > 0 {
            line.push_str("  ");
        }

        line.push_str(&cell.rendered);

        if idx + 1 < widths.len() {
            let padding = width.saturating_sub(cell.visible_width());
            line.push_str(&" ".repeat(padding));
        }
    }

    line
}

fn strip_ansi_codes(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            for code_ch in chars.by_ref() {
                if code_ch.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }

        stripped.push(ch);
    }

    stripped
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
    use crate::outcome::ReasonCode;

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
        item.set_version_policy("stable", Some("1.3.0-beta.1".to_string()), None);

        let note = outcome_note(&item, crate::ui::OutputTheme::test_plain(false));
        assert!(note.contains("blocked by version policy: stable"));
        assert!(note.contains("v1.3.0-beta.1"));
    }

    #[test]
    fn current_with_policy_note_without_blocked_latest_uses_generic_text() {
        let mut item = ItemOutcome::current("pipx", "bar", "2.0.0rc1");
        item.set_version_policy("stable", None, None);

        let note = outcome_note(&item, crate::ui::OutputTheme::test_plain(false));
        assert!(note.contains("newer versions blocked by version policy: stable"));
    }

    #[test]
    fn update_with_policy_note_includes_blocked_latest() {
        let mut item = ItemOutcome::update("npm", "baz", "1.2.0", "1.2.5");
        item.set_version_policy("stable", Some("1.3.0-beta.1".to_string()), None);

        let note = outcome_note(&item, crate::ui::OutputTheme::test_plain(false));
        assert!(note.contains("blocked by version policy: stable"));
        assert!(note.contains("v1.3.0-beta.1"));
    }

    #[test]
    fn delayed_with_policy_note_includes_blocked_latest() {
        let mut item = ItemOutcome::delayed_too_fresh("npm", "qux", "3.1.0", "3.1.1", "3d", "7d");
        item.set_version_policy("stable", Some("4.0.0-beta.2".to_string()), None);

        let note = outcome_note(&item, crate::ui::OutputTheme::test_plain(false));
        assert!(note.contains("(too fresh: 3d < 7d)"));
        assert!(note.contains("blocked by version policy: stable"));
        assert!(note.contains("v4.0.0-beta.2"));
    }

    #[test]
    fn policy_warning_note_is_rendered_when_present() {
        let mut item = ItemOutcome::current("npm", "foo", "1.2.0");
        item.set_version_policy(
            "same-track",
            None,
            Some("same-track fell back to stable because installed track is unknown".to_string()),
        );

        let note = outcome_note(&item, crate::ui::OutputTheme::test_plain(false));
        assert!(note.contains("version policy warning"));
        assert!(note.contains("same-track fell back to stable"));
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

        let table = render_single_plain_table(&item, false);
        assert!(table.contains("- Skipped  [mise]    nometa-tool  v1.0.0  v1.1.0"));
        assert!(table.contains("(no publish-date metadata)"));
    }

    #[test]
    fn update_with_delayed_latest_shows_short_normal_note_and_verbose_age_evidence() {
        let item = ItemOutcome::update_with_delayed_latest(
            "npm", "foo", "1.2.0", "1.2.5", "1.3.0", "3d", "7d",
        );

        let normal = render_single_plain_table(&item, false);
        assert!(normal.contains("+ Update  [npm]    foo  v1.2.0  v1.2.5"));
        assert!(normal.contains("(latest v1.3.0 too fresh)"));
        assert!(!normal.contains("3d < 7d"));

        let verbose_note = outcome_note(&item, crate::ui::OutputTheme::test_plain(true));
        assert!(verbose_note.contains("(latest v1.3.0 too fresh: 3d < 7d)"));
    }

    #[test]
    fn manager_level_skip_omits_placeholder_versions() {
        let item = ItemOutcome::skipped(
            "cargo",
            "*",
            "*",
            "*",
            ReasonCode::MissingCommand,
            "required command 'cargo' is not available",
        );

        let rendered = render_single_plain_table(&item, false);
        assert_eq!(
            rendered,
            "Status     Manager  Note\n- Skipped  [cargo]  (required command 'cargo' is not available)"
        );
    }

    #[test]
    fn delayed_no_eligible_hides_age_evidence_until_verbose() {
        let item = ItemOutcome::delayed_no_eligible_with_latest(
            "npm", "foo", "1.2.0", "1.3.0", "3d", "7d",
        );

        let normal = render_single_plain_table(&item, false);
        assert!(normal.contains("~ Delayed  [npm]    foo  v1.2.0  v1.3.0"));
        assert!(normal.contains("(no eligible release yet; latest v1.3.0 too fresh)"));
        assert!(!normal.contains("3d < 7d"));

        let verbose_note = outcome_note(&item, crate::ui::OutputTheme::test_plain(true));
        assert!(
            verbose_note.contains("(no eligible release yet; latest v1.3.0 too fresh: 3d < 7d)")
        );
    }

    #[test]
    fn no_change_is_current_and_hidden_until_verbose() {
        let item = ItemOutcome::current_no_newer("npm", "foo", "1.2.0");

        assert!(outcome_table_row(&item, crate::ui::OutputTheme::test_plain(false)).is_none());

        let rendered = render_single_plain_table(&item, true);
        assert!(rendered.contains("= Current  [npm]    foo  v1.2.0"));
        assert!(rendered.contains("(no newer version found)"));
    }

    #[test]
    fn outcome_table_aligns_columns_and_omits_version_arrows() {
        let theme = crate::ui::OutputTheme::test_plain(false);
        let items = [
            ItemOutcome::update("npm", "short", "1.0.0", "1.2.0"),
            ItemOutcome::update("npm", "much-longer-name", "2.0.0", "2.1.0"),
        ];
        let rows: Vec<_> = items
            .iter()
            .filter_map(|item| outcome_table_row(item, theme))
            .collect();

        let table = render_outcome_table(&rows, false);

        assert_eq!(
            table[0],
            "Status    Manager  Name              Current  Target"
        );
        let ascii_arrow: String = ['-', '>'].iter().collect();
        assert!(!table.join("\n").contains(&ascii_arrow));
        assert_eq!(table[1].find("v1.0.0"), table[2].find("v2.0.0"));
        assert_eq!(table[1].find("v1.2.0"), table[2].find("v2.1.0"));
    }

    #[test]
    fn outcome_table_header_is_magenta_when_color_is_enabled() {
        let theme = crate::ui::OutputTheme::test_plain(false);
        let item = ItemOutcome::update("npm", "foo", "1.0.0", "1.2.0");
        let rows = std::iter::once(&item)
            .filter_map(|item| outcome_table_row(item, theme))
            .collect::<Vec<_>>();

        let table = render_outcome_table(&rows, true);

        assert!(table[0].contains("\u{1b}[35m"));
        assert!(table[0].contains("\u{1b}[1m"));
    }

    fn render_single_plain_table(item: &ItemOutcome, verbose: bool) -> String {
        let theme = crate::ui::OutputTheme::test_plain(verbose);
        let row = outcome_table_row(item, theme).expect("row should render");
        render_outcome_table(&[row], false).join("\n")
    }
}
