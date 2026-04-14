use crate::util::durationparse::parse_duration;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use toml_edit::{Array, DocumentMut, Item, Table, Value};

const CONFIG_RELATIVE_PATH: &str = "upnow/config.toml";
const DEFAULT_SCAN_OLD_AGE_THRESHOLD: &str = "365d";
pub(crate) const PIN_ALL: &str = "*";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct UpnowConfig {
    upnow: GlobalSectionConfig,
    #[serde(flatten)]
    sections: BTreeMap<String, ManagerSectionConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct GlobalSectionConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    scan_old_age_threshold: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ManagerSectionConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    min_release_age: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    no_update: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pinned: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReleaseAge {
    raw: String,
    duration: Duration,
}

impl ReleaseAge {
    fn parse_for(manager_id: &str, raw: &str) -> Result<Self> {
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

    pub(crate) fn duration(&self) -> Duration {
        self.duration
    }

    pub(crate) fn cli_arg(&self) -> &str {
        &self.raw
    }

    pub(crate) fn whole_days(&self) -> u64 {
        self.duration.as_secs() / (24 * 60 * 60)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagerMode {
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
    fn parse_for(manager_id: &str, raw: &str) -> Result<Self> {
        raw.parse::<Self>()
            .with_context(|| format!("invalid config value [{manager_id}].mode='{raw}'"))
    }

    pub(crate) fn allows_run(self, run_apply: bool) -> bool {
        match self {
            Self::Off => false,
            Self::Plan => !run_apply,
            Self::Apply => true,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ManagerPolicy {
    pub(crate) min_release_age: ReleaseAge,
    pub(crate) no_update: bool,
    pub(crate) mode: ManagerMode,
    pub(crate) pinned: BTreeSet<String>,
}

impl UpnowConfig {
    pub(crate) fn load() -> Result<Self> {
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

    pub(crate) fn scan_old_age_threshold(&self) -> Result<Duration> {
        let raw = self
            .upnow
            .scan_old_age_threshold
            .as_deref()
            .unwrap_or(DEFAULT_SCAN_OLD_AGE_THRESHOLD);
        parse_duration(raw)
            .with_context(|| format!("invalid config value [upnow].scan_old_age_threshold='{raw}'"))
    }

    pub(crate) fn resolve_manager_policy(
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

    pub(crate) fn set_manager_pins(&mut self, manager_id: &str, pins: BTreeSet<String>) {
        let section = self.sections.entry(manager_id.to_string()).or_default();
        section.pinned = pins.into_iter().collect();
    }

    pub(crate) fn persist_manager_pins(&self, manager_id: &str) -> Result<()> {
        let Some(path) = config_path() else {
            bail!("cannot determine config path from environment");
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory {}", parent.display())
            })?;
        }

        let mut doc = if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("failed to read config file {}", path.display()))?;
            if raw.trim().is_empty() {
                DocumentMut::new()
            } else {
                raw.parse::<DocumentMut>()
                    .with_context(|| format!("failed to parse config TOML at {}", path.display()))?
            }
        } else {
            DocumentMut::new()
        };

        let pins: Vec<String> = self
            .sections
            .get(manager_id)
            .map(|cfg| cfg.pinned.clone())
            .unwrap_or_default();

        if pins.is_empty() {
            if let Some(item) = doc.get_mut(manager_id) {
                let Some(table) = item.as_table_like_mut() else {
                    bail!("failed to persist pins: key '{manager_id}' is not a table");
                };
                table.remove("pinned");
            }
        } else {
            if !doc.contains_key(manager_id) {
                doc[manager_id] = Item::Table(Table::new());
            }

            let Some(table) = doc[manager_id].as_table_like_mut() else {
                bail!("failed to persist pins: key '{manager_id}' is not a table");
            };

            let mut array = Array::default();
            for pin in pins {
                array.push(Value::from(pin));
            }
            table.insert("pinned", Item::Value(Value::Array(array)));
        }

        fs::write(&path, doc.to_string())
            .with_context(|| format!("failed to write config file {}", path.display()))
    }

    pub(crate) fn apply_cli_override(
        &mut self,
        raw: &str,
        known_manager_ids: &[&str],
    ) -> Result<()> {
        let (path, value) = raw.split_once('=').with_context(|| {
            format!("invalid override '{raw}': expected <manager>.<key>=<value>")
        })?;

        let (manager_id, key) = path.split_once('.').with_context(|| {
            format!("invalid override '{raw}': expected <manager>.<key>=<value>")
        })?;

        if manager_id.is_empty() || key.is_empty() || value.is_empty() {
            bail!("invalid override '{raw}': expected <manager>.<key>=<value>");
        }

        if manager_id == "upnow" {
            return match key {
                "scan_old_age_threshold" => {
                    self.upnow.scan_old_age_threshold = Some(value.to_string());
                    Ok(())
                }
                _ => bail!(
                    "invalid override '{raw}': unknown key '{key}' for global section 'upnow'"
                ),
            };
        }

        if !known_manager_ids.contains(&manager_id) {
            bail!("invalid override '{raw}': unknown manager '{manager_id}'");
        }

        match key {
            "min_release_age" => {
                self.sections
                    .entry(manager_id.to_string())
                    .or_default()
                    .min_release_age = Some(value.to_string());
                Ok(())
            }
            "pinned" => bail!(
                "invalid override '{raw}': key 'pinned' is interactive-only in this iteration"
            ),
            "no_update" => {
                if manager_id != "brew" {
                    bail!(
                        "invalid override '{raw}': key 'no_update' is only valid for manager 'brew'"
                    );
                }

                let parsed = value.parse::<bool>().with_context(|| {
                    format!(
                        "invalid override '{raw}': value for brew.no_update must be true or false"
                    )
                })?;
                self.sections
                    .entry(manager_id.to_string())
                    .or_default()
                    .no_update = Some(parsed);
                Ok(())
            }
            "mode" => {
                let parsed = ManagerMode::parse_for(manager_id, value).with_context(|| {
                    format!(
                        "invalid override '{raw}': value for {manager_id}.mode must be one of off, plan, apply"
                    )
                })?;
                self.sections
                    .entry(manager_id.to_string())
                    .or_default()
                    .mode = Some(parsed.to_string());
                Ok(())
            }
            _ => bail!("invalid override '{raw}': unknown key '{key}' for manager '{manager_id}'"),
        }
    }
}

fn config_path() -> Option<PathBuf> {
    if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
        let trimmed = xdg_config_home.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join(CONFIG_RELATIVE_PATH));
        }
    }

    let home = std::env::var("HOME").ok()?;
    let trimmed = home.trim();
    if trimmed.is_empty() {
        return None;
    }

    Some(
        PathBuf::from(trimmed)
            .join(".config")
            .join(CONFIG_RELATIVE_PATH),
    )
}
