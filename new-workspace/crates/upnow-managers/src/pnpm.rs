use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use serde::Deserialize;
use upnow_domain::{
    DomainError, InstalledTool, ManagerId, ManagerMetadata, ManagerScanInput, ManagerUpdateInput,
    PackageName, ReleaseEntry, ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline,
    ReleaseTimestamp, ToolId, ToolName, UpdateCandidate, UpdateSeed, VersionPolicy, VersionScheme,
    VersionText,
};
use upnow_execution::{ExecutionCommandIntent, ResolvedExecutionItem, ResolvedExecutionPlan};
use upnow_infra::{CommandCheck, CommandSpec, Env, HttpClient, InfraError, ProcessRunner};
use upnow_release::newest_semver_version;

use crate::adapter::{
    CommandBuildSettings, ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind,
    ManagerCapabilities, ManagerExecutionCommand, ManagerExecutionCommandItem,
    ReleaseLookupSubject,
};

pub const MANAGER_ID: &str = "pnpm";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PnpmError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    InvalidTimestamp { version: String, value: String },
    MissingReleaseMetadata(String),
    UnsupportedCommandIntent(String),
}

impl Display for PnpmError {
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
                    "unsupported pnpm execution command intent `{kind}`"
                )
            }
        }
    }
}

impl std::error::Error for PnpmError {}

impl From<InfraError> for PnpmError {
    fn from(value: InfraError) -> Self {
        if value.is_interruption() {
            Self::Interrupted(value.to_string())
        } else {
            Self::Infra(value.to_string())
        }
    }
}

impl From<DomainError> for PnpmError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
    }
}

