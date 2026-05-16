use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::{self, Display};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use semver::Version;
use serde::Deserialize;
use upnow_domain::{
    DomainError, ExecutionSupport, InstalledTool, ManagerConfig, ManagerId, ManagerMetadata,
    ManagerRuleReason, ManagerScanEvidenceInput, ManagerScanInput, ManagerUpdateInput, PackageName,
    ReleaseEntry, ReleaseEvidenceSource, ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline,
    ReleaseTimestamp, ScanIssue, SkipReason, ToolId, ToolName, UpdateSeed, VersionScheme,
    VersionText,
};
use upnow_execution::{
    ExecutionCommand, ExecutionCommandIntent, ExecutionCommandItem, ResolvedExecutionItem,
    ResolvedExecutionPlan,
};
use upnow_infra::{
    CommandCheck, CommandSpec, Env, HttpClient, InfraError, ProcessRunner, effective_parallelism,
    run_ordered_parallel,
};
use upnow_release::release_evidence_for_version;

use crate::adapter::{
    ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind, ManagerCapabilities,
    ReleaseLookupSubject, validate_version_policy,
};

pub const MANAGER_ID: &str = "go";
const GO_MAX_PARALLEL_CHECKS: usize = 4;

const MISSING_BUILD_METADATA: &str = "missing go build metadata";
const MISSING_MODULE_METADATA: &str = "missing go module/version metadata";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    Io(String),
    MissingHome,
    InvalidTimestamp { version: String, value: String },
    MissingReleaseMetadata(String),
    UnsupportedCommandIntent(String),
    MissingInstallPath(String),
}

impl Display for GoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Infra(detail)
            | Self::Interrupted(detail)
            | Self::Json(detail)
            | Self::Domain(detail)
            | Self::Io(detail)
            | Self::MissingReleaseMetadata(detail) => formatter.write_str(detail),
            Self::MissingHome => formatter.write_str("HOME env var is not set"),
            Self::InvalidTimestamp { version, value } => {
                write!(
                    formatter,
                    "invalid Go module release timestamp `{value}` for version `{version}`"
                )
            }
            Self::UnsupportedCommandIntent(kind) => {
                write!(
                    formatter,
                    "unsupported go execution command intent `{kind}`"
                )
            }
            Self::MissingInstallPath(package) => {
                write!(formatter, "missing Go install path for `{package}`")
            }
        }
    }
}

impl std::error::Error for GoError {}

impl From<InfraError> for GoError {
    fn from(value: InfraError) -> Self {
        if value.is_interruption() {
            Self::Interrupted(value.to_string())
        } else {
            Self::Infra(value.to_string())
        }
    }
}

impl From<DomainError> for GoError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
    }
}

