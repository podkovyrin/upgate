use std::fs;
use std::time::Duration;

use anyhow::{Context, Result};

use super::model::{
    DEFAULT_SCAN_OLD_AGE_THRESHOLD, ManagerMode, ManagerPolicy, ReleaseAge, UpnowConfig,
};
use super::path::config_path;
use crate::managers::shared::versioning::policy::VersionPolicy;
use crate::util::time::parse_duration;

impl UpnowConfig {
    pub fn load() -> Result<Self> {
        let Some(path) = config_path() else {
            return Ok(Self::default());
        };

        if !path.exists() {
            return Ok(Self::default());
        }

        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;

        toml::from_str(&raw)
            .with_context(|| format!("failed to parse config TOML at {}", path.display()))
    }

    pub fn scan_old_age_threshold(&self) -> Result<Duration> {
        let raw = self
            .upnow
            .scan_old_age_threshold
            .as_deref()
            .unwrap_or(DEFAULT_SCAN_OLD_AGE_THRESHOLD);
        parse_duration(raw)
            .with_context(|| format!("invalid config value [upnow].scan_old_age_threshold='{raw}'"))
    }

    pub fn resolve_manager_policy(
        &self,
        manager_id: &str,
        default_min_release_age: &str,
        default_mode: ManagerMode,
        supports_no_update: bool,
    ) -> Result<ManagerPolicy> {
        let section = self.sections.get(manager_id);

        let min_release_age_raw = section
            .and_then(|cfg| cfg.min_release_age.as_deref())
            .unwrap_or(default_min_release_age);
        let min_release_age = ReleaseAge::parse_for(manager_id, min_release_age_raw)?;
        let version_policy = VersionPolicy::parse_optional_for(
            manager_id,
            section.and_then(|cfg| cfg.version_policy.as_deref()),
        )?;

        let no_update = if supports_no_update {
            section.and_then(|cfg| cfg.no_update).unwrap_or(false)
        } else {
            false
        };

        let mode = if let Some(raw) = section.and_then(|cfg| cfg.mode.as_deref()) {
            ManagerMode::parse_for(manager_id, raw)?
        } else {
            default_mode
        };

        Ok(ManagerPolicy {
            min_release_age,
            version_policy,
            no_update,
            mode,
            pinned: section
                .map(|cfg| cfg.pinned.iter().cloned().collect())
                .unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::shared::versioning::policy::VersionPolicy;

    #[test]
    fn resolve_manager_policy_defaults_version_policy_to_disabled() {
        let config = UpnowConfig::default();
        let policy = config
            .resolve_manager_policy("npm", "7d", ManagerMode::Apply, false)
            .expect("policy resolution should succeed");

        assert_eq!(policy.version_policy, VersionPolicy::Disabled);
    }

    #[test]
    fn resolve_manager_policy_parses_version_policy_value() {
        let mut config = UpnowConfig::default();
        config
            .sections
            .entry("npm".to_string())
            .or_default()
            .version_policy = Some("same-track".to_string());

        let policy = config
            .resolve_manager_policy("npm", "7d", ManagerMode::Apply, false)
            .expect("policy resolution should succeed");

        assert_eq!(policy.version_policy, VersionPolicy::SameTrack);
    }

    #[test]
    fn resolve_manager_policy_rejects_invalid_version_policy_value() {
        let mut config = UpnowConfig::default();
        config
            .sections
            .entry("npm".to_string())
            .or_default()
            .version_policy = Some("beta-only".to_string());

        let err = config
            .resolve_manager_policy("npm", "7d", ManagerMode::Apply, false)
            .expect_err("invalid policy value should fail");

        assert_eq!(
            err.to_string(),
            "Invalid version_policy for [npm]: expected one of \"stable\", \"same-track\", \"any\", got \"beta-only\""
        );
    }
}
