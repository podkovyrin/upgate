use owo_colors::OwoColorize;
use unicode_width::UnicodeWidthStr;
use upnow_domain::{ManagerId, PackageName, VersionText};

use crate::theme::OutputTheme;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutcomeTable {
    pub rows: Vec<OutcomeRow>,
}

impl OutcomeTable {
    pub const fn new(rows: Vec<OutcomeRow>) -> Self {
        Self { rows }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeRow {
    pub status: OutcomeStatusView,
    pub manager_id: ManagerId,
    pub subject: OutcomeSubjectView,
    pub versions: OutcomeVersionsView,
    pub notes: Vec<OutcomeNote>,
    pub visibility: OutcomeVisibility,
}

impl OutcomeRow {
    pub const fn item(
        status: OutcomeStatusView,
        manager_id: ManagerId,
        package_name: PackageName,
        versions: OutcomeVersionsView,
    ) -> Self {
        Self {
            status,
            manager_id,
            subject: OutcomeSubjectView::Item { package_name },
            versions,
            notes: Vec::new(),
            visibility: OutcomeVisibility::Always,
        }
    }
    pub const fn manager(status: OutcomeStatusView, manager_id: ManagerId) -> Self {
        Self {
            status,
            manager_id,
            subject: OutcomeSubjectView::Manager,
            versions: OutcomeVersionsView::None,
            notes: Vec::new(),
            visibility: OutcomeVisibility::Always,
        }
    }
    pub fn with_note(mut self, note: OutcomeNote) -> Self {
        self.notes.push(note);
        self
    }
    pub const fn with_visibility(mut self, visibility: OutcomeVisibility) -> Self {
        self.visibility = visibility;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeStatusView {
    Current,
    Update,
    Delayed,
    Blocked,
    Skipped,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutcomeSubjectView {
    Manager,
    Item { package_name: PackageName },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutcomeVersionsView {
    None,
    Current {
        version: VersionText,
    },
    Change {
        from: VersionText,
        to: OutcomeTargetView,
        emphasis: OutcomeVersionEmphasis,
    },
}

impl OutcomeVersionsView {
    pub const fn change(from: VersionText, to: VersionText) -> Self {
        Self::Change {
            from,
            to: OutcomeTargetView::Known(to),
            emphasis: OutcomeVersionEmphasis::Target,
        }
    }
    pub const fn manager_resolved(from: VersionText) -> Self {
        Self::Change {
            from,
            to: OutcomeTargetView::ManagerResolved,
            emphasis: OutcomeVersionEmphasis::Target,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutcomeTargetView {
    Known(VersionText),
    ManagerResolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeVersionEmphasis {
    None,
    Current,
    Target,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeNote {
    pub text: String,
    pub visibility: OutcomeVisibility,
}

impl OutcomeNote {
    pub fn normal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            visibility: OutcomeVisibility::Always,
        }
    }
    pub fn metadata(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            visibility: OutcomeVisibility::Always,
        }
    }
    pub fn emphasized(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            visibility: OutcomeVisibility::Always,
        }
    }
    pub fn warning(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            visibility: OutcomeVisibility::Always,
        }
    }
    pub const fn with_visibility(mut self, visibility: OutcomeVisibility) -> Self {
        self.visibility = visibility;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeVisibility {
    Always,
    VerboseOnly,
}
pub fn render_outcome_table(table: &OutcomeTable, theme: OutputTheme) -> String {
    let rows = table
        .rows
        .iter()
        .filter(|row| row.visibility == OutcomeVisibility::Always || theme.verbose)
        .map(|row| outcome_table_row(row, theme))
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return String::new();
    }

    render_table_rows(&rows, theme.color()).join("\n")
}
pub fn version_label(version: &str) -> String {
    if version.starts_with('v') {
        return version.to_owned();
    }

    match version.chars().next() {
        Some(ch) if ch.is_ascii_digit() => format!("v{version}"),
        _ => version.to_owned(),
    }
}
pub fn changed_version_segment_index(from: &str, to: &str) -> Option<usize> {
    let from_core = strip_v_prefix(from);
    let to_core = strip_v_prefix(to);
    let from_parts = from_core.split('.').collect::<Vec<_>>();
    let to_parts = to_core.split('.').collect::<Vec<_>>();

    first_changed_part_index(&from_parts, &to_parts)
}
pub fn render_to_version(from: &str, to: &str, theme: OutputTheme, emphasize: bool) -> String {
    if !theme.color() {
        return to.to_owned();
    }

    let from_core = strip_v_prefix(from);
    let to_core = strip_v_prefix(to);
    let from_parts = from_core.split('.').collect::<Vec<_>>();
    let to_parts = to_core.split('.').collect::<Vec<_>>();
    let changed_from = first_changed_part_index(&from_parts, &to_parts);

    let mut output = String::new();
    if to.starts_with('v') {
        output.push_str(&style_version_part("v", false, emphasize));
    }

    for (index, part) in to_parts.iter().enumerate() {
        if index > 0 {
            output.push('.');
        }

        let changed = changed_from.is_some_and(|first| index >= first);
        output.push_str(&style_version_part(part, changed, emphasize));
    }

    output
}
pub fn strip_ansi_codes(text: &str) -> String {
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

fn outcome_table_row(row: &OutcomeRow, theme: OutputTheme) -> RenderedOutcomeTableRow {
    let name = match &row.subject {
        OutcomeSubjectView::Manager => TableCell::new(String::new()),
        OutcomeSubjectView::Item { package_name } => {
            TableCell::new(render_name(package_name.as_str(), theme.color()))
        }
    };
    let (current, target) = version_cells(&row.versions, theme);

    RenderedOutcomeTableRow {
        status: TableCell::new(status_prefix(row.status, theme.color())),
        manager: TableCell::new(render_manager(&row.manager_id, theme.color())),
        name,
        current,
        target,
        note: TableCell::new(render_notes(&row.notes, theme)),
    }
}

fn render_notes(notes: &[OutcomeNote], theme: OutputTheme) -> String {
    let separator = if theme.color() {
        "; ".dimmed().to_string()
    } else {
        "; ".to_owned()
    };
    notes
        .iter()
        .filter(|note| note.visibility == OutcomeVisibility::Always || theme.verbose)
        .map(|note| render_note(note, theme.color()))
        .collect::<Vec<_>>()
        .join(&separator)
}

fn render_note(note: &OutcomeNote, color: bool) -> String {
    if !color {
        return note.text.clone();
    }

    note.text.dimmed().to_string()
}

fn version_cells(versions: &OutcomeVersionsView, theme: OutputTheme) -> (TableCell, TableCell) {
    match versions {
        OutcomeVersionsView::None => (TableCell::new(String::new()), TableCell::new(String::new())),
        OutcomeVersionsView::Current { version } => (
            TableCell::new(version_label(version.as_str())),
            TableCell::new(String::new()),
        ),
        OutcomeVersionsView::Change { from, to, emphasis } => {
            let from = version_label(from.as_str());
            let to = match to {
                OutcomeTargetView::Known(version) => version_label(version.as_str()),
                OutcomeTargetView::ManagerResolved => manager_resolved_label().to_owned(),
            };
            (
                TableCell::new(render_from_version(
                    &from,
                    theme.color(),
                    *emphasis == OutcomeVersionEmphasis::Current,
                )),
                TableCell::new(render_to_version(
                    &from,
                    &to,
                    theme,
                    *emphasis == OutcomeVersionEmphasis::Target,
                )),
            )
        }
    }
}

fn status_prefix(status: OutcomeStatusView, color: bool) -> String {
    match status {
        OutcomeStatusView::Current => {
            if color {
                format!("{} {}", "=".cyan().bold(), "Current".cyan().bold())
            } else {
                "= Current".to_owned()
            }
        }
        OutcomeStatusView::Update => {
            if color {
                format!("{} {}", "+".green().bold(), "Update".green().bold())
            } else {
                "+ Update".to_owned()
            }
        }
        OutcomeStatusView::Delayed => {
            if color {
                format!("{} {}", "~".yellow().bold(), "Delayed".yellow().bold())
            } else {
                "~ Delayed".to_owned()
            }
        }
        OutcomeStatusView::Blocked => {
            if color {
                format!("{} {}", "x".red().bold(), "Blocked".red().bold())
            } else {
                "x Blocked".to_owned()
            }
        }
        OutcomeStatusView::Skipped => {
            if color {
                format!("{} {}", "-".blue().bold(), "Skipped".blue().bold())
            } else {
                "- Skipped".to_owned()
            }
        }
        OutcomeStatusView::Error => {
            if color {
                format!("{} {}", "!".red().bold(), "Error".red().bold())
            } else {
                "! Error".to_owned()
            }
        }
    }
}

fn render_manager(manager_id: &ManagerId, color: bool) -> String {
    if color {
        format!("[{}]", manager_id.to_string().bold())
    } else {
        format!("[{manager_id}]")
    }
}

fn render_name(name: &str, color: bool) -> String {
    if color {
        name.bold().to_string()
    } else {
        name.to_owned()
    }
}

fn render_from_version(version: &str, color: bool, emphasize: bool) -> String {
    if color && emphasize {
        return version.bold().to_string();
    }

    version.to_owned()
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

    part.to_owned()
}

fn strip_v_prefix(text: &str) -> &str {
    text.strip_prefix('v').unwrap_or(text)
}

fn first_changed_part_index(from_parts: &[&str], to_parts: &[&str]) -> Option<usize> {
    let max_len = from_parts.len().max(to_parts.len());
    for index in 0..max_len {
        let from = from_parts.get(index).copied();
        let to = to_parts.get(index).copied();
        if from != to {
            return Some(index);
        }
    }

    None
}

pub const fn manager_resolved_label() -> &'static str {
    "selected by manager"
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
            label.to_owned()
        };

        Self {
            rendered,
            plain: label.to_owned(),
        }
    }

    fn visible_width(&self) -> usize {
        UnicodeWidthStr::width(self.plain.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderedOutcomeTableRow {
    status: TableCell,
    manager: TableCell,
    name: TableCell,
    current: TableCell,
    target: TableCell,
    note: TableCell,
}

impl RenderedOutcomeTableRow {
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

fn render_table_rows(rows: &[RenderedOutcomeTableRow], color: bool) -> Vec<String> {
    let columns = outcome_table_columns(rows);
    let headers = columns
        .iter()
        .map(|column| column.header(color))
        .collect::<Vec<_>>();
    let widths = table_widths(rows, &headers, &columns);

    std::iter::once(render_table_cells(headers.iter(), &widths))
        .chain(
            rows.iter().map(|row| {
                render_table_cells(columns.iter().map(|column| row.cell(*column)), &widths)
            }),
        )
        .collect()
}

fn outcome_table_columns(rows: &[RenderedOutcomeTableRow]) -> Vec<OutcomeTableColumn> {
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
    rows: &[RenderedOutcomeTableRow],
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

    for (index, (cell, width)) in cells.into_iter().zip(widths).enumerate() {
        if index > 0 {
            line.push_str("  ");
        }

        line.push_str(&cell.rendered);

        if index + 1 < widths.len() {
            let padding = width.saturating_sub(cell.visible_width());
            line.push_str(&" ".repeat(padding));
        }
    }

    line
}