impl GoError {
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoManagedTool {
    pub binary_name: PackageName,
    pub install_path: String,
    pub module_path: String,
    pub current_version: VersionText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoDiscoveredTool {
    Managed(GoManagedTool),
    Skipped { name: PackageName, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoBuildInfo {
    pub install_path: String,
    pub module_path: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoManager {
    config: ManagerConfig,
}

impl GoManager {
    pub const fn new(config: ManagerConfig) -> Self {
        Self { config }
    }

    pub fn id() -> ManagerId {
        ManagerId::from_static(MANAGER_ID)
    }
}
impl ManagerAdapter for GoManager {
    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new()
    }
    fn scan_inputs(
        &self,
        process: &ProcessRunner,
        env: &Env,
    ) -> Result<Vec<ManagerScanInput>, ManagerAdapterError> {
        scan_inputs(process, env).map_err(|err| adapter_error(&err))
    }

    fn scan_inputs_with_release_evidence(
        &self,
        process: &ProcessRunner,
        _http: &HttpClient,
        env: &Env,
        _max_parallel_checks_per_manager: usize,
    ) -> Result<Vec<ManagerScanEvidenceInput>, ManagerAdapterError> {
        let discovered = discover_global_tools(process, env).map_err(|err| adapter_error(&err))?;
        discovered
            .into_iter()
            .map(|discovered| match discovered {
                GoDiscoveredTool::Managed(tool) => {
                    let installed = installed_tool(&tool);
                    match lookup_release_by_module(process, &tool.module_path)
                        .map_err(|err| adapter_error(&err))?
                    {
                        ReleaseLookupResult::Known(timeline) => {
                            Ok(ManagerScanEvidenceInput::Installed {
                                release_evidence: release_evidence_for_version(
                                    &timeline,
                                    &installed.installed_version,
                                    ReleaseEvidenceSource::ReleaseTimeline,
                                ),
                                tool: installed,
                            })
                        }
                        ReleaseLookupResult::MissingMetadata
                        | ReleaseLookupResult::LookupFailed(_) => {
                            Ok(ManagerScanEvidenceInput::Installed {
                                tool: installed,
                                release_evidence: None,
                            })
                        }
                    }
                }
                GoDiscoveredTool::Skipped { name, reason } => {
                    Ok(ManagerScanEvidenceInput::Skipped {
                        installed: placeholder_installed_tool(&name)
                            .map_err(|err| adapter_error(&err))?,
                        reason: ScanIssue::ExcludedByManagerRule(ManagerRuleReason::Other {
                            detail: reason,
                        }),
                    })
                }
            })
            .collect()
    }

    fn release_lookup(
        &self,
        process: &ProcessRunner,
        _http: &HttpClient,
        env: &Env,
        subject: ReleaseLookupSubject<'_>,
    ) -> Result<ReleaseLookupResult, ManagerAdapterError> {
        match subject {
            ReleaseLookupSubject::Package(package) => lookup_release(process, env, package),
            ReleaseLookupSubject::Installed(tool) => lookup_installed_release(process, env, tool),
        }
        .map_err(|err| adapter_error(&err))
    }

    fn update_inputs(
        &self,
        process: &ProcessRunner,
        _http: &HttpClient,
        env: &Env,
        max_parallel_checks_per_manager: usize,
    ) -> Result<Vec<ManagerUpdateInput>, ManagerAdapterError> {
        validate_version_policy(
            &self.config.manager_id,
            Self::supports_version_policy(self.config.version_policy),
            self.config.version_policy,
        )?;
        update_inputs(process, env, max_parallel_checks_per_manager)
            .map_err(|err| adapter_error(&err))
    }

    fn commands_for_execution_plan(
        &self,
        process: &ProcessRunner,
        env: &Env,
        plan: &ResolvedExecutionPlan,
    ) -> Result<Vec<ExecutionCommand>, ManagerAdapterError> {
        commands_for_execution_plan(process, env, plan).map_err(|err| adapter_error(&err))
    }
}

/// Parses `go version -m <binary>` output.
fn parse_go_version_m_output(text: &str) -> Option<GoBuildInfo> {
    let mut install_path = None::<String>;
    let mut module_path = None::<String>;
    let mut version = None::<String>;

    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("path") => install_path = parts.next().map(ToOwned::to_owned),
            Some("mod") => {
                module_path = parts.next().map(ToOwned::to_owned);
                version = parts.next().map(ToOwned::to_owned);
            }
            _ => {}
        }
    }

    let version = version?;
    if matches!(version.as_str(), "(devel)" | "devel") {
        return None;
    }

    Some(GoBuildInfo {
        install_path: install_path?,
        module_path: module_path?,
        version,
    })
}

/// Parses `go list -m -json -versions <module>`.
///
/// # Errors
///
/// Returns an error when the JSON is malformed.
fn parse_module_versions_json(raw: &str) -> Result<Vec<String>, GoError> {
    let parsed: GoListVersionsResponse =
        serde_json::from_str(raw).map_err(|err| GoError::Json(err.to_string()))?;
    Ok(parsed.versions)
}

/// Parses `go list -m -json <module>@<version>`.
///
/// # Errors
///
/// Returns an error when JSON or the timestamp is invalid.
fn parse_module_time_json(version: &str, raw: &str) -> Result<Option<SystemTime>, GoError> {
    let parsed: GoListModuleResponse =
        serde_json::from_str(raw).map_err(|err| GoError::Json(err.to_string()))?;
    let Some(time_raw) = parsed.time.as_deref() else {
        return Ok(None);
    };
    let parsed = DateTime::parse_from_rfc3339(time_raw).map_err(|_| GoError::InvalidTimestamp {
        version: version.to_owned(),
        value: time_raw.to_owned(),
    })?;
    Ok(Some(system_time_from_datetime(parsed)))
}

/// Discovers Go global binaries and their module metadata.
///
/// # Errors
///
/// Returns an error when the Go bin directory cannot be inspected.
pub fn discover_global_tools(
    process: &ProcessRunner,
    env: &Env,
) -> Result<Vec<GoDiscoveredTool>, GoError> {
    let bin_dir = go_bin_dir(process, env)?;
    if !bin_dir.exists() {
        return Ok(Vec::new());
    }

    let mut paths = Vec::new();
    for entry in fs::read_dir(&bin_dir)
        .map_err(|err| GoError::Io(format!("failed to read {}: {err}", bin_dir.display())))?
    {
        let entry = entry.map_err(|err| GoError::Io(err.to_string()))?;
        let path = entry.path();
        if path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();

    let mut discovered = Vec::new();
    for path in paths {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let package = PackageName::new(name.to_owned())?;
        let output = process.run(
            &CommandSpec::new(
                "go",
                [
                    OsString::from("version"),
                    OsString::from("-m"),
                    path.as_os_str().to_os_string(),
                ],
            ),
            &CommandCheck::IgnoreStatus,
        )?;
        if !output.status().success() {
            discovered.push(GoDiscoveredTool::Skipped {
                name: package,
                reason: MISSING_BUILD_METADATA.to_owned(),
            });
            continue;
        }
        let Some(info) = parse_go_version_m_output(output.stdout()?) else {
            discovered.push(GoDiscoveredTool::Skipped {
                name: package,
                reason: MISSING_MODULE_METADATA.to_owned(),
            });
            continue;
        };
        if parse_go_semver(&info.version).is_none() {
            discovered.push(GoDiscoveredTool::Skipped {
                name: package,
                reason: format!("unsupported Go module version '{}'", info.version),
            });
            continue;
        }
        discovered.push(GoDiscoveredTool::Managed(GoManagedTool {
            binary_name: package,
            install_path: info.install_path,
            module_path: info.module_path,
            current_version: VersionText::new(info.version)?,
        }));
    }

    discovered.sort_by(|left, right| discovered_name(left).cmp(discovered_name(right)));
    Ok(discovered)
}

/// Builds scan inputs for Go.
///
/// # Errors
///
/// Returns an error when discovery fails.
pub fn scan_inputs(process: &ProcessRunner, env: &Env) -> Result<Vec<ManagerScanInput>, GoError> {
    discover_global_tools(process, env)?
        .into_iter()
        .map(scan_input)
        .collect()
}

/// Builds planning inputs for Go.
///
/// # Errors
///
/// Returns an error when discovery fails.
pub fn update_inputs(
    process: &ProcessRunner,
    env: &Env,
    max_parallel_checks_per_manager: usize,
) -> Result<Vec<ManagerUpdateInput>, GoError> {
    let discovered = discover_global_tools(process, env)?;
    let threads = effective_parallelism(max_parallel_checks_per_manager, GO_MAX_PARALLEL_CHECKS);
    run_ordered_parallel(
        discovered,
        threads,
        MANAGER_ID,
        |discovered| match discovered {
            GoDiscoveredTool::Managed(tool) => {
                let lookup = lookup_release_by_module(process, &tool.module_path)?;
                Ok(update_input(&tool, lookup))
            }
            GoDiscoveredTool::Skipped { name, reason } => Ok(ManagerUpdateInput::Skipped {
                installed: placeholder_installed_tool(&name)?,
                reason: SkipReason::ManagerRule(reason),
            }),
        },
    )?
    .into_iter()
    .collect()
}

/// Looks up release metadata for a Go package name by rediscovering its module.
///
/// # Errors
///
/// Returns an error when discovery is interrupted.
pub fn lookup_release(
    process: &ProcessRunner,
    env: &Env,
    package: &PackageName,
) -> Result<ReleaseLookupResult, GoError> {
    for discovered in discover_global_tools(process, env)? {
        let GoDiscoveredTool::Managed(tool) = discovered else {
            continue;
        };
        if tool.binary_name == *package {
            return lookup_release_by_module(process, &tool.module_path);
        }
    }
    Ok(ReleaseLookupResult::MissingMetadata)
}

/// Looks up release metadata for a Go module path.
///
/// # Errors
///
/// Returns an error only when command execution is interrupted.
pub fn lookup_release_by_module(
    process: &ProcessRunner,
    module_path: &str,
) -> Result<ReleaseLookupResult, GoError> {
    let versions = match module_versions(process, module_path) {
        Ok(versions) => versions,
        Err(err) if err.is_interruption() => return Err(err),
        Err(err) => {
            return Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
                err.to_string(),
            )));
        }
    };

