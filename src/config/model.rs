use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::managers::shared::versioning::policy::VersionPolicy;
use crate::util::time::parse_duration;

pub const PIN_ALL: &str = "*";
pub(super) const DEFAULT_SCAN_OLD_AGE_THRESHOLD: &str = "365d";

pub fn is_pinned(name: &str, pinned: &BTreeSet<String>) -> bool {
    pinned.contains(name) || pinned.contains(PIN_ALL)
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpnowConfig {
    pub(super) upnow: GlobalSectionConfig,
    #[serde(flatten)]
    pub(super) sections: BTreeMap<String, ManagerSectionConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(super) struct GlobalSectionConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) scan_old_age_threshold: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(super) struct ManagerSectionConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) min_release_age: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) version_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) no_update: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) mode: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) pinned: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ReleaseAge {
    raw: String,
    duration: Duration,
}

impl ReleaseAge {
    pub(super) fn parse_for(manager_id: &str, raw: &str) -> Result<Self> {
        let raw = raw.to_string();
        let duration = parse_duration(&raw).with_context(|| {
            format!("invalid config value [{manager_id}].min_release_age='{raw}'")
        })?;

        if manager_id == "npm" {
            let day_secs = 24 * 60 * 60;
            if duration.as_secs() % day_secs != 0 {
                bail!(
                    "invalid config value [npm].min_release_age='{raw}': npm requires whole-day values like 7d"
                );
            }
        }

        Ok(Self { raw, duration })
    }

    pub const fn duration(&self) -> Duration {
        self.duration
    }

    pub fn cli_arg(&self) -> &str {
        &self.raw
    }

    pub const fn whole_days(&self) -> u64 {
        self.duration.as_secs() / (24 * 60 * 60)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerMode {
    Off,
    Plan,
    Apply,
}

impl fmt::Display for ManagerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::Plan => f.write_str("plan"),
            Self::Apply => f.write_str("apply"),
        }
    }
}

impl FromStr for ManagerMode {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "off" => Ok(Self::Off),
            "plan" => Ok(Self::Plan),
            "apply" => Ok(Self::Apply),
            _ => bail!("expected one of off, plan, apply"),
        }
    }
}

impl ManagerMode {
    pub(super) fn parse_for(manager_id: &str, raw: &str) -> Result<Self> {
        raw.parse::<Self>()
            .with_context(|| format!("invalid config value [{manager_id}].mode='{raw}'"))
    }

    pub const fn allows_run(self, run_apply: bool) -> bool {
        match self {
            Self::Off => false,
            Self::Plan => !run_apply,
            Self::Apply => true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ManagerPolicy {
    pub min_release_age: ReleaseAge,
    pub version_policy: VersionPolicy,
    pub no_update: bool,
    pub mode: ManagerMode,
    pub pinned: BTreeSet<String>,
}
