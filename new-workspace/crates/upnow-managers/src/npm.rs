use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::time::Duration;

use serde::Deserialize;
use upnow_domain::{
    DomainError, InstalledTool, ManagerId, ManagerMetadata, PackageName, ToolId, ToolName,
    UpdateCandidate, VersionPolicy, VersionScheme, VersionText,
};
use upnow_execution::{ExecutionCommandIntent, ResolvedExecutionItem, ResolvedExecutionPlan};
use upnow_infra::{CommandCheck, CommandSpec, InfraError, ProcessRunner};

use crate::adapter::{
    CommandBuildSettings, ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind,
    ManagerCapabilities, ManagerExecutionCommand, ManagerExecutionCommandItem, UpdateDiscovery,
};
use crate::npm_family_release::{NpmRegistryTimeSource, ReleaseLookupRequest};

pub const MANAGER_ID: &str = "npm";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NpmError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    UnsupportedCommandIntent(String),
}

impl Display for NpmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Infra(detail)
            | Self::Interrupted(detail)
            | Self::Json(detail)
            | Self::Domain(detail) => formatter.write_str(detail),
            Self::UnsupportedCommandIntent(kind) => {
                write!(
                    formatter,
                    "unsupported npm execution command intent `{kind}`"
                )
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

    fn release_lookup_request(
        &self,
        _process: &ProcessRunner,
        package: &PackageName,
    ) -> Result<ReleaseLookupRequest, ManagerAdapterError> {
        Ok(release_lookup_request(package))
    }

    fn update_discoveries(
        &self,
        process: &ProcessRunner,
        version_policy: VersionPolicy,
    ) -> Result<Vec<UpdateDiscovery>, ManagerAdapterError> {
        self.validate_version_policy(version_policy)?;
        update_discoveries(process, version_policy).map_err(adapter_error)
    }

    fn commands_for_execution_plan(
        &self,
        _process: &ProcessRunner,
        plan: &ResolvedExecutionPlan,
        settings: CommandBuildSettings,
    ) -> Result<Vec<ManagerExecutionCommand>, ManagerAdapterError> {
        commands_for_execution_plan(plan, settings).map_err(adapter_error)
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

/// Discovers npm packages that need release metadata before planning.
///
/// # Errors
///
/// Returns an error when installed package discovery fails.
pub fn update_discoveries(
    process: &ProcessRunner,
    version_policy: VersionPolicy,
) -> Result<Vec<UpdateDiscovery>, NpmError> {
    let installed = match version_policy {
        VersionPolicy::None => outdated_global(process)?
            .into_iter()
            .map(installed_tool_from_outdated)
            .collect::<Result<Vec<_>, _>>()?,
        VersionPolicy::Stable | VersionPolicy::SameTrack => installed_global(process)?,
    };
    installed.into_iter().map(update_discovery).collect()
}

/// Creates npm commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when the resolved execution mode is not supported by npm.
pub fn commands_for_execution_plan(
    plan: &ResolvedExecutionPlan,
    settings: CommandBuildSettings,
) -> Result<Vec<ManagerExecutionCommand>, NpmError> {
    let mut commands = Vec::new();
    for intent in &plan.intents {
        match intent {
            ExecutionCommandIntent::NativeSelected(item) => {
                commands.push(ManagerExecutionCommand {
                    items: vec![execution_item(item)],
                    command: selected_native_update_command_for_item(
                        item,
                        whole_days(settings.min_release_age),
                    ),
                });
            }
            ExecutionCommandIntent::Exact(item) => {
                commands.push(ManagerExecutionCommand {
                    items: vec![execution_item(item)],
                    command: exact_command_for_item(
                        item,
                        whole_days(settings.min_release_age),
                        item.forced,
                    ),
                });
            }
            ExecutionCommandIntent::NativeGlobal(_) => {
                return Err(NpmError::UnsupportedCommandIntent(
                    "native-global".to_owned(),
                ));
            }
        }
    }
    Ok(commands)
}

#[must_use]
pub fn exact_command(
    candidate: &UpdateCandidate,
    min_release_age_days: u64,
    bypass_min_release_age: bool,
) -> CommandSpec {
    exact_command_parts(
        &candidate.package_name,
        &candidate.target_version,
        min_release_age_days,
        bypass_min_release_age,
    )
}

#[must_use]
pub fn selected_native_update_command(
    candidate: &UpdateCandidate,
    min_release_age_days: u64,
) -> CommandSpec {
    selected_native_update_command_parts(&candidate.package_name, min_release_age_days)
}

#[must_use]
pub fn global_update_command(min_release_age_days: u64) -> CommandSpec {
    let days = min_release_age_days.to_string();
    CommandSpec::new("npm", ["-g", "update", "--min-release-age", &days]).mutating()
}

fn execution_item(item: &ResolvedExecutionItem) -> ManagerExecutionCommandItem {
    ManagerExecutionCommandItem {
        plan_item_id: item.plan_item_id.clone(),
        package_name: item.package_name.clone(),
        installed_version: item.installed_version.clone(),
        target_version: item.target_version.clone(),
    }
}

fn exact_command_for_item(
    item: &ResolvedExecutionItem,
    min_release_age_days: u64,
    bypass_min_release_age: bool,
) -> CommandSpec {
    exact_command_parts(
        &item.package_name,
        &item.target_version,
        min_release_age_days,
        bypass_min_release_age,
    )
}

fn exact_command_parts(
    package_name: &PackageName,
    target_version: &VersionText,
    min_release_age_days: u64,
    bypass_min_release_age: bool,
) -> CommandSpec {
    let spec = format!("{}@{}", package_name.as_str(), target_version.as_str());
    let days = min_release_age_days.to_string();
    let mut args = vec!["install".to_owned(), "-g".to_owned(), spec];
    if !bypass_min_release_age {
        args.push("--min-release-age".to_owned());
        args.push(days);
    }
    CommandSpec::new("npm", args).mutating()
}

fn selected_native_update_command_for_item(
    item: &ResolvedExecutionItem,
    min_release_age_days: u64,
) -> CommandSpec {
    selected_native_update_command_parts(&item.package_name, min_release_age_days)
}

fn selected_native_update_command_parts(
    package_name: &PackageName,
    min_release_age_days: u64,
) -> CommandSpec {
    let days = min_release_age_days.to_string();
    CommandSpec::new(
        "npm",
        [
            "-g",
            "update",
            package_name.as_str(),
            "--min-release-age",
            &days,
        ],
    )
    .mutating()
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

fn update_discovery(tool: InstalledTool) -> Result<UpdateDiscovery, NpmError> {
    Ok(UpdateDiscovery {
        release_lookup: release_lookup_request(&tool.package_name),
        installed: tool,
        version_scheme: VersionScheme::SemVer,
    })
}

fn release_lookup_request(package: &PackageName) -> ReleaseLookupRequest {
    ReleaseLookupRequest::NpmRegistryTime {
        source: NpmRegistryTimeSource::Npm,
        package: package.clone(),
    }
}

fn adapter_error(err: NpmError) -> ManagerAdapterError {
    let kind = match &err {
        NpmError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        NpmError::Json(_) | NpmError::Domain(_) => ManagerAdapterErrorKind::Parse,
        NpmError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        NpmError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail: err.to_string(),
    }
}