    let mut entries = Vec::new();
    for version in versions {
        if parse_go_semver(&version).is_none() {
            continue;
        }
        match module_version_time(process, module_path, &version) {
            Ok(Some(published_at)) => entries.push(ReleaseEntry::new(
                VersionText::new(version)?,
                ReleaseTimestamp::new(published_at),
            )),
            Ok(None) => {}
            Err(err) if err.is_interruption() => return Err(err),
            Err(err) => {
                return Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
                    err.to_string(),
                )));
            }
        }
    }

    if entries.is_empty() {
        Ok(ReleaseLookupResult::MissingMetadata)
    } else {
        Ok(ReleaseLookupResult::Known(ReleaseTimeline::new(entries)))
    }
}

/// Creates Go install commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when an execution intent is unsupported or install path is unavailable.
pub fn commands_for_execution_plan(
    process: &ProcessRunner,
    env: &Env,
    plan: &ResolvedExecutionPlan,
) -> Result<Vec<ExecutionCommand>, GoError> {
    let install_paths = install_paths_by_package(process, env)?;
    let mut commands = Vec::new();
    for intent in &plan.intents {
        match intent {
            ExecutionCommandIntent::Exact(item) => {
                commands.push(ExecutionCommand {
                    items: vec![ExecutionCommandItem::from(item)],
                    command: exact_command_for_item(item, &install_paths)?,
                });
            }
            ExecutionCommandIntent::NativeSelected(_) => {
                return Err(GoError::UnsupportedCommandIntent(
                    "native-selected".to_owned(),
                ));
            }
            ExecutionCommandIntent::ResolverNative(_) => {
                return Err(GoError::UnsupportedCommandIntent(
                    "resolver-native".to_owned(),
                ));
            }
            ExecutionCommandIntent::ResolverNativeGlobal(_) => {
                return Err(GoError::UnsupportedCommandIntent(
                    "resolver-native-global".to_owned(),
                ));
            }
            ExecutionCommandIntent::GroupedNative(_) => {
                return Err(GoError::UnsupportedCommandIntent(
                    "grouped-native".to_owned(),
                ));
            }
            ExecutionCommandIntent::NativeGlobal(_) => {
                return Err(GoError::UnsupportedCommandIntent(
                    "native-global".to_owned(),
                ));
            }
        }
    }
    Ok(commands)
}
fn exact_command(install_path: &str, target: &VersionText) -> CommandSpec {
    let spec = format!("{install_path}@{}", target.as_str());
    CommandSpec::new("go", ["install", &spec]).mutating()
}

