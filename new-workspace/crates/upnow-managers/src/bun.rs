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

pub const MANAGER_ID: &str = "bun";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BunError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    UnsupportedCommandIntent(String),
}

impl Display for BunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Infra(detail)
            | Self::Interrupted(detail)
            | Self::Json(detail)
            | Self::Domain(detail) => formatter.write_str(detail),
            Self::UnsupportedCommandIntent(kind) => {
                write!(
                    formatter,
                    "unsupported bun execution command intent `{kind}`"
                )
            }
        }
    }
}

impl std::error::Error for BunError {}

impl From<InfraError> for BunError {
    fn from(value: InfraError) -> Self {
        if value.is_interruption() {
            Self::Interrupted(value.to_string())
        } else {
            Self::Infra(value.to_string())
        }
    }
}

impl From<DomainError> for BunError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
    }
}

impl BunError {
    #[must_use]
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BunInstalledPackage {
    pub name: PackageName,
    pub version: VersionText,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BunPmLsJson {
    Root(BunPmLsRoot),
    Roots(Vec<BunPmLsRoot>),
}

#[derive(Debug, Deserialize)]
struct BunPmLsRoot {
    #[serde(default)]
    dependencies: BTreeMap<String, BunPmDependency>,
}

#[derive(Debug, Deserialize)]
struct BunPmDependency {
    version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BunManager;

impl ManagerAdapter for BunManager {
    fn id(&self) -> &'static str {
        MANAGER_ID
    }

    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new(true, false).with_native_global_update(true)
    }

    fn supports_version_policy(&self, _policy: VersionPolicy) -> bool {
        true
    }

    fn installed_tools(
        &self,
        process: &ProcessRunner,
    ) -> Result<Vec<InstalledTool>, ManagerAdapterError> {
        installed_global(process).map_err(|err| adapter_error(&err))
    }

    fn release_lookup_request(
        &self,
        process: &ProcessRunner,
        package: &PackageName,
    ) -> Result<ReleaseLookupRequest, ManagerAdapterError> {
        Ok(release_lookup_request(process, package))
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
        process: &ProcessRunner,
        plan: &ResolvedExecutionPlan,
        settings: CommandBuildSettings,
    ) -> Result<Vec<ManagerExecutionCommand>, ManagerAdapterError> {
        commands_for_execution_plan(process, plan, settings.min_release_age)
            .map_err(|err| adapter_error(&err))
    }
}

/// Parses `bun pm ls -g --json`.
///
/// # Errors
///
/// Returns an error when JSON is malformed or a package/version is blank.
pub fn parse_pm_ls_json(raw: &str) -> Result<Vec<BunInstalledPackage>, BunError> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let parsed: BunPmLsJson =
        serde_json::from_str(raw).map_err(|err| BunError::Json(err.to_string()))?;
    let roots = match parsed {
        BunPmLsJson::Root(root) => vec![root],
        BunPmLsJson::Roots(roots) => roots,
    };
    let mut packages = BTreeMap::new();
    for root in roots {
        for (name, dependency) in root.dependencies {
            if let Some(version) = dependency.version {
                packages.insert(name, version);
            }
        }
    }
    packages
        .into_iter()
        .map(|(name, version)| {
            Ok(BunInstalledPackage {
                name: PackageName::new(name)?,
                version: VersionText::new(version)?,
            })
        })
        .collect()
}

#[must_use]
pub fn is_missing_global_manifest(text: &str) -> bool {
    text.contains("missing package.json")
        || text.contains("MissingPackageJSON")
        || text.contains("No package.json was found for directory")
        || text.contains("missing lockfile, nothing outdated")
        || text.contains("Lockfile not found")
}

/// Reads installed Bun global packages.
///
/// # Errors
///
/// Returns an error when the command fails unexpectedly or output cannot be parsed.
pub fn installed_global(process: &ProcessRunner) -> Result<Vec<InstalledTool>, BunError> {
    let runtime = BunRuntime::resolve(process);
    installed_global_with_bun(process, runtime.executable())
}

fn installed_global_with_bun(
    process: &ProcessRunner,
    bun: &str,
) -> Result<Vec<InstalledTool>, BunError> {
    let output = process.run(
        &CommandSpec::new(bun, ["pm", "ls", "-g", "--json"]),
        &CommandCheck::IgnoreStatus,
    )?;
    let stdout = output.stdout()?;
    let stderr = output.stderr().unwrap_or_default();
    if is_missing_global_manifest(stdout) || is_missing_global_manifest(stderr) {
        return Ok(Vec::new());
    }
    if !output.status().success() {
        if output.status().code().is_none() {
            return Err(BunError::Interrupted(
                "bun pm ls -g --json failed (exit signal)".to_owned(),
            ));
        }
        let detail = if stderr.trim().is_empty() {
            stdout.to_owned()
        } else {
            stderr.to_owned()
        };
        return Err(BunError::Infra(format!(
            "bun pm ls -g --json failed: {detail}"
        )));
    }
    parse_pm_ls_json(stdout)?
        .into_iter()
        .map(installed_tool)
        .collect()
}

/// Discovers Bun packages that need release metadata before planning.
///
/// # Errors
///
/// Returns an error when discovery fails.
pub fn update_discoveries(process: &ProcessRunner) -> Result<Vec<UpdateDiscovery>, BunError> {
    let runtime = BunRuntime::resolve(process);
    installed_global_with_bun(process, runtime.executable())?
        .into_iter()
        .map(|tool| update_discovery(tool, runtime.executable()))
        .collect()
}

