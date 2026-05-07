use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use pep440_rs::Version as Pep440Version;
use serde::Deserialize;
use upnow_domain::{
    DomainError, InstalledTool, ManagerId, ManagerMetadata, ManagerScanInput, ManagerUpdateInput,
    PackageName, ReleaseEntry, ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline,
    ReleaseTimestamp, ToolId, ToolName, UpdateCandidate, UpdateSeed, VersionPolicy, VersionScheme,
    VersionText,
};
use upnow_execution::{ExecutionCommandIntent, ResolvedExecutionItem, ResolvedExecutionPlan};
use upnow_infra::{CommandCheck, CommandSpec, Env, HttpClient, InfraError, ProcessRunner};
use upnow_release::newest_pep440_version;

use crate::adapter::{
    CommandBuildSettings, ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind,
    ManagerCapabilities, ManagerDefaultMode, ManagerDefaults, ManagerExecutionCommand,
    ManagerExecutionCommandItem, ReleaseLookupSubject,
};

pub const MANAGER_ID: &str = "pipx";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipxError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    InvalidTimestamp { version: String, value: String },
    MissingReleaseMetadata(String),
    UnsupportedCommandIntent(String),
}

impl Display for PipxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Infra(detail)
            | Self::Interrupted(detail)
            | Self::Json(detail)
            | Self::Domain(detail)
            | Self::MissingReleaseMetadata(detail) => formatter.write_str(detail),
            Self::InvalidTimestamp { version, value } => {
                write!(
                    formatter,
                    "invalid timestamp `{value}` for version `{version}`"
                )
            }
            Self::UnsupportedCommandIntent(kind) => {
                write!(
                    formatter,
                    "unsupported pipx execution command intent `{kind}`"
                )
            }
        }
    }
}

impl std::error::Error for PipxError {}

impl From<InfraError> for PipxError {
    fn from(value: InfraError) -> Self {
        if value.is_interruption() {
            Self::Interrupted(value.to_string())
        } else {
            Self::Infra(value.to_string())
        }
    }
}

impl From<DomainError> for PipxError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
    }
}

impl PipxError {
    #[must_use]
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipxInstalledPackage {
    pub name: PackageName,
    pub version: VersionText,
}

#[derive(Debug, Deserialize)]
struct PipxListRoot {
    #[serde(default)]
    venvs: BTreeMap<String, PipxVenv>,
}

#[derive(Debug, Deserialize)]
struct PipxVenv {
    metadata: PipxMetadata,
}

#[derive(Debug, Deserialize)]
struct PipxMetadata {
    main_package: PipxMainPackage,
}

#[derive(Debug, Deserialize)]
struct PipxMainPackage {
    package: String,
    package_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipxManager;

impl ManagerAdapter for PipxManager {
    fn id(&self) -> &'static str {
        MANAGER_ID
    }

    fn defaults(&self) -> ManagerDefaults {
        ManagerDefaults {
            min_release_age: Duration::from_secs(7 * 24 * 60 * 60),
            mode: ManagerDefaultMode::Apply,
        }
    }

    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new(true, false)
    }

    fn supports_version_policy(&self, _policy: VersionPolicy) -> bool {
        true
    }

    fn scan_inputs(
        &self,
        process: &ProcessRunner,
        _env: &Env,
    ) -> Result<Vec<ManagerScanInput>, ManagerAdapterError> {
        installed_global(process)
            .map(|tools| tools.into_iter().map(ManagerScanInput::Installed).collect())
            .map_err(|err| adapter_error(&err))
    }

    fn release_lookup(
        &self,
        _process: &ProcessRunner,
        http: &HttpClient,
        env: &Env,
        subject: ReleaseLookupSubject<'_>,
    ) -> Result<ReleaseLookupResult, ManagerAdapterError> {
        Ok(lookup_release(http, env, subject.package_name()))
    }

    fn update_inputs(
        &self,
        process: &ProcessRunner,
        http: &HttpClient,
        env: &Env,
        version_policy: VersionPolicy,
        _min_release_age: Duration,
        _no_update: bool,
    ) -> Result<Vec<ManagerUpdateInput>, ManagerAdapterError> {
        self.validate_version_policy(version_policy)?;
        update_inputs(process, http, env).map_err(|err| adapter_error(&err))
    }

    fn commands_for_execution_plan(
        &self,
        _process: &ProcessRunner,
        _env: &Env,
        plan: &ResolvedExecutionPlan,
        _settings: CommandBuildSettings,
    ) -> Result<Vec<ManagerExecutionCommand>, ManagerAdapterError> {
        commands_for_execution_plan(plan).map_err(|err| adapter_error(&err))
    }
}