fn module_versions(process: &ProcessRunner, module_path: &str) -> Result<Vec<String>, GoError> {
    let output = process.run(
        &CommandSpec::new("go", ["list", "-m", "-json", "-versions", module_path]),
        &CommandCheck::Success,
    )?;
    parse_module_versions_json(output.stdout()?)
}

fn module_version_time(
    process: &ProcessRunner,
    module_path: &str,
    version: &str,
) -> Result<Option<SystemTime>, GoError> {
    let module_spec = format!("{module_path}@{version}");
    let output = process.run(
        &CommandSpec::new("go", ["list", "-m", "-json", &module_spec]),
        &CommandCheck::Success,
    )?;
    parse_module_time_json(version, output.stdout()?)
}

fn go_bin_dir(process: &ProcessRunner, env: &Env) -> Result<PathBuf, GoError> {
    if let Some(gobin) = env.non_empty_path_var("GOBIN") {
        return Ok(gobin);
    }
    if let Some(gopath) = env
        .non_empty_var("GOPATH")
        .as_deref()
        .and_then(first_path_entry)
    {
        return Ok(gopath.join("bin"));
    }
    if let Ok(output) = process.run(
        &CommandSpec::new("go", ["env", "-json", "GOPATH"]),
        &CommandCheck::Success,
    ) && let Ok(parsed) = output.json::<GoEnvJson>()
        && let Some(gopath) = parsed.gopath.as_deref().and_then(first_path_entry)
    {
        return Ok(gopath.join("bin"));
    }
    Ok(env
        .home_dir()
        .ok_or(GoError::MissingHome)?
        .join("go")
        .join("bin"))
}

