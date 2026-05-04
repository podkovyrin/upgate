use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::time::SystemTime;

use chrono::DateTime;
use semver::Version;
use serde::Deserialize;
use upnow_domain::{
    DomainError, InstalledTool, ManagerId, ManagerMetadata, PackageName, ReleaseEntry,
    ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp, ToolId, ToolName,
    UpdateCandidate, UpdatePlan, UpdateSeed, VersionPolicy, VersionScheme, VersionText,
};
use upnow_domain::{PlanItem, PlanSelection};
use upnow_infra::{CommandCheck, CommandSpec, InfraError, ProcessRunner};

use crate::adapter::{
    CommandBuildSettings, ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind,
    ManagerCapabilities, ManagerExecutionCommand,
};

pub const MANAGER_ID: &str = "pnpm";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PnpmError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    UnknownPlanItem(String),
    ItemNotExecutable(String),
    ExactTargetUnsupported(String),
    InvalidTimestamp { version: String, value: String },
    EmptyTimeMap { package: String },
}

impl Display for PnpmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Infra(detail)
            | Self::Interrupted(detail)
            | Self::Json(detail)
            | Self::Domain(detail) => formatter.write_str(detail),
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
                write!(formatter, "pnpm view time JSON is empty for {package}")
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

impl PnpmError {
    #[must_use]
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

impl From<DomainError> for PnpmError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
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

    fn installed_tools(
        &self,
        process: &ProcessRunner,
    ) -> Result<Vec<InstalledTool>, ManagerAdapterError> {
        installed_global(process).map_err(adapter_error)
    }

    fn release_lookup(
        &self,
        process: &ProcessRunner,
        package: &PackageName,
    ) -> Result<ReleaseLookupResult, ManagerAdapterError> {
        release_lookup(process, package).map_err(adapter_error)
    }

    fn update_seeds(
        &self,
        process: &ProcessRunner,
        version_policy: VersionPolicy,
    ) -> Result<Vec<UpdateSeed>, ManagerAdapterError> {
        self.validate_version_policy(version_policy)?;
        update_seeds(process, version_policy).map_err(adapter_error)
    }

    fn commands_for_selection(
        &self,
        plan: &UpdatePlan,
        selection: &PlanSelection,
        _settings: CommandBuildSettings,
    ) -> Result<Vec<ManagerExecutionCommand>, ManagerAdapterError> {
        exact_commands_for_selection(plan, selection).map_err(adapter_error)
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

type PnpmTimeMap = BTreeMap<String, String>;

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

/// Parses `pnpm view <name> time --json`.
///
/// # Errors
///
/// Returns an error when JSON or timestamps are invalid.
pub fn parse_time_json(package: &PackageName, raw: &str) -> Result<ReleaseTimeline, PnpmError> {
    let timestamps: PnpmTimeMap =
        serde_json::from_str(raw).map_err(|err| PnpmError::Json(err.to_string()))?;
    if timestamps.is_empty() {
        return Err(PnpmError::EmptyTimeMap {
            package: package.as_str().to_owned(),
        });
    }

    let mut entries = Vec::new();
    for (version, timestamp) in timestamps {
        if version == "created" || version == "modified" {
            continue;
        }
        let parsed =
            DateTime::parse_from_rfc3339(&timestamp).map_err(|_| PnpmError::InvalidTimestamp {
                version: version.clone(),
                value: timestamp.clone(),
            })?;
        entries.push(ReleaseEntry::new(
            VersionText::new(version)?,
            ReleaseTimestamp::new(system_time_from_datetime(parsed)),
        ));
    }
    if entries.is_empty() {
        return Err(PnpmError::EmptyTimeMap {
            package: package.as_str().to_owned(),
        });
    }
    Ok(ReleaseTimeline::new(entries))
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

/// Creates update seeds for pnpm planning.
///
/// # Errors
///
/// Returns an error when installed package discovery fails.
pub fn update_seeds(
    process: &ProcessRunner,
    version_policy: VersionPolicy,
) -> Result<Vec<UpdateSeed>, PnpmError> {
    let installed = match version_policy {
        VersionPolicy::None => outdated_global(process)?
            .into_iter()
            .map(installed_tool_from_outdated)
            .collect::<Result<Vec<_>, _>>()?,
        VersionPolicy::Stable | VersionPolicy::SameTrack => installed_global(process)?,
    };
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

/// Creates exact pnpm commands for a typed selection.
///
/// # Errors
///
/// Returns an error when a selected item is unknown, not executable, or cannot
/// be executed as an exact target.
pub fn exact_commands_for_selection(
    plan: &UpdatePlan,
    selection: &PlanSelection,
) -> Result<Vec<ManagerExecutionCommand>, PnpmError> {
    let mut commands = Vec::new();
    for selected in &selection.selected_items {
        let item = plan
            .item(&selected.plan_item_id)
            .ok_or_else(|| PnpmError::UnknownPlanItem(selected.plan_item_id.as_str().to_owned()))?;
        let candidate = executable_candidate(item, selected.forced)?;
        if !candidate.execution_eligibility.supports_exact_target() {
            return Err(PnpmError::ExactTargetUnsupported(
                selected.plan_item_id.as_str().to_owned(),
            ));
        }
        commands.push(ManagerExecutionCommand {
            plan_item_id: selected.plan_item_id.clone(),
            package_name: candidate.package_name.clone(),
            installed_version: candidate.installed_version.clone(),
            target_version: candidate.target_version.clone(),
            command: exact_command(candidate),
        });
    }
    Ok(commands)
}

#[must_use]
pub fn exact_command(candidate: &UpdateCandidate) -> CommandSpec {
    let spec = format!(
        "{}@{}",
        candidate.package_name.as_str(),
        candidate.target_version.as_str()
    );
    CommandSpec::new("pnpm", ["add", "-g", &spec]).mutating()
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

fn release_lookup(
    process: &ProcessRunner,
    package: &PackageName,
) -> Result<ReleaseLookupResult, PnpmError> {
    match process.run(
        &CommandSpec::new("pnpm", ["view", package.as_str(), "time", "--json"]),
        &CommandCheck::Success,
    ) {
        Ok(output) => match output.stdout() {
            Ok(stdout) => match parse_time_json(package, stdout) {
                Ok(timeline) => Ok(ReleaseLookupResult::Known(timeline)),
                Err(PnpmError::EmptyTimeMap { .. }) => Ok(ReleaseLookupResult::MissingMetadata),
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

fn executable_candidate(item: &PlanItem, forced: bool) -> Result<&UpdateCandidate, PnpmError> {
    match item {
        PlanItem::Update { candidate, .. } => Ok(candidate),
        PlanItem::Delayed { candidate, .. } if forced => Ok(candidate),
        _ => Err(PnpmError::ItemNotExecutable(item.id().as_str().to_owned())),
    }
}

fn adapter_error(err: PnpmError) -> ManagerAdapterError {
    let kind = match &err {
        PnpmError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        PnpmError::Json(_)
        | PnpmError::Domain(_)
        | PnpmError::InvalidTimestamp { .. }
        | PnpmError::EmptyTimeMap { .. } => ManagerAdapterErrorKind::Parse,
        PnpmError::UnknownPlanItem(_)
        | PnpmError::ItemNotExecutable(_)
        | PnpmError::ExactTargetUnsupported(_) => ManagerAdapterErrorKind::CommandConstruction,
        PnpmError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail: err.to_string(),
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