/// Parses `pipx list --json`.
///
/// # Errors
///
/// Returns an error when JSON is malformed or a package/version is blank.
pub fn parse_list_json(raw: &str) -> Result<Vec<PipxInstalledPackage>, PipxError> {
    let parsed: PipxListRoot =
        serde_json::from_str(raw).map_err(|err| PipxError::Json(err.to_string()))?;
    let mut packages = BTreeMap::new();
    for venv in parsed.venvs.into_values() {
        let package = PipxInstalledPackage {
            name: PackageName::new(venv.metadata.main_package.package)?,
            version: VersionText::new(venv.metadata.main_package.package_version)?,
        };
        packages
            .entry(package.name.as_str().to_owned())
            .or_insert(package);
    }
    Ok(packages.into_values().collect())
}

/// Reads installed pipx main packages.
///
/// # Errors
///
/// Returns an error when command output cannot be parsed.
pub fn installed_global(process: &ProcessRunner) -> Result<Vec<InstalledTool>, PipxError> {
    let output = process.run(
        &CommandSpec::new("pipx", ["list", "--json"]),
        &CommandCheck::Success,
    )?;
    parse_list_json(output.stdout()?)?
        .into_iter()
        .map(installed_tool)
        .collect()
}

/// Discovers pipx packages that need release metadata before planning.
///
/// # Errors
///
/// Returns an error when installed discovery fails.
pub fn update_inputs(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
) -> Result<Vec<ManagerUpdateInput>, PipxError> {
    let mut inputs = Vec::new();
    for tool in installed_global(process)? {
        let lookup = lookup_release(http, env, &tool.package_name);
        inputs.push(update_input(tool, lookup));
    }
    Ok(inputs)
}

/// Looks up PyPI release metadata.
#[must_use]
pub fn lookup_release(http: &HttpClient, env: &Env, package: &PackageName) -> ReleaseLookupResult {
    let base_url = upnow_infra::env_base_url(env, "UPNOW_PIPX_PYPI_BASE_URL", "https://pypi.org");
    let url = format!("{base_url}/pypi/{}/json", package.as_str());
    match http.get_text(&url) {
        Ok(response) => match parse_pypi_json(package, &response.body) {
            Ok(timeline) => ReleaseLookupResult::Known(timeline),
            Err(PipxError::MissingReleaseMetadata(_)) => ReleaseLookupResult::MissingMetadata,
            Err(err) => ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string())),
        },
        Err(err) => ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string())),
    }
}

/// Parses PyPI package metadata into a release timeline.
///
/// # Errors
///
/// Returns an error when JSON or timestamps are invalid, or no version
/// timestamps are present.
pub fn parse_pypi_json(package: &PackageName, raw: &str) -> Result<ReleaseTimeline, PipxError> {
    let root: PypiRoot =
        serde_json::from_str(raw).map_err(|err| PipxError::Json(err.to_string()))?;
    let mut timestamps = BTreeMap::new();
    for (version, files) in root.releases {
        if Pep440Version::from_str(&version).is_err() {
            continue;
        }
        let Some(timestamp) = newest_pypi_upload_timestamp(&version, files)? else {
            continue;
        };
        timestamps.insert(version, timestamp);
    }
    time_map_to_timeline(package, timestamps)
}

/// Creates pipx commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when the resolved execution mode is not supported.
pub fn commands_for_execution_plan(
    plan: &ResolvedExecutionPlan,
) -> Result<Vec<ManagerExecutionCommand>, PipxError> {
    let mut commands = Vec::new();
    for intent in &plan.intents {
        match intent {
            ExecutionCommandIntent::Exact(item) => {
                commands.push(ManagerExecutionCommand {
                    items: vec![execution_item(item)],
                    command: exact_command_for_item(item),
                });
            }
            ExecutionCommandIntent::NativeSelected(_) => {
                return Err(PipxError::UnsupportedCommandIntent(
                    "native-selected".to_owned(),
                ));
            }
            ExecutionCommandIntent::ResolverNative(_) => {
                return Err(PipxError::UnsupportedCommandIntent(
                    "resolver-native".to_owned(),
                ));
            }
            ExecutionCommandIntent::ResolverNativeGlobal(_) => {
                return Err(PipxError::UnsupportedCommandIntent(
                    "resolver-native-global".to_owned(),
                ));
            }
            ExecutionCommandIntent::NativeGlobal(_) => {
                return Err(PipxError::UnsupportedCommandIntent(
                    "native-global".to_owned(),
                ));
            }
        }
    }
    Ok(commands)
}

#[must_use]
pub fn exact_command(candidate: &UpdateCandidate) -> CommandSpec {
    exact_command_parts(&candidate.package_name, &candidate.target_version)
}

fn exact_command_for_item(item: &ResolvedExecutionItem) -> CommandSpec {
    exact_command_parts(&item.package_name, &item.target_version)
}