fn first_path_entry(raw: &str) -> Option<PathBuf> {
    std::env::split_paths(raw.trim()).next()
}

fn installed_tool(tool: &GoManagedTool) -> InstalledTool {
    InstalledTool::new(
        GoManager::id(),
        ToolId::new(tool.binary_name.as_str().to_owned()).expect("valid package is valid tool id"),
        tool.binary_name.clone(),
        ToolName::new(tool.binary_name.as_str().to_owned())
            .expect("valid package is valid tool name"),
        tool.current_version.clone(),
        ManagerMetadata::empty(),
    )
}

fn placeholder_installed_tool(name: &PackageName) -> Result<InstalledTool, GoError> {
    Ok(InstalledTool::new(
        GoManager::id(),
        ToolId::new(name.as_str().to_owned())?,
        name.clone(),
        ToolName::new(name.as_str().to_owned())?,
        VersionText::new("*")?,
        ManagerMetadata::empty(),
    ))
}

fn scan_input(discovered: GoDiscoveredTool) -> Result<ManagerScanInput, GoError> {
    match discovered {
        GoDiscoveredTool::Managed(tool) => Ok(ManagerScanInput::Installed(installed_tool(&tool))),
        GoDiscoveredTool::Skipped { name, reason } => Ok(ManagerScanInput::Skipped {
            installed: placeholder_installed_tool(&name)?,
            reason: ScanIssue::ExcludedByManagerRule(ManagerRuleReason::Other { detail: reason }),
        }),
    }
}

fn update_input(tool: &GoManagedTool, lookup: ReleaseLookupResult) -> ManagerUpdateInput {
    let discovered_target = match &lookup {
        ReleaseLookupResult::Known(timeline) => {
            newest_go_semver_version(timeline).unwrap_or_else(|| tool.current_version.clone())
        }
        ReleaseLookupResult::MissingMetadata | ReleaseLookupResult::LookupFailed(_) => {
            tool.current_version.clone()
        }
    };
    ManagerUpdateInput::Seed(UpdateSeed::new(
        installed_tool(tool),
        discovered_target,
        VersionScheme::SemVer,
        lookup,
        ExecutionSupport::exact_only(),
    ))
}

fn newest_go_semver_version(timeline: &ReleaseTimeline) -> Option<VersionText> {
    timeline
        .versions
        .iter()
        .filter_map(|entry| {
            parse_go_semver(entry.version.as_str()).map(|version| (version, entry.version.clone()))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, version)| version)
}