impl PnpmError {
    #[must_use]
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PnpmInstalledPackage {
    pub name: PackageName,
    pub version: VersionText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PnpmOutdatedPackage {
    pub name: PackageName,
    pub current: VersionText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PnpmManager;

impl ManagerAdapter for PnpmManager {
    fn id(&self) -> &'static str {
        MANAGER_ID
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
            .map_err(adapter_error)
    }

    fn release_lookup(
        &self,
        process: &ProcessRunner,
        _http: &HttpClient,
        _env: &Env,
        subject: ReleaseLookupSubject<'_>,
    ) -> Result<ReleaseLookupResult, ManagerAdapterError> {
        lookup_release(process, subject.package_name()).map_err(adapter_error)
    }

    fn update_inputs(
        &self,
        process: &ProcessRunner,
        _http: &HttpClient,
        _env: &Env,
        version_policy: VersionPolicy,
        _min_release_age: Duration,
        _now: SystemTime,
    ) -> Result<Vec<ManagerUpdateInput>, ManagerAdapterError> {
        self.validate_version_policy(version_policy)?;
        update_inputs(process, version_policy).map_err(adapter_error)
    }

    fn commands_for_execution_plan(
        &self,
        _process: &ProcessRunner,
        _env: &Env,
        plan: &ResolvedExecutionPlan,
        _settings: CommandBuildSettings,
    ) -> Result<Vec<ManagerExecutionCommand>, ManagerAdapterError> {
        exact_commands_for_execution_plan(plan).map_err(adapter_error)
    }
}

#[derive(Debug, Deserialize)]
struct PnpmListItem {
    #[serde(default)]
    dependencies: BTreeMap<String, PnpmDependency>,
}

#[derive(Debug, Deserialize)]
struct PnpmDependency {
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PnpmOutdatedMapEntry {
    current: Option<String>,
}

/// Parses `pnpm list -g --depth 0 --json`.
///
/// # Errors
///
/// Returns an error when JSON is malformed or a package/version is blank.
pub fn parse_installed_json(raw: &str) -> Result<Vec<PnpmInstalledPackage>, PnpmError> {
    let items: Vec<PnpmListItem> =
        serde_json::from_str(raw).map_err(|err| PnpmError::Json(err.to_string()))?;
    let mut packages = BTreeMap::new();
    for item in items {
        for (name, dependency) in item.dependencies {
            if let Some(version) = dependency.version {
                packages.insert(name, version);
            }
        }
    }

    packages
        .into_iter()
        .map(|(name, version)| {
            Ok(PnpmInstalledPackage {
                name: PackageName::new(name)?,
                version: VersionText::new(version)?,
            })
        })
        .collect()
}

/// Parses `pnpm outdated -g --json`.
///
/// # Errors
///
/// Returns an error when JSON is malformed or a package/version is blank.
pub fn parse_outdated_json(raw: &str) -> Result<Vec<PnpmOutdatedPackage>, PnpmError> {
    let entries: BTreeMap<String, PnpmOutdatedMapEntry> =
        serde_json::from_str(raw).map_err(|err| PnpmError::Json(err.to_string()))?;

    entries
        .into_iter()
        .filter_map(|(name, entry)| entry.current.map(|current| (name, current)))
        .map(|(name, current)| {
            Ok(PnpmOutdatedPackage {
                name: PackageName::new(name)?,
                current: VersionText::new(current)?,
            })
        })
        .collect()
}

#[must_use]
pub fn is_no_importer_manifest_error(text: &str) -> bool {
    text.contains("ERR_PNPM_NO_IMPORTER_MANIFEST_FOUND")
}

/// Reads installed pnpm global packages.
///
/// # Errors
///
/// Returns an error when the command fails or output cannot be parsed.
pub fn installed_global(process: &ProcessRunner) -> Result<Vec<InstalledTool>, PnpmError> {
    let output = process.run(
        &CommandSpec::new("pnpm", ["list", "-g", "--depth", "0", "--json"]),
        &CommandCheck::Success,
    )?;
    parse_installed_json(output.stdout()?)?
        .into_iter()
        .map(installed_tool)
        .collect()
}

/// Reads pnpm global outdated package names and current versions.
///
/// # Errors
///
/// Returns an error when the command fails unexpectedly or output cannot be parsed.
pub fn outdated_global(process: &ProcessRunner) -> Result<Vec<PnpmOutdatedPackage>, PnpmError> {
    let output = process.run(
        &CommandSpec::new("pnpm", ["outdated", "-g", "--json"]),
        &CommandCheck::IgnoreStatus,
    )?;
    let stdout = output.stdout()?;
    let stderr = output.stderr().unwrap_or_default();
    if is_no_importer_manifest_error(stdout) || is_no_importer_manifest_error(stderr) {
        return Ok(Vec::new());
    }
    if !output.status().success() && output.status().code() != Some(1) {
        if output.status().code().is_none() {
            return Err(PnpmError::Interrupted(
                "pnpm outdated -g --json failed (exit signal)".to_owned(),
            ));
        }
        let detail = if stderr.trim().is_empty() {
            stdout.to_owned()
        } else {
            stderr.to_owned()
        };
        return Err(PnpmError::Infra(format!(
            "pnpm outdated -g --json failed: {detail}"
        )));
    }
    if stdout.trim().is_empty() {
        return Ok(Vec::new());
    }
    parse_outdated_json(stdout)
}

/// Builds manager-owned planning inputs for pnpm.
///
/// # Errors
///
/// Returns an error when installed discovery fails or release lookup is interrupted.
pub fn update_inputs(
    process: &ProcessRunner,
    version_policy: VersionPolicy,
) -> Result<Vec<ManagerUpdateInput>, PnpmError> {
    let installed = match version_policy {
        VersionPolicy::None => outdated_global(process)?
            .into_iter()
            .map(installed_tool_from_outdated)
            .collect::<Result<Vec<_>, _>>()?,
        VersionPolicy::Stable | VersionPolicy::SameTrack => installed_global(process)?,
    };
    let mut inputs = Vec::new();
    for tool in installed {
        let lookup = lookup_release(process, &tool.package_name)?;
        inputs.push(update_input(tool, lookup));
    }
    Ok(inputs)
}

/// Looks up pnpm registry release metadata.
///
/// # Errors
///
/// Returns an error only when command execution is interrupted.
pub fn lookup_release(
    process: &ProcessRunner,
    package: &PackageName,
) -> Result<ReleaseLookupResult, PnpmError> {
    let command = CommandSpec::new("pnpm", ["view", package.as_str(), "time", "--json"]);
    match process.run(&command, &CommandCheck::Success) {
        Ok(output) => match output.stdout() {
            Ok(stdout) => match parse_pnpm_time_json(package, stdout) {
                Ok(timeline) => Ok(ReleaseLookupResult::Known(timeline)),
                Err(PnpmError::MissingReleaseMetadata(_)) => {
                    Ok(ReleaseLookupResult::MissingMetadata)
                }
                Err(err) => Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
                    err.to_string(),
                ))),
            },
            Err(err) => Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
                err.to_string(),
            ))),
        },
        Err(err) if err.is_interruption() => Err(PnpmError::from(err)),
        Err(err) => Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
            err.to_string(),
        ))),
    }
}

