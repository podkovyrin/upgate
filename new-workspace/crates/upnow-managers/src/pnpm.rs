use std::collections::BTreeMap;
use std::fmt::{self, Display};

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

pub const MANAGER_ID: &str = "pnpm";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PnpmError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    UnsupportedCommandIntent(String),
}

impl Display for PnpmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Infra(detail)
            | Self::Interrupted(detail)
            | Self::Json(detail)
            | Self::Domain(detail) => formatter.write_str(detail),
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

/// Discovers pnpm packages that need release metadata before planning.
///
/// # Errors
///
/// Returns an error when installed package discovery fails.
pub fn update_discoveries(
    process: &ProcessRunner,
    version_policy: VersionPolicy,
) -> Result<Vec<UpdateDiscovery>, PnpmError> {
    let installed = match version_policy {
        VersionPolicy::None => outdated_global(process)?
            .into_iter()
            .map(installed_tool_from_outdated)
            .collect::<Result<Vec<_>, _>>()?,
        VersionPolicy::Stable | VersionPolicy::SameTrack => installed_global(process)?,
    };
    installed.into_iter().map(update_discovery).collect()
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

fn update_discovery(tool: InstalledTool) -> Result<UpdateDiscovery, PnpmError> {
    Ok(UpdateDiscovery {
        release_lookup: release_lookup_request(&tool.package_name),
        installed: tool,
        version_scheme: VersionScheme::SemVer,
    })
}

fn release_lookup_request(package: &PackageName) -> ReleaseLookupRequest {
    ReleaseLookupRequest::NpmRegistryTime {
        source: NpmRegistryTimeSource::Pnpm,
        package: package.clone(),
    }
}

fn adapter_error(err: PnpmError) -> ManagerAdapterError {
    let kind = match &err {
        PnpmError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        PnpmError::Json(_) | PnpmError::Domain(_) => ManagerAdapterErrorKind::Parse,
        PnpmError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        PnpmError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail: err.to_string(),
    }
}
