use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use serde::Deserialize;
use upnow_domain::{
    DomainError, ExecutionSupport, InstalledTool, ManagerConfig, ManagerId, ManagerMetadata,
    ManagerScanInput, ManagerUpdateInput, PackageName, ReleaseEntry, ReleaseLookupError,
    ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp, ToolId, ToolName, UnsupportedReason,
    UpdateSeed, VersionScheme, VersionText,
};
use upnow_execution::{
    ExecutionCommand, ExecutionCommandIntent, ExecutionCommandItem, ResolvedExecutionItem,
    ResolvedExecutionPlan,
};
use upnow_infra::{CommandCheck, CommandSpec, Env, HttpClient, InfraError, ProcessRunner};
use upnow_release::newest_semver_version;

use crate::adapter::{
    ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind, ManagerCapabilities,
    ReleaseLookupSubject, UnsupportedManagerVersion, validate_version_policy,
};

pub const MANAGER_ID: &str = "yarn";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YarnError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    InvalidTimestamp { version: String, value: String },
    MissingReleaseMetadata(String),
    InvalidMajorVersion(String),
    UnsupportedCommandIntent(String),
}

impl Display for YarnError {
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
            Self::InvalidMajorVersion(value) => {
                write!(
                    formatter,
                    "failed to parse yarn major version from `{value}`"
                )
            }
            Self::UnsupportedCommandIntent(kind) => {
                write!(
                    formatter,
                    "unsupported yarn execution command intent `{kind}`"
                )
            }
        }
    }
}

impl std::error::Error for YarnError {}

impl From<InfraError> for YarnError {
    fn from(value: InfraError) -> Self {
        if value.is_interruption() {
            Self::Interrupted(value.to_string())
        } else {
            Self::Infra(value.to_string())
        }
    }
}

impl From<DomainError> for YarnError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
    }
}

