use super::model::{
    DEFAULT_SCAN_OLD_AGE_THRESHOLD, ManagerMode, ManagerPolicy, ReleaseAge, UpnowConfig,
};
use super::path::config_path;
use crate::util::time::parse_duration;
use anyhow::{Context, Result};
use std::fs;
use std::time::Duration;

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
            no_update,
            mode,
            pinned: section
                .map(|cfg| cfg.pinned.iter().cloned().collect())
                .unwrap_or_default(),
        })
    }
}
