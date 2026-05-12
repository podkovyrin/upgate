mod footer;
mod frame;
mod scrollbar;
mod table;
mod tabs;

pub(crate) use footer::{KeyBinding, key_footer};
pub(crate) use frame::{app_block, render_separator};
pub(crate) use table::render_selection_table;
pub(crate) use tabs::{render_tabs, visible_tabs};
