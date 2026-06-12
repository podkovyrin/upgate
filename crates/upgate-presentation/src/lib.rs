//! Presentation crate for the `upgate` rebuild.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

pub mod batch;
mod notes;
pub mod outcome;
pub mod selection_view;
pub mod terminal;
pub mod theme;
pub mod tui;

pub use batch::{
    BatchRenderOptions, apply_execution_report_table, manager_error_table, render_batch_table,
    scan_report_table, update_plan_table,
};
pub use outcome::{
    OutcomeNote, OutcomeRow, OutcomeStatusView, OutcomeSubjectView, OutcomeTable,
    OutcomeTargetView, OutcomeVersionEmphasis, OutcomeVersionsView, OutcomeVisibility,
};
pub(crate) use outcome::{render_outcome_table, version_label};
pub use selection_view::{
    CandidateNoteKind, CandidateNotePart, CandidateNoteTone, SelectionRow, SelectionRowStatus,
    SelectionRowVisibility, SelectionView, TargetOption, selection_view,
};
pub use theme::{OutputTheme, ThemeOptions};