/// Parses pnpm registry `time` JSON metadata.
///
/// # Errors
///
/// Returns an error when JSON or timestamps are invalid, or no version timestamps are present.
pub fn parse_pnpm_time_json(
    package: &PackageName,
    raw: &str,
) -> Result<ReleaseTimeline, PnpmError> {
    let timestamps: BTreeMap<String, String> =
        serde_json::from_str(raw).map_err(|err| PnpmError::Json(err.to_string()))?;
    time_map_to_timeline(package, timestamps)
}

/// Creates exact pnpm commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when the resolved execution mode is not supported by pnpm.
pub fn exact_commands_for_execution_plan(
    plan: &ResolvedExecutionPlan,
) -> Result<Vec<ManagerExecutionCommand>, PnpmError> {
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
                return Err(PnpmError::UnsupportedCommandIntent(
                    "native-selected".to_owned(),
                ));
            }
            ExecutionCommandIntent::NativeGlobal(_) => {
                return Err(PnpmError::UnsupportedCommandIntent(
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
    let spec = format!("{}@{}", package_name.as_str(), target_version.as_str());
    CommandSpec::new("pnpm", ["add", "-g", &spec]).mutating()
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
    ManagerId::new(MANAGER_ID).expect("static pnpm manager id should be valid")
}

fn installed_tool(package: PnpmInstalledPackage) -> Result<InstalledTool, PnpmError> {
    Ok(InstalledTool::new(
        manager_id(),
        ToolId::new(package.name.as_str().to_owned())?,
        package.name.clone(),
        ToolName::new(package.name.as_str().to_owned())?,
        package.version,
        ManagerMetadata::empty(),
    ))
}

fn installed_tool_from_outdated(package: PnpmOutdatedPackage) -> Result<InstalledTool, PnpmError> {
    Ok(InstalledTool::new(
        manager_id(),
        ToolId::new(package.name.as_str().to_owned())?,
        package.name.clone(),
        ToolName::new(package.name.as_str().to_owned())?,
        package.current,
        ManagerMetadata::empty(),
    ))
}

fn update_input(tool: InstalledTool, lookup: ReleaseLookupResult) -> ManagerUpdateInput {
    let discovered_target = match &lookup {
        ReleaseLookupResult::Known(timeline) => {
            newest_semver_version(timeline).unwrap_or_else(|| tool.installed_version.clone())
        }
        ReleaseLookupResult::MissingMetadata | ReleaseLookupResult::LookupFailed(_) => {
            tool.installed_version.clone()
        }
    };
    ManagerUpdateInput::Seed(UpdateSeed::new(
        tool,
        discovered_target,
        VersionScheme::SemVer,
        lookup,
    ))
}

fn time_map_to_timeline(
    package: &PackageName,
    timestamps: BTreeMap<String, String>,
) -> Result<ReleaseTimeline, PnpmError> {
    if timestamps.is_empty() {
        return Err(PnpmError::MissingReleaseMetadata(format!(
            "registry time metadata is empty for {}",
            package.as_str()
        )));
    }

    let mut entries = Vec::new();
    for (version, timestamp) in timestamps {
        if version == "created" || version == "modified" {
            continue;
        }
        let parsed = parse_timestamp(&timestamp).ok_or_else(|| PnpmError::InvalidTimestamp {
            version: version.clone(),
            value: timestamp.clone(),
        })?;
        entries.push(ReleaseEntry::new(
            VersionText::new(version)?,
            ReleaseTimestamp::new(system_time_from_datetime(parsed)),
        ));
    }
    if entries.is_empty() {
        return Err(PnpmError::MissingReleaseMetadata(format!(
            "registry time metadata is empty for {}",
            package.as_str()
        )));
    }
    Ok(ReleaseTimeline::new(entries))
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

fn adapter_error(err: PnpmError) -> ManagerAdapterError {
    let kind = match &err {
        PnpmError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        PnpmError::Json(_)
        | PnpmError::Domain(_)
        | PnpmError::InvalidTimestamp { .. }
        | PnpmError::MissingReleaseMetadata(_) => ManagerAdapterErrorKind::Parse,
        PnpmError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        PnpmError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail: err.to_string(),
    }
}