/// Creates Bun commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when the resolved execution mode is not supported by Bun.
pub fn commands_for_execution_plan(
    process: &ProcessRunner,
    plan: &ResolvedExecutionPlan,
    min_release_age: Duration,
) -> Result<Vec<ManagerExecutionCommand>, BunError> {
    let runtime = BunRuntime::resolve(process);
    let mut commands = Vec::new();
    for intent in &plan.intents {
        match intent {
            ExecutionCommandIntent::NativeGlobal(items) => {
                commands.push(ManagerExecutionCommand {
                    items: items.iter().map(execution_item).collect(),
                    command: global_update_command(runtime.executable(), min_release_age),
                });
            }
            ExecutionCommandIntent::Exact(item) => {
                commands.push(ManagerExecutionCommand {
                    items: vec![execution_item(item)],
                    command: exact_command_with_program(
                        runtime.executable(),
                        &item.package_name,
                        &item.target_version,
                        min_release_age,
                        item.forced,
                    ),
                });
            }
            ExecutionCommandIntent::NativeSelected(_) => {
                return Err(BunError::UnsupportedCommandIntent(
                    "native-selected".to_owned(),
                ));
            }
        }
    }
    Ok(commands)
}

#[must_use]
pub fn exact_command(
    candidate: &UpdateCandidate,
    min_release_age: Duration,
    bypass_min_release_age: bool,
) -> CommandSpec {
    exact_command_with_program(
        "bun",
        &candidate.package_name,
        &candidate.target_version,
        min_release_age,
        bypass_min_release_age,
    )
}

#[must_use]
pub fn exact_command_with_program(
    bun: &str,
    package_name: &PackageName,
    target_version: &VersionText,
    min_release_age: Duration,
    bypass_min_release_age: bool,
) -> CommandSpec {
    let spec = format!("{}@{}", package_name.as_str(), target_version.as_str());
    let min_age_secs = min_release_age.as_secs().to_string();
    let mut args = vec!["update".to_owned(), "-g".to_owned(), spec];
    if !bypass_min_release_age {
        args.push("--minimum-release-age".to_owned());
        args.push(min_age_secs);
    }
    CommandSpec::new(bun, args).mutating()
}

#[must_use]
pub fn global_update_command(bun: &str, min_release_age: Duration) -> CommandSpec {
    let min_age_secs = min_release_age.as_secs().to_string();
    CommandSpec::new(
        bun,
        [
            "update",
            "-g",
            "--minimum-release-age",
            min_age_secs.as_str(),
        ],
    )
    .mutating()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BunRuntime {
    executable: String,
}

impl BunRuntime {
    fn resolve(process: &ProcessRunner) -> Self {
        Self {
            executable: bun_executable(process),
        }
    }

    fn executable(&self) -> &str {
        &self.executable
    }
}

fn execution_item(item: &ResolvedExecutionItem) -> ManagerExecutionCommandItem {
    ManagerExecutionCommandItem {
        plan_item_id: item.plan_item_id.clone(),
        package_name: item.package_name.clone(),
        installed_version: item.installed_version.clone(),
        target_version: item.target_version.clone(),
    }
}

fn bun_executable(process: &ProcessRunner) -> String {
    if let Ok(path) = std::env::var("UPNOW_BUN_BIN")
        && let Some(trimmed) = trim_non_empty(&path)
    {
        return trimmed.to_owned();
    }
    match process.run(
        &CommandSpec::new("mise", ["which", "bun"]),
        &CommandCheck::Success,
    ) {
        Ok(output) => output
            .stdout()
            .ok()
            .and_then(trim_non_empty)
            .map_or_else(|| MANAGER_ID.to_owned(), ToOwned::to_owned),
        Err(_) => MANAGER_ID.to_owned(),
    }
}

fn trim_non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn manager_id() -> ManagerId {
    ManagerId::new(MANAGER_ID).expect("static bun manager id should be valid")
}

fn installed_tool(package: BunInstalledPackage) -> Result<InstalledTool, BunError> {
    Ok(InstalledTool::new(
        manager_id(),
        ToolId::new(package.name.as_str().to_owned())?,
        package.name.clone(),
        ToolName::new(package.name.as_str().to_owned())?,
        package.version,
        ManagerMetadata::empty(),
    ))
}

fn update_discovery(tool: InstalledTool, executable: &str) -> Result<UpdateDiscovery, BunError> {
    Ok(UpdateDiscovery {
        release_lookup: ReleaseLookupRequest::NpmRegistryTime {
            source: NpmRegistryTimeSource::Bun {
                executable: executable.to_owned(),
            },
            package: tool.package_name.clone(),
        },
        installed: tool,
        version_scheme: VersionScheme::SemVer,
    })
}

fn release_lookup_request(process: &ProcessRunner, package: &PackageName) -> ReleaseLookupRequest {
    let runtime = BunRuntime::resolve(process);
    ReleaseLookupRequest::NpmRegistryTime {
        source: NpmRegistryTimeSource::Bun {
            executable: runtime.executable,
        },
        package: package.clone(),
    }
}

fn adapter_error(err: &BunError) -> ManagerAdapterError {
    let detail = err.to_string();
    let kind = match err {
        &BunError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        &BunError::Json(_) | &BunError::Domain(_) => ManagerAdapterErrorKind::Parse,
        &BunError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        &BunError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail,
    }
}
