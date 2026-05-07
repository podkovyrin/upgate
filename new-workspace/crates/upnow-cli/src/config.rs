use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt::{self, Display};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::Deserialize;
use toml_edit::{Array, DocumentMut, Item, Table, Value};
use upnow_domain::{ManagerId, PackageName, VersionPolicy};
use upnow_managers::registry::manager_by_id;

pub const PIN_ALL: &str = "*";

const CONFIG_RELATIVE_PATH: &str = "upnow/config.toml";
const DEFAULT_SCAN_OLD_AGE_THRESHOLD: &str = "365d";
const DEFAULT_MIN_RELEASE_AGE: &str = "7d";
const BREW_MIN_RELEASE_AGE: &str = "12h";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Io(String),
    Toml(String),
    InvalidDuration {
        key: String,
        value: String,
    },
    InvalidDurationUnit {
        key: String,
        value: String,
        unit: String,
    },
    InvalidMode {
        manager_id: String,
        value: String,
    },
    InvalidOverride(String),
    UnknownManager(String),
    UnknownKey {
        section: String,
        key: String,
    },
    InvalidVersionPolicy {
        manager_id: String,
        value: String,
    },
    UnsupportedVersionPolicy {
        manager_id: String,
        policy: VersionPolicy,
    },
    InvalidNoUpdate {
        value: String,
    },
    NoUpdateOnlyBrew(String),
    PinnedOverrideNotSupported(String),
    InvalidPinnedName(String),
    ConfigPathUnavailable,
    NonTableManagerSection(String),
}

