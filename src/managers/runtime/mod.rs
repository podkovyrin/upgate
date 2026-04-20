pub mod context;
pub mod pipeline;
pub mod plugin;
pub mod registry;

pub use context::{ManagerCtx, RunMode};
pub use pipeline::run_manager_pipeline;
pub use plugin::ManagerPlugin;
pub use registry::{all_plugins, build_ctx_for_plugin, resolve_selected_plugins};