fn exact_command_parts(package_name: &PackageName, target_version: &VersionText) -> CommandSpec {
    let spec = format!("{}=={}", package_name.as_str(), target_version.as_str());
    CommandSpec::new("pipx", ["upgrade", &spec]).mutating()
}

fn execution_item(item: &ResolvedExecutionItem) -> ManagerExecutionCommandItem {
    ManagerExecutionCommandItem {
        plan_item_id: item.plan_item_id.clone(),
        package_name: item.package_name.clone(),
        installed_version: item.installed_version.clone(),
        target_version: item.target_version.clone(),
    }
}

fn manager_id() -> ManagerId {
    ManagerId::new(MANAGER_ID).expect("static pipx manager id should be valid")
}

fn installed_tool(package: PipxInstalledPackage) -> Result<InstalledTool, PipxError> {
    Ok(InstalledTool::new(
        manager_id(),
        ToolId::new(package.name.as_str().to_owned())?,
        package.name.clone(),
        ToolName::new(package.name.as_str().to_owned())?,
        package.version,
        ManagerMetadata::empty(),
    ))
}

#[derive(Debug, Deserialize)]
struct PypiRoot {
    #[serde(default)]
    releases: BTreeMap<String, Vec<PypiReleaseFile>>,
}

#[derive(Debug, Deserialize)]
struct PypiReleaseFile {
    upload_time_iso_8601: Option<String>,
    upload_time: Option<String>,
}

fn update_input(tool: InstalledTool, lookup: ReleaseLookupResult) -> ManagerUpdateInput {
    let discovered_target = match &lookup {
        ReleaseLookupResult::Known(timeline) => {
            newest_pep440_version(timeline).unwrap_or_else(|| tool.installed_version.clone())
        }
        ReleaseLookupResult::MissingMetadata | ReleaseLookupResult::LookupFailed(_) => {
            tool.installed_version.clone()
        }
    };
    ManagerUpdateInput::Seed(UpdateSeed::new(
        tool,
        discovered_target,
        VersionScheme::Pep440,
        lookup,
    ))
}

fn time_map_to_timeline(
    package: &PackageName,
    timestamps: BTreeMap<String, String>,
) -> Result<ReleaseTimeline, PipxError> {
    if timestamps.is_empty() {
        return Err(PipxError::MissingReleaseMetadata(format!(
            "registry time metadata is empty for {}",
            package.as_str()
        )));
    }

    let mut entries = Vec::new();
    for (version, timestamp) in timestamps {
        let parsed = parse_timestamp(&timestamp).ok_or_else(|| PipxError::InvalidTimestamp {
            version: version.clone(),
            value: timestamp.clone(),
        })?;
        entries.push(ReleaseEntry::new(
            VersionText::new(version)?,
            ReleaseTimestamp::new(system_time_from_datetime(parsed)),
        ));
    }
    if entries.is_empty() {
        return Err(PipxError::MissingReleaseMetadata(format!(
            "registry time metadata is empty for {}",
            package.as_str()
        )));
    }
    Ok(ReleaseTimeline::new(entries))
}

fn newest_pypi_upload_timestamp(
    version: &str,
    files: Vec<PypiReleaseFile>,
) -> Result<Option<String>, PipxError> {
    let mut newest = None::<(DateTime<chrono::FixedOffset>, String)>;
    for file in files {
        let Some(timestamp) = file
            .upload_time_iso_8601
            .or(file.upload_time)
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let parsed = parse_timestamp(&timestamp).ok_or_else(|| PipxError::InvalidTimestamp {
            version: version.to_owned(),
            value: timestamp.clone(),
        })?;
        if newest.as_ref().is_none_or(|(current, _)| parsed > *current) {
            newest = Some((parsed, timestamp));
        }
    }
    Ok(newest.map(|(_, timestamp)| timestamp))
}

fn parse_timestamp(value: &str) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(value).ok().or_else(|| {
        let naive = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
        Some(DateTime::from_naive_utc_and_offset(
            naive,
            chrono::FixedOffset::east_opt(0)?,
        ))
    })
}

fn system_time_from_datetime(datetime: DateTime<chrono::FixedOffset>) -> SystemTime {
    let timestamp = datetime.timestamp();
    if timestamp >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_secs(timestamp.unsigned_abs())
    } else {
        SystemTime::UNIX_EPOCH - Duration::from_secs(timestamp.unsigned_abs())
    }
}

fn adapter_error(err: &PipxError) -> ManagerAdapterError {
    let kind = match err {
        PipxError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        PipxError::Json(_)
        | PipxError::Domain(_)
        | PipxError::InvalidTimestamp { .. }
        | PipxError::MissingReleaseMetadata(_) => ManagerAdapterErrorKind::Parse,
        PipxError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        PipxError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail: err.to_string(),
    }
}