impl Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(detail) | Self::Toml(detail) => formatter.write_str(detail),
            Self::InvalidDuration { key, value } => {
                write!(formatter, "invalid duration for {key}: `{value}`")
            }
            Self::InvalidDurationUnit { key, value, unit } => {
                write!(
                    formatter,
                    "invalid duration unit `{unit}` for {key}: `{value}`, expected one of s, m, h, d"
                )
            }
            Self::InvalidMode { manager_id, value } => {
                write!(
                    formatter,
                    "invalid mode for [{manager_id}]: `{value}`, expected one of off, plan, apply"
                )
            }
            Self::InvalidOverride(raw) => {
                write!(
                    formatter,
                    "invalid override `{raw}`, expected <section>.<key>=<value>"
                )
            }
            Self::UnknownManager(manager_id) => write!(formatter, "unknown manager `{manager_id}`"),
            Self::UnknownKey { section, key } => {
                write!(
                    formatter,
                    "unknown config key `{key}` for section [{section}]"
                )
            }
            Self::InvalidVersionPolicy { manager_id, value } => {
                write!(
                    formatter,
                    "invalid version_policy for [{manager_id}]: `{value}`, expected one of none, stable, same-track"
                )
            }
            Self::UnsupportedVersionPolicy { manager_id, policy } => {
                write!(
                    formatter,
                    "version_policy `{policy}` is not supported by manager `{manager_id}`"
                )
            }
            Self::InvalidNoUpdate { value } => {
                write!(
                    formatter,
                    "invalid no_update value `{value}`, expected true or false"
                )
            }
            Self::NoUpdateOnlyBrew(raw) => {
                write!(
                    formatter,
                    "invalid override `{raw}`: no_update is only valid for brew"
                )
            }
            Self::PinnedOverrideNotSupported(raw) => {
                write!(
                    formatter,
                    "invalid override `{raw}`: pinned is interactive-only"
                )
            }
            Self::InvalidPinnedName(value) => write!(formatter, "invalid pinned name `{value}`"),
            Self::ConfigPathUnavailable => {
                formatter.write_str("cannot determine config path from environment")
            }
            Self::NonTableManagerSection(manager_id) => {
                write!(formatter, "config key `{manager_id}` is not a table")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpnowConfig {
    upnow: GlobalSectionConfig,
    #[serde(flatten)]
    sections: BTreeMap<String, ManagerSectionConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct GlobalSectionConfig {
    scan_old_age_threshold: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ManagerSectionConfig {
    min_release_age: Option<String>,
    version_policy: Option<String>,
    no_update: Option<bool>,
    mode: Option<String>,
    pinned: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerMode {
    Off,
    Plan,
    Apply,
}

impl ManagerMode {
    #[must_use]
    pub const fn allows_run(self, applying: bool) -> bool {
        match self {
            Self::Off => false,
            Self::Plan => !applying,
            Self::Apply => true,
        }
    }

    fn parse_for(manager_id: &str, raw: &str) -> Result<Self, ConfigError> {
        match raw {
            "off" => Ok(Self::Off),
            "plan" => Ok(Self::Plan),
            "apply" => Ok(Self::Apply),
            other => Err(ConfigError::InvalidMode {
                manager_id: manager_id.to_owned(),
                value: other.to_owned(),
            }),
        }
    }
}

impl Display for ManagerMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Off => "off",
            Self::Plan => "plan",
            Self::Apply => "apply",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerConfig {
    pub manager_id: ManagerId,
    pub mode: ManagerMode,
    pub min_release_age: Duration,
    pub version_policy: VersionPolicy,
    pub no_update: bool,
    pub pinned: BTreeSet<PackageName>,
}

impl UpnowConfig {
    /// Loads config from the standard user config path.
    ///
    /// # Errors
    ///
    /// Returns an error when the file exists but cannot be read or parsed.
    pub fn load() -> Result<Self, ConfigError> {
        let Some(path) = config_path() else {
            return Ok(Self::default());
        };
        Self::load_from_path(&path)
    }

    /// Loads config from a specific path.
    ///
    /// # Errors
    ///
    /// Returns an error when the file exists but cannot be read or parsed.
    pub fn load_from_path(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let raw = fs::read_to_string(path).map_err(|err| {
            ConfigError::Io(format!(
                "failed to read config file {}: {err}",
                path.display()
            ))
        })?;
        toml::from_str(&raw).map_err(|err| {
            ConfigError::Toml(format!(
                "failed to parse config TOML at {}: {err}",
                path.display()
            ))
        })
    }

    /// Resolves the global verbose-scan old-age threshold.
    ///
    /// # Errors
    ///
    /// Returns an error when the duration string is invalid.
    pub fn scan_old_age_threshold(&self) -> Result<Duration, ConfigError> {
        let raw = self
            .upnow
            .scan_old_age_threshold
            .as_deref()
            .unwrap_or(DEFAULT_SCAN_OLD_AGE_THRESHOLD);
        parse_duration_key("[upnow].scan_old_age_threshold", raw)
    }

    /// Resolves manager config into typed settings.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown managers, invalid values, or unsupported
    /// manager/policy combinations.
    pub fn resolve_manager(&self, manager_id: &str) -> Result<ManagerConfig, ConfigError> {
        let defaults = manager_defaults(manager_id)
            .ok_or_else(|| ConfigError::UnknownManager(manager_id.to_owned()))?;
        let section = self.sections.get(manager_id);

        let min_release_age_raw = section
            .and_then(|section| section.min_release_age.as_deref())
            .unwrap_or(defaults.min_release_age);
        let min_release_age = parse_duration_key(
            &format!("[{manager_id}].min_release_age"),
            min_release_age_raw,
        )?;
        if manager_id == "npm" && min_release_age.as_secs() % (24 * 60 * 60) != 0 {
            return Err(ConfigError::InvalidDuration {
                key: "[npm].min_release_age".to_owned(),
                value: min_release_age_raw.to_owned(),
            });
        }

        let version_policy = parse_optional_policy(
            manager_id,
            section.and_then(|section| section.version_policy.as_deref()),
        )?;
        validate_policy_support(manager_id, version_policy)?;

        let mode = match section.and_then(|section| section.mode.as_deref()) {
            Some(raw) => ManagerMode::parse_for(manager_id, raw)?,
            None => defaults.mode,
        };

        let no_update = if manager_id == "brew" {
            section
                .and_then(|section| section.no_update)
                .unwrap_or(false)
        } else {
            false
        };

        let mut pinned = BTreeSet::new();
        if let Some(section) = section {
            for pin in &section.pinned {
                pinned.insert(
                    PackageName::new(pin.clone())
                        .map_err(|_| ConfigError::InvalidPinnedName(pin.clone()))?,
                );
            }
        }

        Ok(ManagerConfig {
            manager_id: ManagerId::new(manager_id.to_owned())
                .map_err(|_| ConfigError::UnknownManager(manager_id.to_owned()))?,
            mode,
            min_release_age,
            version_policy,
            no_update,
            pinned,
        })
    }

    /// Applies one CLI config override in `<section>.<key>=<value>` form.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed overrides, unknown managers or keys,
    /// invalid values, and unsupported manager/policy combinations.
    pub fn apply_cli_override(&mut self, raw: &str) -> Result<(), ConfigError> {
        let (path, value) = raw
            .split_once('=')
            .ok_or_else(|| ConfigError::InvalidOverride(raw.to_owned()))?;
        let (section, key) = path
            .split_once('.')
            .ok_or_else(|| ConfigError::InvalidOverride(raw.to_owned()))?;

        if section.is_empty() || key.is_empty() || value.is_empty() {
            return Err(ConfigError::InvalidOverride(raw.to_owned()));
        }

        if section == "upnow" {
            return match key {
                "scan_old_age_threshold" => {
                    parse_duration_key("[upnow].scan_old_age_threshold", value)?;
                    self.upnow.scan_old_age_threshold = Some(value.to_owned());
                    Ok(())
                }
                other => Err(ConfigError::UnknownKey {
                    section: section.to_owned(),
                    key: other.to_owned(),
                }),
            };
        }

        if manager_defaults(section).is_none() {
            return Err(ConfigError::UnknownManager(section.to_owned()));
        }

        match key {
            "min_release_age" => {
                let parsed_duration =
                    parse_duration_key(&format!("[{section}].min_release_age"), value)?;
                if section == "npm" && parsed_duration.as_secs() % (24 * 60 * 60) != 0 {
                    return Err(ConfigError::InvalidDuration {
                        key: "[npm].min_release_age".to_owned(),
                        value: value.to_owned(),
                    });
                }
                self.sections
                    .entry(section.to_owned())
                    .or_default()
                    .min_release_age = Some(value.to_owned());
                Ok(())
            }
            "version_policy" => {
                let parsed = parse_policy(section, value)?;
                validate_policy_support(section, parsed)?;
                self.sections
                    .entry(section.to_owned())
                    .or_default()
                    .version_policy = Some(parsed.to_string());
                Ok(())
            }
            "pinned" => Err(ConfigError::PinnedOverrideNotSupported(raw.to_owned())),
            "no_update" => {
                if section != "brew" {
                    return Err(ConfigError::NoUpdateOnlyBrew(raw.to_owned()));
                }
                let parsed = value
                    .parse::<bool>()
                    .map_err(|_| ConfigError::InvalidNoUpdate {
                        value: value.to_owned(),
                    })?;
                self.sections
                    .entry(section.to_owned())
                    .or_default()
                    .no_update = Some(parsed);
                Ok(())
            }
            "mode" => {
                let parsed = ManagerMode::parse_for(section, value)?;
                self.sections.entry(section.to_owned()).or_default().mode =
                    Some(parsed.to_string());
                Ok(())
            }
            other => Err(ConfigError::UnknownKey {
                section: section.to_owned(),
                key: other.to_owned(),
            }),
        }
    }

    /// Applies the implicit mode override from an explicit manager selection.
    ///
    /// # Errors
    ///
    /// Returns an error when any selected manager id is unknown.
    pub fn apply_selected_managers_cli_override<S: AsRef<str>>(
        &mut self,
        selected_ids: &[S],
    ) -> Result<(), ConfigError> {
        for manager_id in selected_ids {
            let manager_id = manager_id.as_ref();
            if manager_defaults(manager_id).is_none() {
                return Err(ConfigError::UnknownManager(manager_id.to_owned()));
            }
            self.sections.entry(manager_id.to_owned()).or_default().mode =
                Some(ManagerMode::Apply.to_string());
        }
        Ok(())
    }

    /// Replaces the in-memory pins for one manager.
    ///
    /// # Errors
    ///
    /// Returns an error when the manager id is unknown.
    pub fn set_manager_pins(
        &mut self,
        manager_id: &str,
        pins: BTreeSet<PackageName>,
    ) -> Result<(), ConfigError> {
        if manager_defaults(manager_id).is_none() {
            return Err(ConfigError::UnknownManager(manager_id.to_owned()));
        }

        self.sections
            .entry(manager_id.to_owned())
            .or_default()
            .pinned = pins
            .into_iter()
            .map(|pin| pin.as_str().to_owned())
            .collect();
        Ok(())
    }

    /// Persists pins for one manager to the standard user config path.
    ///
    /// # Errors
    ///
    /// Returns an error when the config path cannot be resolved or the file
    /// cannot be read, parsed, or written.
    pub fn persist_manager_pins(&self, manager_id: &str) -> Result<(), ConfigError> {
        let path = config_path().ok_or(ConfigError::ConfigPathUnavailable)?;
        self.persist_manager_pins_to_path(manager_id, &path)
    }

    /// Persists pins for one manager to a specific config path.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, parsed, or written.
    pub fn persist_manager_pins_to_path(
        &self,
        manager_id: &str,
        path: &Path,
    ) -> Result<(), ConfigError> {
        if manager_defaults(manager_id).is_none() {
            return Err(ConfigError::UnknownManager(manager_id.to_owned()));
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                ConfigError::Io(format!(
                    "failed to create config directory {}: {err}",
                    parent.display()
                ))
            })?;
        }

        let mut doc = if path.exists() {
            let raw = fs::read_to_string(path).map_err(|err| {
                ConfigError::Io(format!(
                    "failed to read config file {}: {err}",
                    path.display()
                ))
            })?;
            if raw.trim().is_empty() {
                DocumentMut::new()
            } else {
                raw.parse::<DocumentMut>().map_err(|err| {
                    ConfigError::Toml(format!(
                        "failed to parse config TOML at {}: {err}",
                        path.display()
                    ))
                })?
            }
        } else {
            DocumentMut::new()
        };

        let pins = self
            .sections
            .get(manager_id)
            .map(|section| section.pinned.clone())
            .unwrap_or_default();

        if pins.is_empty() {
            if let Some(item) = doc.get_mut(manager_id) {
                let table = item
                    .as_table_like_mut()
                    .ok_or_else(|| ConfigError::NonTableManagerSection(manager_id.to_owned()))?;
                table.remove("pinned");
            }
        } else {
            if !doc.contains_key(manager_id) {
                doc[manager_id] = Item::Table(Table::new());
            }
            let table = doc[manager_id]
                .as_table_like_mut()
                .ok_or_else(|| ConfigError::NonTableManagerSection(manager_id.to_owned()))?;
            let mut array = Array::default();
            for pin in pins {
                array.push(Value::from(pin));
            }
            table.insert("pinned", Item::Value(Value::Array(array)));
        }

        fs::write(path, doc.to_string()).map_err(|err| {
            ConfigError::Io(format!(
                "failed to write config file {}: {err}",
                path.display()
            ))
        })
    }
}

#[derive(Clone, Copy)]
struct ManagerDefaults {
    min_release_age: &'static str,
    mode: ManagerMode,
}

fn manager_defaults(manager_id: &str) -> Option<ManagerDefaults> {
    match manager_id {
        "brew" => Some(ManagerDefaults {
            min_release_age: BREW_MIN_RELEASE_AGE,
            mode: ManagerMode::Apply,
        }),
        "gem" | "dotnet" => Some(ManagerDefaults {
            min_release_age: DEFAULT_MIN_RELEASE_AGE,
            mode: ManagerMode::Off,
        }),
        "bun" | "cargo" | "go" | "mise" | "npm" | "pipx" | "pnpm" | "uv" | "yarn" => {
            Some(ManagerDefaults {
                min_release_age: DEFAULT_MIN_RELEASE_AGE,
                mode: ManagerMode::Apply,
            })
        }
        _ => None,
    }
}

fn validate_policy_support(manager_id: &str, policy: VersionPolicy) -> Result<(), ConfigError> {
    let manager_id = ManagerId::new(manager_id.to_owned())
        .map_err(|_| ConfigError::UnknownManager(manager_id.to_owned()))?;
    let manager = manager_by_id(&manager_id)
        .map_err(|_| ConfigError::UnknownManager(manager_id.as_str().to_owned()))?;
    if manager.supports_version_policy(policy) {
        Ok(())
    } else {
        Err(ConfigError::UnsupportedVersionPolicy {
            manager_id: manager_id.as_str().to_owned(),
            policy,
        })
    }
}

fn parse_optional_policy(
    manager_id: &str,
    raw: Option<&str>,
) -> Result<VersionPolicy, ConfigError> {
    raw.map_or(Ok(VersionPolicy::None), |raw| parse_policy(manager_id, raw))
}

fn parse_policy(manager_id: &str, raw: &str) -> Result<VersionPolicy, ConfigError> {
    VersionPolicy::from_str(raw).map_err(|_| ConfigError::InvalidVersionPolicy {
        manager_id: manager_id.to_owned(),
        value: raw.to_owned(),
    })
}

fn parse_duration_key(key: &str, raw: &str) -> Result<Duration, ConfigError> {
    let trimmed = raw.trim();
    if trimmed.len() < 2 {
        return Err(ConfigError::InvalidDuration {
            key: key.to_owned(),
            value: raw.to_owned(),
        });
    }

    let (number, unit) = trimmed.split_at(trimmed.len() - 1);
    let value = number
        .parse::<u64>()
        .map_err(|_| ConfigError::InvalidDuration {
            key: key.to_owned(),
            value: raw.to_owned(),
        })?;

    let seconds = match unit {
        "s" => value,
        "m" => value.saturating_mul(60),
        "h" => value.saturating_mul(60 * 60),
        "d" => value.saturating_mul(24 * 60 * 60),
        other => {
            return Err(ConfigError::InvalidDurationUnit {
                key: key.to_owned(),
                value: raw.to_owned(),
                unit: other.to_owned(),
            });
        }
    };

    Ok(Duration::from_secs(seconds))
}

fn config_path() -> Option<PathBuf> {
    if let Some(xdg_config_home) = non_empty_path_var("XDG_CONFIG_HOME") {
        return Some(xdg_config_home.join(CONFIG_RELATIVE_PATH));
    }

    non_empty_path_var("HOME").map(|home_dir| home_dir.join(".config").join(CONFIG_RELATIVE_PATH))
}

fn non_empty_path_var(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