impl YarnError {
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YarnInstalledPackage {
    pub name: PackageName,
    pub version: VersionText,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum YarnGlobalListJsonLine {
    #[serde(rename = "tree")]
    Tree { data: YarnListTreeData },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum YarnTimeJsonLine {
    #[serde(rename = "inspect")]
    Inspect { data: BTreeMap<String, String> },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct YarnListTreeData {
    #[serde(default)]
    trees: Vec<YarnListTreeNode>,
}

#[derive(Debug, Deserialize)]
struct YarnListTreeNode {
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YarnManager {
    config: ManagerConfig,
}

impl YarnManager {
    pub const fn new(config: ManagerConfig) -> Self {
        Self { config }
    }

    pub fn id() -> ManagerId {
        ManagerId::from_static(MANAGER_ID)
    }
}
impl ManagerAdapter for YarnManager {
    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new()
    }
    fn unsupported_manager_version(
        &self,
        process: &ProcessRunner,
    ) -> Result<Option<UnsupportedManagerVersion>, ManagerAdapterError> {
        unsupported_manager_version(process).map_err(|err| adapter_error(&err))
    }

    fn scan_inputs(
        &self,
        process: &ProcessRunner,
        _env: &Env,
    ) -> Result<Vec<ManagerScanInput>, ManagerAdapterError> {
        installed_global_classic(process)
            .map(|tools| tools.into_iter().map(ManagerScanInput::Installed).collect())
            .map_err(|err| adapter_error(&err))
    }

    fn release_lookup(
        &self,
        process: &ProcessRunner,
        _http: &HttpClient,
        _env: &Env,
        subject: ReleaseLookupSubject<'_>,
    ) -> Result<ReleaseLookupResult, ManagerAdapterError> {
        lookup_release(process, subject.package_name()).map_err(|err| adapter_error(&err))
    }

    fn update_inputs(
        &self,
        process: &ProcessRunner,
        _http: &HttpClient,
        _env: &Env,
    ) -> Result<Vec<ManagerUpdateInput>, ManagerAdapterError> {
        validate_version_policy(
            &self.config.manager_id,
            Self::supports_version_policy(self.config.version_policy),
            self.config.version_policy,
        )?;
        update_inputs(process).map_err(|err| adapter_error(&err))
    }

    fn commands_for_execution_plan(
        &self,
        _process: &ProcessRunner,
        _env: &Env,
        plan: &ResolvedExecutionPlan,
    ) -> Result<Vec<ExecutionCommand>, ManagerAdapterError> {
        commands_for_execution_plan(plan).map_err(|err| adapter_error(&err))
    }
}
pub fn parse_yarn_major_version(text: &str) -> Option<u64> {
    let first_token = text.split_whitespace().next()?;
    let trimmed = first_token.strip_prefix('v').unwrap_or(first_token);
    trimmed.split('.').next()?.parse::<u64>().ok()
}

/// Parses `yarn global list --depth=0 --json`.
///
/// Malformed JSONL rows and non-tree rows are ignored to match Yarn's mixed
/// informational JSONL output.
///
/// # Errors
///
/// Returns an error when a parsed package name or version is invalid.
pub fn parse_global_list_jsonl(raw: &str) -> Result<Vec<YarnInstalledPackage>, YarnError> {
    let mut packages = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: YarnGlobalListJsonLine = match serde_json::from_str(trimmed) {
            Ok(parsed) => parsed,
            Err(_) => continue,
        };
        let YarnGlobalListJsonLine::Tree { data } = parsed else {
            continue;
        };
        for node in data.trees {
            if let Some((name, version)) = parse_yarn_package_spec(&node.name) {
                packages.insert(name.to_owned(), version.to_owned());
            }
        }
    }

    packages
        .into_iter()
        .map(|(name, version)| {
            Ok(YarnInstalledPackage {
                name: PackageName::new(name)?,
                version: VersionText::new(version)?,
            })
        })
        .collect()
}

/// Reads installed Yarn classic global packages.
///
/// # Errors
///
/// Returns an error when command execution or parsing fails. Yarn 2+ returns an
/// empty inventory because global upgrades are unsupported there.
pub fn installed_global(process: &ProcessRunner) -> Result<Vec<InstalledTool>, YarnError> {
    if yarn_major_version(process)? >= 2 {
        return Ok(Vec::new());
    }
    installed_global_classic(process)
}

fn installed_global_classic(process: &ProcessRunner) -> Result<Vec<InstalledTool>, YarnError> {
    let output = process.run(
        &CommandSpec::new("yarn", ["global", "list", "--depth=0", "--json"]),
        &CommandCheck::Success,
    )?;
    parse_global_list_jsonl(output.stdout()?)?
        .into_iter()
        .map(installed_tool)
        .collect()
}

/// Discovers Yarn classic packages that need release metadata before planning.
///
/// # Errors
///
/// Returns an error when discovery fails.
pub fn update_inputs(process: &ProcessRunner) -> Result<Vec<ManagerUpdateInput>, YarnError> {
    let mut inputs = Vec::new();
    for tool in installed_global_classic(process)? {
        let lookup = lookup_release(process, &tool.package_name)?;
        inputs.push(update_input(tool, lookup));
    }
    Ok(inputs)
}

/// Looks up Yarn classic registry release metadata.
///
/// # Errors
///
/// Returns an error only when command execution is interrupted.
pub fn lookup_release(
    process: &ProcessRunner,
    package: &PackageName,
) -> Result<ReleaseLookupResult, YarnError> {
    let command = CommandSpec::new("yarn", ["info", package.as_str(), "time", "--json"]);
    match process.run(&command, &CommandCheck::Success) {
        Ok(output) => match output.stdout() {
            Ok(stdout) => match parse_yarn_time_jsonl(package, stdout) {
                Ok(timeline) => Ok(ReleaseLookupResult::Known(timeline)),
                Err(YarnError::MissingReleaseMetadata(_)) => {
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
        Err(err) if err.is_interruption() => Err(YarnError::from(err)),
        Err(err) => Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
            err.to_string(),
        ))),
    }
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
) -> Result<ReleaseTimeline, YarnError> {
    let timestamps = parse_yarn_inspect_object(raw).ok_or_else(|| {
        YarnError::MissingReleaseMetadata(format!(
            "registry time metadata is empty for {}",
            package.as_str()
        ))
    })?;
    time_map_to_timeline(package, timestamps)
}

/// Creates exact Yarn commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when the resolved execution mode is not supported by Yarn.
pub fn commands_for_execution_plan(
    plan: &ResolvedExecutionPlan,
) -> Result<Vec<ExecutionCommand>, YarnError> {
    let mut commands = Vec::new();
    for intent in &plan.intents {
        match intent {
            ExecutionCommandIntent::Exact(item) => {
                commands.push(ExecutionCommand {
                    items: vec![ExecutionCommandItem::from(item)],
                    command: exact_command_for_item(item)?,
                });
            }
            ExecutionCommandIntent::NativeSelected(_) => {
                return Err(YarnError::UnsupportedCommandIntent(
                    "native-selected".to_owned(),
                ));
            }
            ExecutionCommandIntent::ResolverNative(_) => {
                return Err(YarnError::UnsupportedCommandIntent(
                    "resolver-native".to_owned(),
                ));
            }
            ExecutionCommandIntent::ResolverNativeGlobal(_) => {
                return Err(YarnError::UnsupportedCommandIntent(
                    "resolver-native-global".to_owned(),
                ));
            }
            ExecutionCommandIntent::GroupedNative(_) => {
                return Err(YarnError::UnsupportedCommandIntent(
                    "grouped-native".to_owned(),
                ));
            }
            ExecutionCommandIntent::NativeGlobal(_) => {
                return Err(YarnError::UnsupportedCommandIntent(
                    "native-global".to_owned(),
                ));
            }
        }
    }
    Ok(commands)
}

fn exact_command_for_item(item: &ResolvedExecutionItem) -> Result<CommandSpec, YarnError> {
    Ok(exact_command_parts(
        &item.package_name,
        item.known_target_version().ok_or_else(|| {
            YarnError::UnsupportedCommandIntent("exact-without-known-target".to_owned())
        })?,
    ))
}

fn exact_command_parts(package_name: &PackageName, target_version: &VersionText) -> CommandSpec {
    let spec = format!("{}@{}", package_name.as_str(), target_version.as_str());
    CommandSpec::new("yarn", ["global", "add", &spec]).mutating()
}

fn parse_yarn_package_spec(spec: &str) -> Option<(&str, &str)> {
    let (name, version) = spec.rsplit_once('@')?;
    if name.is_empty() || version.is_empty() {
        None
    } else {
        Some((name, version))
    }
}

fn yarn_major_version(process: &ProcessRunner) -> Result<u64, YarnError> {
    let version = yarn_version_text(process)?;
    parse_yarn_major_version(version.as_str())
        .ok_or_else(|| YarnError::InvalidMajorVersion(version.as_str().to_owned()))
}

fn yarn_version_text(process: &ProcessRunner) -> Result<VersionText, YarnError> {
    let output = process.run(
        &CommandSpec::new("yarn", ["--version"]),
        &CommandCheck::Success,
    )?;
    let stdout = output.stdout()?;
    VersionText::new(stdout.trim().to_owned()).map_err(YarnError::from)
}

fn unsupported_manager_version(
    process: &ProcessRunner,
) -> Result<Option<UnsupportedManagerVersion>, YarnError> {
    let version = yarn_version_text(process)?;
    let major = parse_yarn_major_version(version.as_str())
        .ok_or_else(|| YarnError::InvalidMajorVersion(version.as_str().to_owned()))?;
    if major >= 2 {
        Ok(Some(UnsupportedManagerVersion {
            installed_version: version,
            reason: UnsupportedReason::YarnModernGlobalUnsupported,
        }))
    } else {
        Ok(None)
    }
}

fn installed_tool(package: YarnInstalledPackage) -> Result<InstalledTool, YarnError> {
    Ok(InstalledTool::new(
        YarnManager::id(),
        ToolId::new(package.name.as_str().to_owned())?,
        package.name.clone(),
        ToolName::new(package.name.as_str().to_owned())?,
        package.version,
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
        ExecutionSupport::exact_only(),
    ))
}

fn time_map_to_timeline(
    package: &PackageName,
    timestamps: BTreeMap<String, String>,
) -> Result<ReleaseTimeline, YarnError> {
    if timestamps.is_empty() {
        return Err(YarnError::MissingReleaseMetadata(format!(
            "registry time metadata is empty for {}",
            package.as_str()
        )));
    }

    let mut entries = Vec::new();
    for (version, timestamp) in timestamps {
        if version == "created" || version == "modified" {
            continue;
        }
        let parsed = parse_timestamp(&timestamp).ok_or_else(|| YarnError::InvalidTimestamp {
            version: version.clone(),
            value: timestamp.clone(),
        })?;
        entries.push(ReleaseEntry::new(
            VersionText::new(version)?,
            ReleaseTimestamp::new(system_time_from_datetime(parsed)),
        ));
    }
    if entries.is_empty() {
        return Err(YarnError::MissingReleaseMetadata(format!(
            "registry time metadata is empty for {}",
            package.as_str()
        )));
    }
    Ok(ReleaseTimeline::new(entries))
}

fn parse_yarn_inspect_object(raw: &str) -> Option<BTreeMap<String, String>> {
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: YarnTimeJsonLine = match serde_json::from_str(trimmed) {
            Ok(parsed) => parsed,
            Err(_) => continue,
        };
        if let YarnTimeJsonLine::Inspect { data } = parsed {
            return Some(data);
        }
    }
    None
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

fn adapter_error(err: &YarnError) -> ManagerAdapterError {
    let detail = err.to_string();
    let kind = match err {
        &YarnError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        &YarnError::Json(_)
        | &YarnError::Domain(_)
        | &YarnError::InvalidTimestamp { .. }
        | &YarnError::MissingReleaseMetadata(_)
        | &YarnError::InvalidMajorVersion(_) => ManagerAdapterErrorKind::Parse,
        &YarnError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        &YarnError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail,
    }
}
