use std::collections::BTreeMap;
use std::fmt::{self, Display};

use serde::Deserialize;
use upnow_domain::{
    DomainError, InstalledTool, ManagerId, ManagerMetadata, PackageName, ToolId, ToolName,
    UnsupportedReason, UpdateCandidate, VersionPolicy, VersionScheme, VersionText,
};
use upnow_execution::{ExecutionCommandIntent, ResolvedExecutionItem, ResolvedExecutionPlan};
use upnow_infra::{CommandCheck, CommandSpec, InfraError, ProcessRunner};

use crate::adapter::{
    CommandBuildSettings, ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind,
    ManagerCapabilities, ManagerExecutionCommand, ManagerExecutionCommandItem,
    UnsupportedManagerVersion, UpdateDiscovery,
};
use crate::npm_family_release::{NpmRegistryTimeSource, ReleaseLookupRequest};

pub const MANAGER_ID: &str = "yarn";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YarnError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    InvalidMajorVersion(String),
    UnsupportedMajorVersion(u64),
    UnsupportedCommandIntent(String),
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
        update_discoveries(process).map_err(|err| adapter_error(&err))
    }

    fn commands_for_execution_plan(
        &self,
        _process: &ProcessRunner,
        plan: &ResolvedExecutionPlan,
        _settings: CommandBuildSettings,
    ) -> Result<Vec<ManagerExecutionCommand>, ManagerAdapterError> {
        commands_for_execution_plan(plan).map_err(|err| adapter_error(&err))
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
pub fn update_discoveries(process: &ProcessRunner) -> Result<Vec<UpdateDiscovery>, YarnError> {
    installed_global_classic(process)?
        .into_iter()
        .map(update_discovery)
        .collect()
}

/// Creates exact Yarn commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when the resolved execution mode is not supported by Yarn.
pub fn commands_for_execution_plan(
    plan: &ResolvedExecutionPlan,
) -> Result<Vec<ManagerExecutionCommand>, YarnError> {
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
                return Err(YarnError::UnsupportedCommandIntent(
                    "native-selected".to_owned(),
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

#[must_use]
pub fn exact_command(candidate: &UpdateCandidate) -> CommandSpec {
    exact_command_parts(&candidate.package_name, &candidate.target_version)
}

fn exact_command_for_item(item: &ResolvedExecutionItem) -> CommandSpec {
    exact_command_parts(&item.package_name, &item.target_version)
}

fn exact_command_parts(package_name: &PackageName, target_version: &VersionText) -> CommandSpec {
    let spec = format!("{}@{}", package_name.as_str(), target_version.as_str());
    CommandSpec::new("yarn", ["global", "add", &spec]).mutating()
}

fn execution_item(item: &ResolvedExecutionItem) -> ManagerExecutionCommandItem {
    ManagerExecutionCommandItem {
        plan_item_id: item.plan_item_id.clone(),
        package_name: item.package_name.clone(),
        installed_version: item.installed_version.clone(),
        target_version: item.target_version.clone(),
    }
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

fn update_discovery(tool: InstalledTool) -> Result<UpdateDiscovery, YarnError> {
    Ok(UpdateDiscovery {
        release_lookup: release_lookup_request(&tool.package_name),
        installed: tool,
        version_scheme: VersionScheme::SemVer,
    })
}

fn release_lookup_request(package: &PackageName) -> ReleaseLookupRequest {
    ReleaseLookupRequest::NpmRegistryTime {
        source: NpmRegistryTimeSource::YarnClassic,
        package: package.clone(),
    }
}

fn adapter_error(err: &YarnError) -> ManagerAdapterError {
    let detail = err.to_string();
    let kind = match err {
        &YarnError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        &YarnError::Json(_) | &YarnError::Domain(_) | &YarnError::InvalidMajorVersion(_) => {
            ManagerAdapterErrorKind::Parse
        }
        &YarnError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
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
