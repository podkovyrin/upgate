use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use serde::Deserialize;
use upnow_domain::{
    DomainError, PackageName, ReleaseEntry, ReleaseLookupError, ReleaseLookupResult,
    ReleaseTimeline, ReleaseTimestamp, VersionText,
};
use upnow_infra::{CommandCheck, CommandSpec, InfraError, ProcessRunner};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseLookupRequest {
    NpmRegistryTime {
        source: NpmRegistryTimeSource,
        package: PackageName,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NpmRegistryTimeSource {
    Npm,
    Pnpm,
    YarnClassic,
    Bun { executable: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseParseError {
    Json(String),
    Domain(String),
    InvalidTimestamp { version: String, value: String },
    EmptyTimeMap { package: String },
}

impl Display for ReleaseParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(detail) | Self::Domain(detail) => formatter.write_str(detail),
            Self::InvalidTimestamp { version, value } => {
                write!(
                    formatter,
                    "invalid timestamp `{value}` for version `{version}`"
                )
            }
            Self::EmptyTimeMap { package } => {
                write!(formatter, "registry time metadata is empty for {package}")
            }
        }
    }
}

impl std::error::Error for ReleaseParseError {}

impl From<DomainError> for ReleaseParseError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum YarnInfoJsonLine {
    #[serde(rename = "inspect")]
    Inspect { data: BTreeMap<String, String> },
    #[serde(other)]
    Other,
}

type NpmTimeMap = BTreeMap<String, String>;

/// Looks up npm-family release metadata for one adapter-owned request.
///
/// # Errors
///
/// Returns an infra error only for fatal command interruptions.
pub fn lookup_release(
    process: &ProcessRunner,
    request: &ReleaseLookupRequest,
) -> Result<ReleaseLookupResult, InfraError> {
    match request {
        ReleaseLookupRequest::NpmRegistryTime { source, package } => {
            lookup_npm_registry_time(process, source, package)
        }
    }
}

/// Parses npm-compatible registry `time` JSON metadata.
///
/// # Errors
///
/// Returns an error when JSON or timestamps are invalid, or no version
/// timestamps are present.
pub fn parse_npm_time_json(
    package: &PackageName,
    raw: &str,
) -> Result<ReleaseTimeline, ReleaseParseError> {
    let timestamps: NpmTimeMap =
        serde_json::from_str(raw).map_err(|err| ReleaseParseError::Json(err.to_string()))?;
    time_map_to_timeline(package, timestamps)
}

/// Parses Yarn classic JSONL `info <package> time --json` metadata.
///
/// # Errors
///
/// Returns an error when no inspect object is present, timestamps are invalid,
/// or no version timestamps are present.
pub fn parse_yarn_time_jsonl(
    package: &PackageName,
    raw: &str,
) -> Result<ReleaseTimeline, ReleaseParseError> {
    let timestamps =
        parse_yarn_inspect_object(raw).ok_or_else(|| ReleaseParseError::EmptyTimeMap {
            package: package.as_str().to_owned(),
        })?;
    time_map_to_timeline(package, timestamps)
}

#[must_use]
pub fn bun_global_cwd_from_values(bun_install: Option<&str>, home: Option<&str>) -> Option<String> {
    bun_install
        .and_then(trim_non_empty)
        .map(|path| format!("{path}/install/global"))
        .or_else(|| {
            home.and_then(trim_non_empty)
                .map(|path| format!("{path}/.bun/install/global"))
        })
}

fn lookup_npm_registry_time(
    process: &ProcessRunner,
    source: &NpmRegistryTimeSource,
    package: &PackageName,
) -> Result<ReleaseLookupResult, InfraError> {
    let command = match source {
        NpmRegistryTimeSource::Npm => {
            CommandSpec::new("npm", ["view", package.as_str(), "time", "--json"])
        }
        NpmRegistryTimeSource::Pnpm => {
            CommandSpec::new("pnpm", ["view", package.as_str(), "time", "--json"])
        }
        NpmRegistryTimeSource::YarnClassic => {
            CommandSpec::new("yarn", ["info", package.as_str(), "time", "--json"])
        }
        NpmRegistryTimeSource::Bun { executable } => {
            let Some(cwd) = bun_global_cwd() else {
                return Ok(ReleaseLookupResult::MissingMetadata);
            };
            CommandSpec::new(
                executable,
                [
                    "pm",
                    "view",
                    package.as_str(),
                    "time",
                    "--json",
                    "--cwd",
                    &cwd,
                ],
            )
        }
    };

    let check = if matches!(source, NpmRegistryTimeSource::Bun { .. }) {
        CommandCheck::IgnoreStatus
    } else {
        CommandCheck::Success
    };

    match process.run(&command, &check) {
        Ok(output)
            if matches!(source, NpmRegistryTimeSource::Bun { .. })
                && !output.status().success() =>
        {
            let detail = output.stderr().unwrap_or_default().to_owned();
            Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
                detail,
            )))
        }
        Ok(output) => match output.stdout() {
            Ok(stdout) => {
                let parsed = if matches!(source, NpmRegistryTimeSource::YarnClassic) {
                    parse_yarn_time_jsonl(package, stdout)
                } else {
                    parse_npm_time_json(package, stdout)
                };
                match parsed {
                    Ok(timeline) => Ok(ReleaseLookupResult::Known(timeline)),
                    Err(ReleaseParseError::EmptyTimeMap { .. }) => {
                        Ok(ReleaseLookupResult::MissingMetadata)
                    }
                    Err(err) => Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
                        err.to_string(),
                    ))),
                }
            }
            Err(err) => Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
                err.to_string(),
            ))),
        },
        Err(err) if err.is_interruption() => Err(err),
        Err(err) => Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
            err.to_string(),
        ))),
    }
}

fn time_map_to_timeline(
    package: &PackageName,
    timestamps: BTreeMap<String, String>,
) -> Result<ReleaseTimeline, ReleaseParseError> {
    if timestamps.is_empty() {
        return Err(ReleaseParseError::EmptyTimeMap {
            package: package.as_str().to_owned(),
        });
    }

    let mut entries = Vec::new();
    for (version, timestamp) in timestamps {
        if version == "created" || version == "modified" {
            continue;
        }
        let parsed = DateTime::parse_from_rfc3339(&timestamp).map_err(|_| {
            ReleaseParseError::InvalidTimestamp {
                version: version.clone(),
                value: timestamp.clone(),
            }
        })?;
        entries.push(ReleaseEntry::new(
            VersionText::new(version)?,
            ReleaseTimestamp::new(system_time_from_datetime(parsed)),
        ));
    }
    if entries.is_empty() {
        return Err(ReleaseParseError::EmptyTimeMap {
            package: package.as_str().to_owned(),
        });
    }
    Ok(ReleaseTimeline::new(entries))
}

fn parse_yarn_inspect_object(raw: &str) -> Option<BTreeMap<String, String>> {
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: YarnInfoJsonLine = match serde_json::from_str(trimmed) {
            Ok(parsed) => parsed,
            Err(_) => continue,
        };
        if let YarnInfoJsonLine::Inspect { data } = parsed {
            return Some(data);
        }
    }
    None
}

fn bun_global_cwd() -> Option<String> {
    bun_global_cwd_from_values(
        std::env::var("BUN_INSTALL").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

fn trim_non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn system_time_from_datetime(datetime: DateTime<chrono::FixedOffset>) -> SystemTime {
    let timestamp = datetime.timestamp();
    if timestamp >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_secs(timestamp.unsigned_abs())
    } else {
        SystemTime::UNIX_EPOCH - Duration::from_secs(timestamp.unsigned_abs())
    }
}
