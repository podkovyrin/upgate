use anyhow::{Result, bail};

use super::context::ManagerCtx;
use crate::config::ManagerMode;
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
    fn run(&self, ctx: &ManagerCtx) -> Result<()>;
}
