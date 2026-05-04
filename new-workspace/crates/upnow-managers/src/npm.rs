use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use semver::Version;
use serde::Deserialize;
use upnow_domain::{
    DomainError, InstalledTool, ManagerId, ManagerMetadata, PackageName, PlanItem, PlanSelection,
    ReleaseEntry, ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp,
    ToolId, ToolName, UpdateCandidate, UpdatePlan, UpdateSeed, VersionPolicy, VersionScheme,
    VersionText,
};
use upnow_infra::{CommandCheck, CommandSpec, InfraError, ProcessRunner};

use crate::adapter::{
    CommandBuildSettings, ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind,
    ManagerCapabilities, ManagerExecutionCommand,
};

pub const MANAGER_ID: &str = "npm";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NpmError {
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

impl Display for NpmError {
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
                write!(formatter, "npm view time JSON is empty for {package}")
            }
        }
    }
}

impl std::error::Error for NpmError {}

impl From<InfraError> for NpmError {
    fn from(value: InfraError) -> Self {
        if value.is_interruption() {
            Self::Interrupted(value.to_string())
        } else {
            Self::Infra(value.to_string())
        }
    }
}

impl From<DomainError> for NpmError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
    }
}

impl NpmError {
    #[must_use]
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmInstalledPackage {
    pub name: PackageName,
    pub version: VersionText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmOutdatedPackage {
    pub name: PackageName,
    pub current: VersionText,
}

#[derive(Debug, Deserialize)]
struct NpmListJson {
    #[serde(default)]
    dependencies: BTreeMap<String, NpmDependency>,
}

#[derive(Debug, Deserialize)]
struct NpmDependency {
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NpmOutdatedMapEntry {
    current: String,
}

type NpmTimeMap = BTreeMap<String, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NpmManager;

impl ManagerAdapter for NpmManager {
    fn id(&self) -> &'static str {
        MANAGER_ID
    }

    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new(true, true)
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
        settings: CommandBuildSettings,
    ) -> Result<Vec<ManagerExecutionCommand>, ManagerAdapterError> {
        commands_for_selection(plan, selection, settings).map_err(adapter_error)
    }
}

/// Parses `npm ls -g --depth=0 --json`.
///
/// # Errors
///
/// Returns an error when JSON is malformed or a package/version is blank.
pub fn parse_installed_json(raw: &str) -> Result<Vec<NpmInstalledPackage>, NpmError> {
    let parsed: NpmListJson =
        serde_json::from_str(raw).map_err(|err| NpmError::Json(err.to_string()))?;

    parsed
        .dependencies
        .into_iter()
        .filter_map(|(name, dependency)| dependency.version.map(|version| (name, version)))
        .map(|(name, version)| {
            Ok(NpmInstalledPackage {
                name: PackageName::new(name)?,
                version: VersionText::new(version)?,
            })
        })
        .collect()
}

/// Parses `npm outdated -g --json`.
///
/// # Errors
///
/// Returns an error when JSON is malformed or a package/version is blank.
pub fn parse_outdated_json(raw: &str) -> Result<Vec<NpmOutdatedPackage>, NpmError> {
    let entries: BTreeMap<String, NpmOutdatedMapEntry> =
        serde_json::from_str(raw).map_err(|err| NpmError::Json(err.to_string()))?;

    entries
        .into_iter()
        .map(|(name, entry)| {
            Ok(NpmOutdatedPackage {
                name: PackageName::new(name)?,
                current: VersionText::new(entry.current)?,
            })
        })
        .collect()
}

/// Parses `npm view <name> time --json`.
///
/// # Errors
///
/// Returns an error when JSON or timestamps are invalid.
pub fn parse_time_json(package: &PackageName, raw: &str) -> Result<ReleaseTimeline, NpmError> {
    let timestamps: NpmTimeMap =
        serde_json::from_str(raw).map_err(|err| NpmError::Json(err.to_string()))?;
    if timestamps.is_empty() {
        return Err(NpmError::EmptyTimeMap {
            package: package.as_str().to_owned(),
        });
    }

    let mut entries = Vec::new();
    for (version, timestamp) in timestamps {
        if version == "created" || version == "modified" {
            continue;
        }
        let parsed =
            DateTime::parse_from_rfc3339(&timestamp).map_err(|_| NpmError::InvalidTimestamp {
                version: version.clone(),
                value: timestamp.clone(),
            })?;
        entries.push(ReleaseEntry::new(
            VersionText::new(version)?,
            ReleaseTimestamp::new(system_time_from_datetime(parsed)),
        ));
    }
    if entries.is_empty() {
        return Err(NpmError::EmptyTimeMap {
            package: package.as_str().to_owned(),
        });
    }
    Ok(ReleaseTimeline::new(entries))
}

/// Reads installed npm global packages.
///
/// # Errors
///
/// Returns an error when the command fails or output cannot be parsed.
pub fn installed_global(process: &ProcessRunner) -> Result<Vec<InstalledTool>, NpmError> {
    let output = process.run(
        &CommandSpec::new("npm", ["ls", "-g", "--depth=0", "--json"]),
        &CommandCheck::Success,
    )?;
    parse_installed_json(output.stdout()?)?
        .into_iter()
        .map(installed_tool)
        .collect()
}

/// Reads npm global outdated package names and current versions.
///
/// # Errors
///
/// Returns an error when the command fails unexpectedly or output cannot be parsed.
pub fn outdated_global(process: &ProcessRunner) -> Result<Vec<NpmOutdatedPackage>, NpmError> {
    let output = process.run(
        &CommandSpec::new("npm", ["outdated", "-g", "--json"]),
        &CommandCheck::Allow(vec![1]),
    )?;
    let stdout = output.stdout()?;
    if stdout.trim().is_empty() {
        return Ok(Vec::new());
    }
    parse_outdated_json(stdout)
}

/// Creates update seeds for npm planning.
///
/// # Errors
///
/// Returns an error when installed package discovery fails.
pub fn update_seeds(
    process: &ProcessRunner,
    version_policy: VersionPolicy,
) -> Result<Vec<UpdateSeed>, NpmError> {
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

/// Creates npm commands for a typed selection.
///
/// # Errors
///
/// Returns an error when a selected item is unknown, not executable, or cannot
/// be executed as an exact target when exact execution is required.
pub fn commands_for_selection(
    plan: &UpdatePlan,
    selection: &PlanSelection,
    settings: CommandBuildSettings,
) -> Result<Vec<ManagerExecutionCommand>, NpmError> {
    let mut commands = Vec::new();
    for selected in &selection.selected_items {
        let item = plan
            .item(&selected.plan_item_id)
            .ok_or_else(|| NpmError::UnknownPlanItem(selected.plan_item_id.as_str().to_owned()))?;
        let candidate = executable_candidate(item, selected.forced)?;
        if should_use_native_selected_update(candidate, settings.version_policy, selected.forced) {
            commands.push(ManagerExecutionCommand {
                plan_item_id: selected.plan_item_id.clone(),
                package_name: candidate.package_name.clone(),
                installed_version: candidate.installed_version.clone(),
                target_version: candidate.target_version.clone(),
                command: selected_native_update_command(
                    candidate,
                    whole_days(settings.min_release_age),
                ),
            });
            continue;
        }
        if !candidate.execution_eligibility.supports_exact_target() {
            return Err(NpmError::ExactTargetUnsupported(
                selected.plan_item_id.as_str().to_owned(),
            ));
        }
        commands.push(ManagerExecutionCommand {
            plan_item_id: selected.plan_item_id.clone(),
            package_name: candidate.package_name.clone(),
            installed_version: candidate.installed_version.clone(),
            target_version: candidate.target_version.clone(),
            command: exact_command(
                candidate,
                whole_days(settings.min_release_age),
                selected.forced,
            ),
        });
    }
    Ok(commands)
}

#[must_use]
pub fn exact_command(
    candidate: &UpdateCandidate,
    min_release_age_days: u64,
    bypass_min_release_age: bool,
) -> CommandSpec {
    let spec = format!(
        "{}@{}",
        candidate.package_name.as_str(),
        candidate.target_version.as_str()
    );
    let days = min_release_age_days.to_string();
    let mut args = vec!["install".to_owned(), "-g".to_owned(), spec];
    if !bypass_min_release_age {
        args.push("--min-release-age".to_owned());
        args.push(days);
    }
    CommandSpec::new("npm", args).mutating()
}

#[must_use]
pub fn selected_native_update_command(
    candidate: &UpdateCandidate,
    min_release_age_days: u64,
) -> CommandSpec {
    let days = min_release_age_days.to_string();
    CommandSpec::new(
        "npm",
        [
            "-g",
            "update",
            candidate.package_name.as_str(),
            "--min-release-age",
            &days,
        ],
    )
    .mutating()
}

#[must_use]
pub fn global_update_command(min_release_age_days: u64) -> CommandSpec {
    let days = min_release_age_days.to_string();
    CommandSpec::new("npm", ["-g", "update", "--min-release-age", &days]).mutating()
}

fn should_use_native_selected_update(
    candidate: &UpdateCandidate,
    version_policy: VersionPolicy,
    forced: bool,
) -> bool {
    version_policy == VersionPolicy::None
        && !forced
        && matches!(
            candidate.execution_eligibility,
            upnow_domain::ExecutionEligibility::NativeOrExact
                | upnow_domain::ExecutionEligibility::NativeOnly
        )
}

fn whole_days(duration: Duration) -> u64 {
    duration.as_secs() / 86_400
}

fn manager_id() -> ManagerId {
    ManagerId::new(MANAGER_ID).expect("static npm manager id should be valid")
}

fn installed_tool(package: NpmInstalledPackage) -> Result<InstalledTool, NpmError> {
    Ok(InstalledTool::new(
        manager_id(),
        ToolId::new(package.name.as_str().to_owned())?,
        package.name.clone(),
        ToolName::new(package.name.as_str().to_owned())?,
        package.version,
        ManagerMetadata::empty(),
    ))
}

fn installed_tool_from_outdated(package: NpmOutdatedPackage) -> Result<InstalledTool, NpmError> {
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
) -> Result<ReleaseLookupResult, NpmError> {
    match process.run(
        &CommandSpec::new("npm", ["view", package.as_str(), "time", "--json"]),
        &CommandCheck::Success,
    ) {
        Ok(output) => match output.stdout() {
            Ok(stdout) => match parse_time_json(package, stdout) {
                Ok(timeline) => Ok(ReleaseLookupResult::Known(timeline)),
                Err(NpmError::EmptyTimeMap { .. }) => Ok(ReleaseLookupResult::MissingMetadata),
                Err(err) => Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
                    err.to_string(),
                ))),
            },
            Err(err) => Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
                err.to_string(),
            ))),
        },
        Err(err) if err.is_interruption() => Err(NpmError::from(err)),
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

fn executable_candidate(item: &PlanItem, forced: bool) -> Result<&UpdateCandidate, NpmError> {
    match item {
        PlanItem::Update { candidate, .. } => Ok(candidate),
        PlanItem::Delayed { candidate, .. } if forced => Ok(candidate),
        _ => Err(NpmError::ItemNotExecutable(item.id().as_str().to_owned())),
    }
}

fn adapter_error(err: NpmError) -> ManagerAdapterError {
    let kind = match &err {
        NpmError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        NpmError::Json(_)
        | NpmError::Domain(_)
        | NpmError::InvalidTimestamp { .. }
        | NpmError::EmptyTimeMap { .. } => ManagerAdapterErrorKind::Parse,
        NpmError::UnknownPlanItem(_)
        | NpmError::ItemNotExecutable(_)
        | NpmError::ExactTargetUnsupported(_) => ManagerAdapterErrorKind::CommandConstruction,
        NpmError::Infra(_) => ManagerAdapterErrorKind::Infra,
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
