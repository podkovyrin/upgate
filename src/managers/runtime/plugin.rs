use anyhow::{Result, bail};

use super::context::ManagerCtx;
use crate::config::ManagerMode;
use crate::interactive::apply::InteractiveApplyPlan;
use crate::managers::shared::versioning::policy::VersionPolicy;

pub trait ManagerPlugin: Sync {
    fn id(&self) -> &'static str;
    fn default_min_release_age(&self) -> &'static str;
    fn default_mode(&self) -> ManagerMode {
        ManagerMode::Apply
    }
    fn supports_current_platform(&self) -> bool {
        cfg!(unix)
    }
    fn unsupported_platform_reason(&self) -> &'static str {
        "unsupported platform: requires unix"
    }
    fn probe_command(&self) -> Option<String> {
        Some(self.id().to_string())
    }
    fn supports_no_update(&self) -> bool {
        false
    }
    fn supports_version_policy(&self, policy: VersionPolicy) -> bool {
        matches!(policy, VersionPolicy::Disabled | VersionPolicy::Any)
    }
    fn validate_version_policy(&self, policy: VersionPolicy) -> Result<()> {
        if self.supports_version_policy(policy) {
            return Ok(());
        }

        bail!(
            "version_policy \"{}\" is not supported by this manager",
            policy.as_str()
        )
    }
    fn scan(&self, ctx: &ManagerCtx) -> Result<()>;
    fn apply(&self, ctx: &ManagerCtx) -> Result<()>;
    fn interactive_apply(&self, ctx: &ManagerCtx) -> Result<Option<InteractiveApplyPlan>>;
    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        if ctx.is_scan() {
            return self.scan(ctx);
        }

        self.apply(ctx)
    }
}

#[macro_export]
macro_rules! impl_manager_pipeline {
    () => {
        fn scan(&self, ctx: &$crate::managers::ManagerCtx) -> anyhow::Result<()> {
            scan(ctx)
        }

        fn apply(&self, ctx: &$crate::managers::ManagerCtx) -> anyhow::Result<()> {
            apply(ctx)
        }

        fn interactive_apply(
            &self,
            ctx: &$crate::managers::ManagerCtx,
        ) -> anyhow::Result<Option<$crate::interactive::apply::InteractiveApplyPlan>> {
            interactive_apply(ctx)
        }
    };
}
