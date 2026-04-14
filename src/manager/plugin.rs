use super::context::ManagerCtx;
use crate::config::ManagerMode;
use anyhow::Result;

pub trait ManagerPlugin: Sync {
    fn id(&self) -> &'static str;
    fn default_min_release_age(&self) -> &'static str;
    fn default_mode(&self) -> ManagerMode {
        ManagerMode::Apply
    }
    fn supports_no_update(&self) -> bool {
        false
    }
    fn run(&self, ctx: &ManagerCtx) -> Result<()>;
}
