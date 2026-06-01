use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt::{self, Display};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::Deserialize;
use toml_edit::{Array, DocumentMut, Item, Table, Value};
use upnow_domain::{
    ManagerConfig, ManagerId, ManagerMode, PackageName, UpdateSelectionMode, UpdateSelectionPolicy,
    VersionPolicy,
};
use upnow_managers::adapter::ManagerConfigRuleError;
use upnow_managers::gem::GemManager;

use crate::registry::{
    accepts_no_update, ensure_known_manager, manager_defaults, min_release_age_rule_error,
    supports_version_policy,
};

const CONFIG_RELATIVE_PATH: &str = "upnow/config.toml";
const DEFAULT_SCAN_OLD_AGE_THRESHOLD: &str = "365d";
const DEFAULT_MANAGER_CONCURRENCY: usize = 4;
const DEFAULT_AUDIT_CONCURRENCY: usize = 8;
const MAX_AUDIT_CONCURRENCY: usize = 16;

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
    SelectionOverrideNotSupported(String),
    InvalidSelectionMode {
        manager_id: String,
        value: String,
    },
    InvalidSelectionException(String),
    InvalidManagerConcurrency {
        value: usize,
    },
    InvalidAuditConcurrency {
        value: usize,
    },
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
            Self::SelectionOverrideNotSupported(raw) => {
                write!(
                    formatter,
                    "invalid override `{raw}`: selection is interactive-only"
                )
            }
            Self::InvalidSelectionMode { manager_id, value } => {
                write!(
                    formatter,
                    "invalid selection mode for [{manager_id}.selection]: `{value}`, expected one of include, skip"
                )
            }
            Self::InvalidSelectionException(value) => {
                write!(formatter, "invalid selection exception `{value}`")
            }
            Self::InvalidManagerConcurrency { value } => {
                write!(
                    formatter,
                    "invalid [upnow].manager_concurrency `{value}`, expected a value greater than 0"
                )
            }
            Self::InvalidAuditConcurrency { value } => {
                write!(
                    formatter,
                    "invalid [upnow].audit_concurrency `{value}`, expected a value from 1 to 16"
                )
            }
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
    manager_concurrency: Option<usize>,
    audit_concurrency: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ManagerSectionConfig {
    min_release_age: Option<String>,
    version_policy: Option<String>,
    no_update: Option<bool>,
    mode: Option<String>,
    selection: Option<SelectionSectionConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SelectionSectionConfig {
    mode: Option<String>,
    except: Vec<String>,
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

    /// Resolves the app-level manager concurrency budget.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured value is zero.
    pub fn manager_concurrency(&self) -> Result<usize, ConfigError> {
        let value = self
            .upnow
            .manager_concurrency
            .unwrap_or(DEFAULT_MANAGER_CONCURRENCY);
        validate_manager_concurrency(value)?;
        Ok(value)
    }

    /// Resolves the app-level audit request concurrency budget.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured value is outside `1..=16`.
    pub fn audit_concurrency(&self) -> Result<usize, ConfigError> {
        let value = self
            .upnow
            .audit_concurrency
            .unwrap_or(DEFAULT_AUDIT_CONCURRENCY);
        validate_audit_concurrency(value)?;
        Ok(value)
    }

    /// Overrides the app-level manager concurrency budget in memory.
    ///
    /// # Errors
    ///
    /// Returns an error when the override value is zero.
    pub fn set_manager_concurrency(&mut self, value: usize) -> Result<(), ConfigError> {
        validate_manager_concurrency(value)?;
        self.upnow.manager_concurrency = Some(value);
        Ok(())
    }

    /// Resolves manager config into typed settings.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown managers, invalid values, or unsupported
    /// manager/policy combinations.
    pub fn resolve_manager(&self, manager_id: &str) -> Result<ManagerConfig, ConfigError> {
        let manager_id_value = ManagerId::new(manager_id.to_owned())
            .map_err(|_| ConfigError::UnknownManager(manager_id.to_owned()))?;
        let defaults = manager_defaults(manager_id)
            .map_err(|_| ConfigError::UnknownManager(manager_id.to_owned()))?;
        let section = self.sections.get(manager_id);

        let min_release_age_raw = section.and_then(|section| section.min_release_age.as_deref());
        let min_release_age = match min_release_age_raw {
            Some(raw) => parse_duration_key(&format!("[{manager_id}].min_release_age"), raw)?,
            None => defaults.min_release_age,
        };
        validate_min_release_age_rule(
            manager_id,
            min_release_age,
            min_release_age_raw.unwrap_or("<default>"),
        )?;

        let version_policy = parse_optional_policy(
            manager_id,
            section.and_then(|section| section.version_policy.as_deref()),
        )?;
        validate_policy_support(manager_id, version_policy)?;

        let mode = match section.and_then(|section| section.mode.as_deref()) {
            Some(raw) => parse_manager_mode(manager_id, raw)?,
            None => defaults.mode,
        };

        let no_update = if manager_accepts_no_update(manager_id)? {
            section
                .and_then(|section| section.no_update)
                .unwrap_or(false)
        } else {
            false
        };

        let selection = parse_optional_selection_policy(
            manager_id,
            section.and_then(|section| section.selection.as_ref()),
        )?;

        Ok(ManagerConfig {
            manager_id: manager_id_value,
            mode,
            min_release_age,
            version_policy,
            no_update,
            selection,
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
                "manager_concurrency" => {
                    let parsed = value
                        .parse::<usize>()
                        .map_err(|_| ConfigError::InvalidOverride(raw.to_owned()))?;
                    self.set_manager_concurrency(parsed)
                }
                "audit_concurrency" => {
                    let parsed = value
                        .parse::<usize>()
                        .map_err(|_| ConfigError::InvalidOverride(raw.to_owned()))?;
                    validate_audit_concurrency(parsed)?;
                    self.upnow.audit_concurrency = Some(parsed);
                    Ok(())
                }
                other => Err(ConfigError::UnknownKey {
                    section: section.to_owned(),
                    key: other.to_owned(),
                }),
            };
        }

        let section_manager_id = ManagerId::new(section.to_owned())
            .map_err(|_| ConfigError::UnknownManager(section.to_owned()))?;
        ensure_known_manager(section_manager_id.as_str())
            .map_err(|_| ConfigError::UnknownManager(section.to_owned()))?;

        match key {
            "min_release_age" => {
                let parsed_duration =
                    parse_duration_key(&format!("[{section}].min_release_age"), value)?;
                validate_min_release_age_rule(section, parsed_duration, value)?;
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
            "selection" | "pinned" | "unpinned" => {
                Err(ConfigError::SelectionOverrideNotSupported(raw.to_owned()))
            }
            "no_update" => {
                if !manager_accepts_no_update(section)? {
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
                let parsed = parse_manager_mode(section, value)?;
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
            let manager_id_value = ManagerId::new(manager_id.to_owned())
                .map_err(|_| ConfigError::UnknownManager(manager_id.to_owned()))?;
            ensure_known_manager(manager_id_value.as_str())
                .map_err(|_| ConfigError::UnknownManager(manager_id.to_owned()))?;
            self.sections.entry(manager_id.to_owned()).or_default().mode =
                Some(ManagerMode::Apply.to_string());
        }
        Ok(())
    }

    /// Replaces the in-memory selection policy for one manager.
    ///
    /// # Errors
    ///
    /// Returns an error when the manager id is unknown.
    pub fn set_manager_selection_policy(
        &mut self,
        manager_id: &str,
        selection_policy: UpdateSelectionPolicy,
    ) -> Result<(), ConfigError> {
        let manager_id_value = ManagerId::new(manager_id.to_owned())
            .map_err(|_| ConfigError::UnknownManager(manager_id.to_owned()))?;
        ensure_known_manager(manager_id_value.as_str())
            .map_err(|_| ConfigError::UnknownManager(manager_id.to_owned()))?;

        self.sections
            .entry(manager_id.to_owned())
            .or_default()
            .selection = Some(selection_section_from_policy(selection_policy));
        Ok(())
    }

    /// Persists the selection policy for one manager to the standard user config path.
    ///
    /// # Errors
    ///
    /// Returns an error when the config path cannot be resolved or the file
    /// cannot be read, parsed, or written.
    pub fn persist_manager_selection_policy(&self, manager_id: &str) -> Result<(), ConfigError> {
        let path = config_path().ok_or(ConfigError::ConfigPathUnavailable)?;
        self.persist_manager_selection_policy_to_path(manager_id, &path)
    }

    /// Persists the selection policy for one manager to a specific config path.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, parsed, or written.
    pub fn persist_manager_selection_policy_to_path(
        &self,
        manager_id: &str,
        path: &Path,
    ) -> Result<(), ConfigError> {
        let manager_id_value = ManagerId::new(manager_id.to_owned())
            .map_err(|_| ConfigError::UnknownManager(manager_id.to_owned()))?;
        ensure_known_manager(manager_id_value.as_str())
            .map_err(|_| ConfigError::UnknownManager(manager_id.to_owned()))?;

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

        let selection_policy = self
            .sections
            .get(manager_id)
            .and_then(|section| section.selection.as_ref())
            .map(|section| parse_selection_policy(manager_id, section))
            .transpose()?
            .unwrap_or_default();

        if selection_policy.is_default() {
            if let Some(item) = doc.get_mut(manager_id) {
                let table = item
                    .as_table_like_mut()
                    .ok_or_else(|| ConfigError::NonTableManagerSection(manager_id.to_owned()))?;
                table.remove("selection");
            }
        } else {
            if !doc.contains_key(manager_id) {
                doc[manager_id] = Item::Table(Table::new());
            }
            let table = doc[manager_id]
                .as_table_like_mut()
                .ok_or_else(|| ConfigError::NonTableManagerSection(manager_id.to_owned()))?;
            let mut selection_table = Table::new();
            selection_table.insert(
                "mode",
                Item::Value(Value::from(match selection_policy.mode {
                    UpdateSelectionMode::Include => "include",
                    UpdateSelectionMode::Skip => "skip",
                })),
            );
            let mut array = Array::default();
            for package_name in selection_policy.except {
                array.push(Value::from(package_name.as_str()));
            }
            if !array.is_empty() {
                selection_table.insert("except", Item::Value(Value::Array(array)));
            }
            table.insert("selection", Item::Table(selection_table));
        }

        fs::write(path, doc.to_string()).map_err(|err| {
            ConfigError::Io(format!(
                "failed to write config file {}: {err}",
                path.display()
            ))
        })
    }
}

const fn validate_manager_concurrency(value: usize) -> Result<(), ConfigError> {
    if value == 0 {
        Err(ConfigError::InvalidManagerConcurrency { value })
    } else {
        Ok(())
    }
}

fn validate_audit_concurrency(value: usize) -> Result<(), ConfigError> {
    if (1..=MAX_AUDIT_CONCURRENCY).contains(&value) {
        Ok(())
    } else {
        Err(ConfigError::InvalidAuditConcurrency { value })
    }
}

fn parse_manager_mode(manager_id: &str, raw: &str) -> Result<ManagerMode, ConfigError> {
    match raw {
        "off" => Ok(ManagerMode::Off),
        "plan" => Ok(ManagerMode::Plan),
        "apply" => Ok(ManagerMode::Apply),
        other => Err(ConfigError::InvalidMode {
            manager_id: manager_id.to_owned(),
            value: other.to_owned(),
        }),
    }
}

fn validate_policy_support(manager_id: &str, policy: VersionPolicy) -> Result<(), ConfigError> {
    let manager_id = ManagerId::new(manager_id.to_owned())
        .map_err(|_| ConfigError::UnknownManager(manager_id.to_owned()))?;
    let supported = supports_version_policy(manager_id.as_str(), policy)
        .map_err(|_| ConfigError::UnknownManager(manager_id.to_string()))?;
    if supported {
        Ok(())
    } else {
        Err(ConfigError::UnsupportedVersionPolicy {
            manager_id: manager_id.to_string(),
            policy,
        })
    }
}

fn manager_accepts_no_update(manager_id: &str) -> Result<bool, ConfigError> {
    accepts_no_update(manager_id).map_err(|_| ConfigError::UnknownManager(manager_id.to_owned()))
}

fn validate_min_release_age_rule(
    manager_id: &str,
    min_release_age: Duration,
    raw_value: &str,
) -> Result<(), ConfigError> {
    match min_release_age_rule_error(manager_id, min_release_age)
        .map_err(|_| ConfigError::UnknownManager(manager_id.to_owned()))?
    {
        Some(ManagerConfigRuleError::MinReleaseAgeMustBeWholeDays) => {
            Err(ConfigError::InvalidDuration {
                key: format!("[{manager_id}].min_release_age"),
                value: raw_value.to_owned(),
            })
        }
        None => Ok(()),
    }
}

fn parse_optional_policy(
    manager_id: &str,
    raw: Option<&str>,
) -> Result<VersionPolicy, ConfigError> {
    raw.map_or_else(
        || {
            if manager_id == GemManager::id().as_str() {
                Ok(VersionPolicy::Stable)
            } else {
                Ok(VersionPolicy::None)
            }
        },
        |raw| parse_policy(manager_id, raw),
    )
}

fn parse_optional_selection_policy(
    manager_id: &str,
    section: Option<&SelectionSectionConfig>,
) -> Result<UpdateSelectionPolicy, ConfigError> {
    section.map_or_else(
        || Ok(UpdateSelectionPolicy::default()),
        |section| parse_selection_policy(manager_id, section),
    )
}

fn parse_selection_policy(
    manager_id: &str,
    section: &SelectionSectionConfig,
) -> Result<UpdateSelectionPolicy, ConfigError> {
    let mode = match section.mode.as_deref().unwrap_or("include") {
        "include" => UpdateSelectionMode::Include,
        "skip" => UpdateSelectionMode::Skip,
        other => {
            return Err(ConfigError::InvalidSelectionMode {
                manager_id: manager_id.to_owned(),
                value: other.to_owned(),
            });
        }
    };

    let mut except = BTreeSet::new();
    for package_name in &section.except {
        except.insert(
            PackageName::new(package_name.to_owned())
                .map_err(|_| ConfigError::InvalidSelectionException(package_name.to_owned()))?,
        );
    }

    Ok(UpdateSelectionPolicy { mode, except })
}

fn selection_section_from_policy(
    selection_policy: UpdateSelectionPolicy,
) -> SelectionSectionConfig {
    SelectionSectionConfig {
        mode: Some(
            match selection_policy.mode {
                UpdateSelectionMode::Include => "include",
                UpdateSelectionMode::Skip => "skip",
            }
            .to_owned(),
        ),
        except: selection_policy
            .except
            .into_iter()
            .map(|package_name| package_name.to_string())
            .collect(),
    }
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
