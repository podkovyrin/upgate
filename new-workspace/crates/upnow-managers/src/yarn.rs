use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::time::SystemTime;

use chrono::DateTime;
use semver::Version;
use serde::Deserialize;
use upnow_domain::{
    DomainError, InstalledTool, ManagerId, ManagerMetadata, PackageName, PlanItem, PlanSelection,
    ReleaseEntry, ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp,
    ToolId, ToolName, UnsupportedReason, UpdateCandidate, UpdatePlan, UpdateSeed, VersionPolicy,
    VersionScheme, VersionText,
};
use upnow_infra::{CommandCheck, CommandSpec, InfraError, ProcessRunner};

use crate::adapter::{
    CommandBuildSettings, ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind,
    ManagerCapabilities, ManagerExecutionCommand, ManagerExecutionCommandItem,
    UnsupportedManagerVersion,
};

pub const MANAGER_ID: &str = "yarn";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YarnError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    InvalidMajorVersion(String),
    UnsupportedMajorVersion(u64),
    UnknownPlanItem(String),
    ItemNotExecutable(String),
    ExactTargetUnsupported(String),
    InvalidTimestamp { version: String, value: String },
    EmptyTimeMap { package: String },
}

impl Display for YarnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Infra(detail)
            | Self::Interrupted(detail)
            | Self::Json(detail)
            | Self::Domain(detail) => formatter.write_str(detail),
            Self::InvalidMajorVersion(value) => {
                write!(
                    formatter,
                    "failed to parse yarn major version from `{value}`"
                )
            }
            Self::UnsupportedMajorVersion(version) => {
                write!(
                    formatter,
                    "global upgrades are not supported for Yarn {version}+"
                )
            }
            Self::UnknownPlanItem(id) => write!(formatter, "unknown selected plan item `{id}`"),
            Self::ItemNotExecutable(id) => write!(formatter, "plan item `{id}` is not executable"),
            Self::ExactTargetUnsupported(id) => {
                write!(
                    formatter,
                    "plan item `{id}` does not support exact target execution"
                )
            }
            Self::InvalidTimestamp { version, value } => {
                write!(
                    formatter,
                    "invalid timestamp `{value}` for version `{version}`"
                )
            }
            Self::EmptyTimeMap { package } => {
                write!(formatter, "yarn info time JSON is empty for {package}")
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
    #[must_use]
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
struct YarnListTreeData {
    #[serde(default)]
    trees: Vec<YarnListTreeNode>,
}

#[derive(Debug, Deserialize)]
struct YarnListTreeNode {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum YarnInfoJsonLine {
    #[serde(rename = "inspect")]
    Inspect { data: BTreeMap<String, String> },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YarnManager;

impl ManagerAdapter for YarnManager {
    fn id(&self) -> &'static str {
        MANAGER_ID
    }

    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new(true, false)
    }

    fn supports_version_policy(&self, _policy: VersionPolicy) -> bool {
        true
    }

    fn unsupported_manager_version(
        &self,
        process: &ProcessRunner,
    ) -> Result<Option<UnsupportedManagerVersion>, ManagerAdapterError> {
        unsupported_manager_version(process).map_err(|err| adapter_error(&err))
    }

    fn installed_tools(
        &self,
        process: &ProcessRunner,
    ) -> Result<Vec<InstalledTool>, ManagerAdapterError> {
        installed_global_classic(process).map_err(|err| adapter_error(&err))
    }

    fn release_lookup(
        &self,
        process: &ProcessRunner,
        package: &PackageName,
    ) -> Result<ReleaseLookupResult, ManagerAdapterError> {
        release_lookup(process, package).map_err(|err| adapter_error(&err))
    }

    fn update_seeds(
        &self,
        process: &ProcessRunner,
        version_policy: VersionPolicy,
    ) -> Result<Vec<UpdateSeed>, ManagerAdapterError> {
        self.validate_version_policy(version_policy)?;
        update_seeds(process).map_err(|err| adapter_error(&err))
    }

    fn commands_for_selection(
        &self,
        _process: &ProcessRunner,
        plan: &UpdatePlan,
        selection: &PlanSelection,
        _settings: CommandBuildSettings,
    ) -> Result<Vec<ManagerExecutionCommand>, ManagerAdapterError> {
        commands_for_selection(plan, selection).map_err(|err| adapter_error(&err))
    }
}

#[must_use]
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

/// Parses `yarn info <name> time --json`.
///
/// # Errors
///
/// Returns an error when no inspect object is present or timestamps are invalid.
pub fn parse_time_jsonl(package: &PackageName, raw: &str) -> Result<ReleaseTimeline, YarnError> {
    let timestamps = parse_inspect_object(raw).ok_or_else(|| YarnError::EmptyTimeMap {
        package: package.as_str().to_owned(),
    })?;
    time_map_to_timeline(package, timestamps)
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

/// Creates update seeds for Yarn classic.
///
/// # Errors
///
/// Returns an error when discovery fails.
pub fn update_seeds(process: &ProcessRunner) -> Result<Vec<UpdateSeed>, YarnError> {
    let installed = installed_global_classic(process)?;
    let mut seeds = Vec::new();
    for tool in installed {
        let lookup = release_lookup(process, &tool.package_name)?;
        let discovered_target = match &lookup {
            ReleaseLookupResult::Known(timeline) => {
                newest_semver(timeline).unwrap_or_else(|| tool.installed_version.clone())
            }
            ReleaseLookupResult::MissingMetadata | ReleaseLookupResult::LookupFailed(_) => {
                tool.installed_version.clone()
            }
        };
        seeds.push(UpdateSeed::new(
            tool,
            discovered_target,
            VersionScheme::SemVer,
            lookup,
        ));
    }
    Ok(seeds)
}

/// Creates exact Yarn commands for a typed selection.
///
/// # Errors
///
/// Returns an error when the selected item is unknown or not exact-executable.
pub fn commands_for_selection(
    plan: &UpdatePlan,
    selection: &PlanSelection,
) -> Result<Vec<ManagerExecutionCommand>, YarnError> {
    let mut commands = Vec::new();
    for selected in &selection.selected_items {
        let item = plan
            .item(&selected.plan_item_id)
            .ok_or_else(|| YarnError::UnknownPlanItem(selected.plan_item_id.as_str().to_owned()))?;
        let candidate = executable_candidate(item, selected.forced)?;
        if !candidate.execution_eligibility.supports_exact_target() {
            return Err(YarnError::ExactTargetUnsupported(
                selected.plan_item_id.as_str().to_owned(),
            ));
        }
        commands.push(ManagerExecutionCommand {
            items: vec![execution_item(selected.plan_item_id.clone(), candidate)],
            command: exact_command(candidate),
        });
    }
    Ok(commands)
}

fn execution_item(
    plan_item_id: upnow_domain::PlanItemId,
    candidate: &UpdateCandidate,
) -> ManagerExecutionCommandItem {
    ManagerExecutionCommandItem {
        plan_item_id,
        package_name: candidate.package_name.clone(),
        installed_version: candidate.installed_version.clone(),
        target_version: candidate.target_version.clone(),
    }
}

#[must_use]
pub fn exact_command(candidate: &UpdateCandidate) -> CommandSpec {
    let spec = format!(
        "{}@{}",
        candidate.package_name.as_str(),
        candidate.target_version.as_str()
    );
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

fn parse_inspect_object(raw: &str) -> Option<BTreeMap<String, String>> {
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

fn time_map_to_timeline(
    package: &PackageName,
    timestamps: BTreeMap<String, String>,
) -> Result<ReleaseTimeline, YarnError> {
    if timestamps.is_empty() {
        return Err(YarnError::EmptyTimeMap {
            package: package.as_str().to_owned(),
        });
    }
    let mut entries = Vec::new();
    for (version, timestamp) in timestamps {
        if version == "created" || version == "modified" {
            continue;
        }
        let parsed =
            DateTime::parse_from_rfc3339(&timestamp).map_err(|_| YarnError::InvalidTimestamp {
                version: version.clone(),
                value: timestamp.clone(),
            })?;
        entries.push(ReleaseEntry::new(
            VersionText::new(version)?,
            ReleaseTimestamp::new(system_time_from_datetime(parsed)),
        ));
    }
    if entries.is_empty() {
        return Err(YarnError::EmptyTimeMap {
            package: package.as_str().to_owned(),
        });
    }
    Ok(ReleaseTimeline::new(entries))
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

fn manager_id() -> ManagerId {
    ManagerId::new(MANAGER_ID).expect("static yarn manager id should be valid")
}

fn installed_tool(package: YarnInstalledPackage) -> Result<InstalledTool, YarnError> {
    Ok(InstalledTool::new(
        manager_id(),
        ToolId::new(package.name.as_str().to_owned())?,
        package.name.clone(),
        ToolName::new(package.name.as_str().to_owned())?,
        package.version,
        ManagerMetadata::empty(),
    ))
}

fn release_lookup(
    process: &ProcessRunner,
    package: &PackageName,
) -> Result<ReleaseLookupResult, YarnError> {
    match process.run(
        &CommandSpec::new("yarn", ["info", package.as_str(), "time", "--json"]),
        &CommandCheck::Success,
    ) {
        Ok(output) => match output.stdout() {
            Ok(stdout) => match parse_time_jsonl(package, stdout) {
                Ok(timeline) => Ok(ReleaseLookupResult::Known(timeline)),
                Err(YarnError::EmptyTimeMap { .. }) => Ok(ReleaseLookupResult::MissingMetadata),
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

fn newest_semver(timeline: &ReleaseTimeline) -> Option<VersionText> {
    timeline
        .versions
        .iter()
        .filter_map(|entry| {
            Version::parse(entry.version.as_str())
                .ok()
                .map(|version| (version, entry.version.clone()))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, version)| version)
}

fn executable_candidate(item: &PlanItem, forced: bool) -> Result<&UpdateCandidate, YarnError> {
    match item {
        PlanItem::Update { candidate, .. } => Ok(candidate),
        PlanItem::Delayed { candidate, .. } if forced => Ok(candidate),
        _ => Err(YarnError::ItemNotExecutable(item.id().as_str().to_owned())),
    }
}

fn adapter_error(err: &YarnError) -> ManagerAdapterError {
    let detail = err.to_string();
    let kind = match err {
        &YarnError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        &YarnError::Json(_)
        | &YarnError::Domain(_)
        | &YarnError::InvalidMajorVersion(_)
        | &YarnError::InvalidTimestamp { .. }
        | &YarnError::EmptyTimeMap { .. } => ManagerAdapterErrorKind::Parse,
        &YarnError::UnknownPlanItem(_)
        | &YarnError::ItemNotExecutable(_)
        | &YarnError::ExactTargetUnsupported(_) => ManagerAdapterErrorKind::CommandConstruction,
        &YarnError::Infra(_) | &YarnError::UnsupportedMajorVersion(_) => {
            ManagerAdapterErrorKind::Infra
        }
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail,
    }
}

fn system_time_from_datetime(datetime: DateTime<chrono::FixedOffset>) -> SystemTime {
    let timestamp = datetime.timestamp();
    if timestamp >= 0 {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(timestamp.unsigned_abs())
    } else {
        SystemTime::UNIX_EPOCH - std::time::Duration::from_secs(timestamp.unsigned_abs())
    }
}