fn parse_go_semver(raw: &str) -> Option<Version> {
    Version::parse(raw.strip_prefix(['v', 'V']).unwrap_or(raw)).ok()
}

fn install_paths_by_package(
    process: &ProcessRunner,
    env: &Env,
) -> Result<BTreeMap<PackageName, String>, GoError> {
    let mut paths = BTreeMap::new();
    for discovered in discover_global_tools(process, env)? {
        let GoDiscoveredTool::Managed(tool) = discovered else {
            continue;
        };
        paths.insert(tool.binary_name, tool.install_path);
    }
    Ok(paths)
}

fn exact_command_for_item(
    item: &ResolvedExecutionItem,
    install_paths: &BTreeMap<PackageName, String>,
) -> Result<CommandSpec, GoError> {
    let install_path = install_paths
        .get(&item.package_name)
        .ok_or_else(|| GoError::MissingInstallPath(item.package_name.as_str().to_owned()))?;
    Ok(exact_command(
        install_path,
        item.known_target_version().ok_or_else(|| {
            GoError::UnsupportedCommandIntent("exact-without-known-target".to_owned())
        })?,
    ))
}

fn lookup_installed_release(
    process: &ProcessRunner,
    env: &Env,
    tool: &InstalledTool,
) -> Result<ReleaseLookupResult, GoError> {
    for discovered in discover_global_tools(process, env)? {
        let GoDiscoveredTool::Managed(managed) = discovered else {
            continue;
        };
        if managed.binary_name == tool.package_name {
            return lookup_installed_release_by_module(
                process,
                &managed.module_path,
                &tool.installed_version,
            );
        }
    }
    Ok(ReleaseLookupResult::MissingMetadata)
}

fn lookup_installed_release_by_module(
    process: &ProcessRunner,
    module_path: &str,
    installed_version: &VersionText,
) -> Result<ReleaseLookupResult, GoError> {
    match module_version_time(process, module_path, installed_version.as_str()) {
        Ok(Some(published_at)) => Ok(ReleaseLookupResult::Known(ReleaseTimeline::new(vec![
            ReleaseEntry::new(
                installed_version.clone(),
                ReleaseTimestamp::new(published_at),
            ),
        ]))),
        Ok(None) => Ok(ReleaseLookupResult::MissingMetadata),
        Err(err) if err.is_interruption() => Err(err),
        Err(err) => Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
            err.to_string(),
        ))),
    }
}

const fn discovered_name(discovered: &GoDiscoveredTool) -> &PackageName {
    match discovered {
        GoDiscoveredTool::Managed(tool) => &tool.binary_name,
        GoDiscoveredTool::Skipped { name, .. } => name,
    }
}

fn system_time_from_datetime(datetime: DateTime<chrono::FixedOffset>) -> SystemTime {
    let timestamp = datetime.timestamp();
    if timestamp >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_secs(timestamp.unsigned_abs())
    } else {
        SystemTime::UNIX_EPOCH - Duration::from_secs(timestamp.unsigned_abs())
    }
}

fn adapter_error(err: &GoError) -> ManagerAdapterError {
    let kind = match err {
        GoError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        GoError::Json(_)
        | GoError::Domain(_)
        | GoError::InvalidTimestamp { .. }
        | GoError::MissingReleaseMetadata(_) => ManagerAdapterErrorKind::Parse,
        GoError::UnsupportedCommandIntent(_) | GoError::MissingInstallPath(_) => {
            ManagerAdapterErrorKind::CommandConstruction
        }
        GoError::Infra(_) | GoError::Io(_) | GoError::MissingHome => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail: err.to_string(),
    }
}

#[derive(Debug, Deserialize)]
struct GoEnvJson {
    #[serde(rename = "GOPATH")]
    gopath: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoListVersionsResponse {
    #[serde(default, rename = "Versions")]
    versions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GoListModuleResponse {
    #[serde(rename = "Time")]
    time: Option<String>,
}
