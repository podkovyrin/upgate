//! Presentation crate for the `upnow` rebuild.

pub mod batch;
pub mod outcome;
pub mod terminal;
pub mod theme;
pub mod tui;

pub use batch::{
    BatchRenderOptions, execution_report_table, manager_error_table, render_batch_table,
    render_execution_report, render_manager_error, render_scan_report, render_update_plan,
    scan_report_table, update_plan_table,
};
pub use outcome::{
    OutcomeNote, OutcomeNoteTone, OutcomeRow, OutcomeStatusView, OutcomeSubjectView, OutcomeTable,
    OutcomeVersionEmphasis, OutcomeVersionsView, OutcomeVisibility, changed_version_segment_index,
    render_outcome_table, render_to_version, strip_ansi_codes, version_label,
};
pub use theme::{OutputTheme, TerminalCapabilities, ThemeOptions};
