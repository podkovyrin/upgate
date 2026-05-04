//! Presentation crate for the `upnow` rebuild.

pub mod batch;

pub use batch::{
    render_execution_report, render_manager_error, render_scan_report, render_update_plan,
};
