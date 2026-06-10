use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display};
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use pep440_rs::Version as Pep440Version;
use semver::Version as SemverVersion;
use serde::Deserialize;
use upgate_domain::{
    AuditPackageName, AuditSubject, DomainError, ExecutionSupport, InstalledTool, ManagerConfig,
    ManagerId, ManagerScanEvidenceInput, ManagerScanInput, ManagerSelectedTarget,
    ManagerUpdateInput, MinAgeConstraintSupport, OsvEcosystem, PackageName, ReleaseEntry,
    ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp, TargetAgeEvidence,
    TargetAgeLookupResult, ToolId, ToolName, UpdateSeed, VersionPolicy, VersionReleaseEvidence,
    VersionScheme, VersionText,
};
use upgate_execution::{
    ExecutionCommand, ExecutionCommandIntent, ExecutionCommandItem, ResolvedExecutionPlan,
    ResolvedExecutionTarget,
};
use upgate_infra::{
    CommandCheck, CommandSpec, Env, HttpClient, InfraError, ProcessRunner, effective_parallelism,
    run_ordered_parallel,
};

use crate::adapter::{
    ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind, ManagerCapabilities,
    ReleaseLookupSubject, validate_version_policy,
};

pub const MANAGER_ID: &str = "mise";
const MISE_AGE_MAX_PARALLEL_CHECKS: usize = 4;

const VERSIONS_HOST_BASE_URL: &str = "https://mise-versions.jdx.dev";
const VERSIONS_HOST_BASE_URL_ENV: &str = "upgate_MISE_VERSIONS_BASE_URL";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MiseError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Toml(String),
    Domain(String),
    InvalidTimestamp { version: String, value: String },
    InvalidDryRun(String),
    MissingReleaseMetadata(String),
    UnsupportedCommandIntent(String),
}

impl Display for MiseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Infra(detail)
            | Self::Interrupted(detail)
            | Self::Json(detail)
            | Self::Toml(detail)
            | Self::Domain(detail)
            | Self::InvalidDryRun(detail)
            | Self::MissingReleaseMetadata(detail) => formatter.write_str(detail),
            Self::InvalidTimestamp { version, value } => {
                write!(
                    formatter,
                    "invalid mise release timestamp `{value}` for version `{version}`"
                )
            }
            Self::UnsupportedCommandIntent(kind) => {
                write!(
                    formatter,
                    "unsupported mise execution command intent `{kind}`"
                )
            }
        }
    }
}

impl std::error::Error for MiseError {}

impl From<InfraError> for MiseError {
    fn from(value: InfraError) -> Self {
        if value.is_interruption() {
            Self::Interrupted(value.to_string())
        } else {
            Self::Infra(value.to_string())
        }
    }
}

impl From<DomainError> for MiseError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
    }
}

impl MiseError {
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiseInstalledTool {
    pub tool: PackageName,
    pub version: VersionText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MisePlanItem {
    pub tool: PackageName,
    pub from_version: VersionText,
    pub to_version: VersionText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MiseDryRunItem {
    tool: PackageName,
    from_version: Option<VersionText>,
    to_version: VersionText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiseManager {
    config: ManagerConfig,
}

impl MiseManager {
    pub const fn new(config: ManagerConfig) -> Self {
        Self { config }
    }

    pub fn id() -> ManagerId {
        ManagerId::from_static(MANAGER_ID)
    }
}
impl ManagerAdapter for MiseManager {
    fn supports_version_policy(policy: VersionPolicy) -> bool {
        policy == VersionPolicy::None
    }

    fn required_executable() -> &'static str {
        MANAGER_ID
    }

    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new().with_resolver_native_global_update(true)
    }
    fn scan_inputs(
        &self,
        process: &ProcessRunner,
        _env: &Env,
    ) -> Result<Vec<ManagerScanInput>, ManagerAdapterError> {
        installed_tools(process)
            .and_then(|tools| {
                tools
                    .into_iter()
                    .map(|tool| installed_tool(&tool).map(ManagerScanInput::Installed))
                    .collect()
            })
            .map_err(|err| adapter_error(&err))
    }

    fn scan_inputs_with_release_evidence(
        &self,
        process: &ProcessRunner,
        http: &HttpClient,
        env: &Env,
        max_parallel_checks_per_manager: usize,
    ) -> Result<Vec<ManagerScanEvidenceInput>, ManagerAdapterError> {
        scan_inputs_with_release_evidence(process, http, env, max_parallel_checks_per_manager)
            .map_err(|err| adapter_error(&err))
    }

    fn release_lookup(
        &self,
        process: &ProcessRunner,
        http: &HttpClient,
        env: &Env,
        subject: ReleaseLookupSubject<'_>,
    ) -> Result<ReleaseLookupResult, ManagerAdapterError> {
        Ok(match subject {
            ReleaseLookupSubject::Package(package) => {
                lookup_release_for_tool(process, http, env, package, None)
            }
            ReleaseLookupSubject::Installed(tool) => lookup_release_for_tool(
                process,
                http,
                env,
                &tool.package_name,
                Some(&tool.installed_version),
            ),
        })
    }

    fn update_inputs(
        &self,
        process: &ProcessRunner,
        http: &HttpClient,
        env: &Env,
        max_parallel_checks_per_manager: usize,
    ) -> Result<Vec<ManagerUpdateInput>, ManagerAdapterError> {
        validate_version_policy(
            &self.config.manager_id,
            Self::supports_version_policy(self.config.version_policy),
            self.config.version_policy,
        )?;
        update_inputs(
            process,
            http,
            env,
            self.config.min_release_age,
            max_parallel_checks_per_manager,
        )
        .map_err(|err| adapter_error(&err))
    }

    fn commands_for_execution_plan(
        &self,
        _process: &ProcessRunner,
        _env: &Env,
        plan: &ResolvedExecutionPlan,
    ) -> Result<Vec<ExecutionCommand>, ManagerAdapterError> {
        commands_for_execution_plan(plan, self.config.min_release_age)
            .map_err(|err| adapter_error(&err))
    }
}

#[derive(Debug, Deserialize)]
struct MiseLsEntry {
    version: Option<String>,
    #[serde(default)]
    active: bool,
}

#[derive(Debug, Deserialize)]
struct MiseOutdatedEntry {
    latest: String,
}

#[derive(Debug, Deserialize)]
struct MiseLsRemoteVersion {
    version: String,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MiseRegistryTool {
    #[serde(default)]
    backends: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MiseRegistryEntry {
    short: String,
    #[serde(default)]
    backends: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct VersionsHostRoot {
    #[serde(default)]
    versions: BTreeMap<String, VersionsHostVersion>,
}

#[derive(Debug, Deserialize)]
struct VersionsHostVersion {
    created_at: VersionsHostCreatedAt,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum VersionsHostCreatedAt {
    Datetime(toml::value::Datetime),
    String(String),
}

/// Parses `mise ls --json`.
///
/// # Errors
///
/// Returns an error when JSON is malformed or a tool/version is blank.
pub fn parse_installed_json(raw: &str) -> Result<Vec<MiseInstalledTool>, MiseError> {
    let parsed: BTreeMap<String, Vec<MiseLsEntry>> =
        serde_json::from_str(raw).map_err(|err| MiseError::Json(err.to_string()))?;
    let mut tools = BTreeMap::new();
    for (tool, entries) in parsed {
        let active_version = entries
            .iter()
            .find(|entry| entry.active)
            .and_then(|entry| entry.version.clone());
        if let Some(version) =
            active_version.or_else(|| entries.into_iter().rev().find_map(|entry| entry.version))
        {
            tools.insert(tool, version);
        }
    }
    tools
        .into_iter()
        .map(|(tool, version)| {
            Ok(MiseInstalledTool {
                tool: PackageName::new(tool)?,
                version: VersionText::new(version)?,
            })
        })
        .collect()
}

fn parse_upgrade_dry_run_targets(raw: &str) -> Result<Vec<MiseDryRunItem>, MiseError> {
    let mut old_versions = BTreeMap::<String, String>::new();
    let mut items = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("Would uninstall ") {
            let (tool, version) = split_tool_and_version(rest).ok_or_else(|| {
                MiseError::InvalidDryRun(format!("invalid mise dry-run uninstall line: {trimmed}"))
            })?;
            old_versions.insert(tool.to_owned(), version.to_owned());
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("Would install ") {
            let (tool, version) = split_tool_and_version(rest).ok_or_else(|| {
                MiseError::InvalidDryRun(format!("invalid mise dry-run install line: {trimmed}"))
            })?;
            let from_version = old_versions
                .remove(tool)
                .map(VersionText::new)
                .transpose()?;
            items.push(MiseDryRunItem {
                tool: PackageName::new(tool.to_owned())?,
                from_version,
                to_version: VersionText::new(version.to_owned())?,
            });
        }
    }

    if let Some((tool, _)) = old_versions.into_iter().next() {
        return Err(MiseError::InvalidDryRun(format!(
            "mise dry-run uninstall for {tool} was not followed by matching install"
        )));
    }

    Ok(items)
}

fn complete_dry_run_items_without_installed_lookup(
    items: Vec<MiseDryRunItem>,
) -> Result<Vec<MisePlanItem>, MiseError> {
    items
        .into_iter()
        .map(|item| {
            let from_version = item.from_version.ok_or_else(|| {
                MiseError::InvalidDryRun(format!(
                    "mise dry-run install for {} was not preceded by matching uninstall",
                    item.tool
                ))
            })?;
            Ok(MisePlanItem {
                tool: item.tool,
                from_version,
                to_version: item.to_version,
            })
        })
        .collect()
}

/// Parses `mise outdated --json` into advisory latest versions.
///
/// # Errors
///
/// Returns an error when JSON is malformed.
pub fn parse_outdated_json(raw: &str) -> Result<BTreeMap<PackageName, VersionText>, MiseError> {
    let parsed: BTreeMap<String, MiseOutdatedEntry> =
        serde_json::from_str(raw).map_err(|err| MiseError::Json(err.to_string()))?;
    parsed
        .into_iter()
        .map(|(tool, entry)| Ok((PackageName::new(tool)?, VersionText::new(entry.latest)?)))
        .collect()
}

/// Parses `mise ls-remote --json <tool>`.
///
/// # Errors
///
/// Returns an error when JSON is malformed or timestamps are invalid.
pub fn parse_ls_remote_json(tool: &str, raw: &str) -> Result<ReleaseTimeline, MiseError> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|err| MiseError::Json(err.to_string()))?;
    if let Ok(entries) = serde_json::from_value::<Vec<MiseLsRemoteVersion>>(value.clone()) {
        let mut releases = Vec::new();
        for entry in entries {
            let Some(created_at) = entry.created_at else {
                continue;
            };
            if let Ok(release) = release_entry(entry.version, created_at) {
                releases.push(release);
            }
        }
        return Ok(ReleaseTimeline::new(releases));
    }

    if serde_json::from_value::<Vec<String>>(value).is_ok() {
        return Ok(ReleaseTimeline::new(Vec::new()));
    }

    Err(MiseError::Json(format!(
        "failed to parse mise ls-remote JSON for {tool}"
    )))
}

/// Parses mise versions-host TOML.
///
/// # Errors
///
/// Returns an error when TOML is malformed or timestamps are invalid.
pub fn parse_versions_host_toml(raw: &str) -> Result<ReleaseTimeline, MiseError> {
    let parsed: VersionsHostRoot =
        toml::from_str(raw).map_err(|err| MiseError::Toml(err.to_string()))?;
    let mut releases = Vec::new();
    for (version, entry) in parsed.versions {
        if let Ok(release) = release_entry(version, entry.created_at.to_timestamp_string()) {
            releases.push(release);
        }
    }
    Ok(ReleaseTimeline::new(releases))
}

/// Reads installed mise tools.
///
/// # Errors
///
/// Returns an error when `mise ls --json` fails or cannot be parsed.
pub fn installed_tools(process: &ProcessRunner) -> Result<Vec<MiseInstalledTool>, MiseError> {
    let output = process.run(
        &CommandSpec::new("mise", ["ls", "--json"]),
        &CommandCheck::Success,
    )?;
    parse_installed_json(output.stdout()?)
}

/// Builds manager-selected planning inputs from mise's resolver.
///
/// # Errors
///
/// Returns an error when dry-run discovery fails or cannot be parsed.
pub fn update_inputs(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    min_release_age: Duration,
    max_parallel_checks_per_manager: usize,
) -> Result<Vec<ManagerUpdateInput>, MiseError> {
    let min_age_arg = duration_arg(min_release_age);
    let plan_items = upgrade_dry_run(process, &min_age_arg)?;
    if plan_items.is_empty() {
        return Ok(Vec::new());
    }
    let advisory_latest = advisory_latest_map(process)?;

    let threads = effective_parallelism(
        max_parallel_checks_per_manager,
        MISE_AGE_MAX_PARALLEL_CHECKS,
    );
    run_ordered_parallel(plan_items, threads, MANAGER_ID, |item| {
        let installed = installed_tool_from_plan_item(process, &item)?;
        let target_age = lookup_target_age(process, http, env, &item.tool, &item.to_version);
        let version_scheme = version_scheme(&item.from_version, &item.to_version);
        let advisory_release_lookup = if let Some(latest) = advisory_latest.latest.get(&item.tool)
            && latest != &item.to_version
        {
            Some((
                latest.clone(),
                lookup_release_for_tool(process, http, env, &item.tool, Some(latest)),
            ))
        } else {
            None
        };
        let mut selected = ManagerSelectedTarget::new(item.to_version, target_age);
        if let Some(failure) = advisory_latest.failure.as_ref() {
            selected = selected.with_advisory_lookup_failure(failure.clone());
        }
        if let Some((latest, lookup)) = advisory_release_lookup {
            selected = selected.with_advisory_release_lookup(latest, lookup);
        }
        Ok(ManagerUpdateInput::Seed(UpdateSeed::manager_selected(
            installed,
            selected,
            version_scheme,
            ExecutionSupport::resolver_native(MinAgeConstraintSupport::Optional, true, true),
        )))
    })?
    .into_iter()
    .collect()
}

fn scan_inputs_with_release_evidence(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    max_parallel_checks_per_manager: usize,
) -> Result<Vec<ManagerScanEvidenceInput>, MiseError> {
    let tools = installed_tools(process)?;
    run_ordered_parallel(
        tools,
        max_parallel_checks_per_manager.max(1),
        "mise verbose scan",
        |tool| {
            let installed = installed_tool_with_registry_audit(process, &tool)?;
            let release_lookup = lookup_release_for_tool(
                process,
                http,
                env,
                &installed.package_name,
                Some(&installed.installed_version),
            );
            Ok(scan_input_for_release_lookup(installed, release_lookup))
        },
    )?
    .into_iter()
    .collect()
}

/// Creates mise commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when the resolved execution mode is unsupported.
pub fn commands_for_execution_plan(
    plan: &ResolvedExecutionPlan,
    min_release_age: Duration,
) -> Result<Vec<ExecutionCommand>, MiseError> {
    let min_age_arg = duration_arg(min_release_age);
    let mut commands = Vec::new();
    for intent in &plan.intents {
        match intent {
            ExecutionCommandIntent::ResolverNative(item) => {
                commands.push(ExecutionCommand {
                    items: vec![ExecutionCommandItem::from(item)],
                    command: selected_upgrade_command(
                        &min_age_arg,
                        &item.package_name,
                        matches!(item.target, ResolvedExecutionTarget::ManagerResolved)
                            || item.bypass_min_release_age,
                    ),
                });
            }
            ExecutionCommandIntent::ResolverNativeGlobal(items) => {
                commands.push(ExecutionCommand {
                    items: items.iter().map(ExecutionCommandItem::from).collect(),
                    command: global_upgrade_command(&min_age_arg),
                });
            }
            ExecutionCommandIntent::Exact(_) => {
                return Err(MiseError::UnsupportedCommandIntent("exact".to_owned()));
            }
            ExecutionCommandIntent::GroupedNative(_) => {
                return Err(MiseError::UnsupportedCommandIntent(
                    "grouped-native".to_owned(),
                ));
            }
            ExecutionCommandIntent::NativeGlobal(_) => {
                return Err(MiseError::UnsupportedCommandIntent(
                    "native-global".to_owned(),
                ));
            }
            ExecutionCommandIntent::NativeSelected(_) => {
                return Err(MiseError::UnsupportedCommandIntent(
                    "native-selected".to_owned(),
                ));
            }
        }
    }
    Ok(commands)
}
fn selected_upgrade_command(
    min_age_arg: &str,
    tool: &PackageName,
    bypass_min_release_age: bool,
) -> CommandSpec {
    if bypass_min_release_age {
        CommandSpec::new("mise", ["upgrade", tool.as_str()]).mutating()
    } else {
        CommandSpec::new("mise", ["upgrade", "--before", min_age_arg, tool.as_str()]).mutating()
    }
}
fn global_upgrade_command(min_age_arg: &str) -> CommandSpec {
    CommandSpec::new("mise", ["upgrade", "--before", min_age_arg]).mutating()
}

fn split_tool_and_version(input: &str) -> Option<(&str, &str)> {
    let idx = input.rfind('@')?;
    let (tool, version) = input.split_at(idx);
    Some((tool, version.strip_prefix('@')?))
}

fn upgrade_dry_run(process: &ProcessRunner, before: &str) -> Result<Vec<MisePlanItem>, MiseError> {
    let output = process.run(
        &CommandSpec::new("mise", ["upgrade", "--dry-run", "--before", before]),
        &CommandCheck::Success,
    )?;
    let items = parse_upgrade_dry_run_targets(output.stdout()?)?;
    if items.iter().any(|item| item.from_version.is_none()) {
        complete_dry_run_items(items, installed_tools(process)?)
    } else {
        complete_dry_run_items_without_installed_lookup(items)
    }
}

fn complete_dry_run_items(
    items: Vec<MiseDryRunItem>,
    installed: Vec<MiseInstalledTool>,
) -> Result<Vec<MisePlanItem>, MiseError> {
    let installed_versions = installed
        .into_iter()
        .map(|tool| (tool.tool, tool.version))
        .collect::<BTreeMap<_, _>>();

    items
        .into_iter()
        .map(|item| {
            let from_version = match item.from_version {
                Some(version) => version,
                None => installed_versions.get(&item.tool).cloned().ok_or_else(|| {
                    MiseError::InvalidDryRun(format!(
                        "mise dry-run install for {} did not match an installed tool",
                        item.tool
                    ))
                })?,
            };
            Ok(MisePlanItem {
                tool: item.tool,
                from_version,
                to_version: item.to_version,
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct MiseAdvisoryLatest {
    latest: BTreeMap<PackageName, VersionText>,
    failure: Option<ReleaseLookupError>,
}

fn advisory_latest_map(process: &ProcessRunner) -> Result<MiseAdvisoryLatest, MiseError> {
    let output = match process.run(
        &CommandSpec::new("mise", ["outdated", "--json"]),
        &CommandCheck::Success,
    ) {
        Ok(output) => output,
        Err(err) if err.is_interruption() => return Err(MiseError::from(err)),
        Err(err) => {
            return Ok(MiseAdvisoryLatest {
                latest: BTreeMap::new(),
                failure: Some(ReleaseLookupError::new(err.to_string())),
            });
        }
    };
    let raw = match output.stdout() {
        Ok(raw) => raw,
        Err(err) => {
            return Ok(MiseAdvisoryLatest {
                latest: BTreeMap::new(),
                failure: Some(ReleaseLookupError::new(err.to_string())),
            });
        }
    };
    parse_outdated_json(raw)
        .map(|latest| MiseAdvisoryLatest {
            latest,
            failure: None,
        })
        .or_else(|err| {
            Ok(MiseAdvisoryLatest {
                latest: BTreeMap::new(),
                failure: Some(ReleaseLookupError::new(err.to_string())),
            })
        })
}

fn lookup_target_age(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    tool: &PackageName,
    target: &VersionText,
) -> TargetAgeLookupResult {
    match lookup_release_for_tool(process, http, env, tool, Some(target)) {
        ReleaseLookupResult::Known(timeline) => matching_entry(&timeline, target).map_or(
            TargetAgeLookupResult::MissingMetadata,
            |entry| {
                TargetAgeLookupResult::Known(TargetAgeEvidence::PublishedAt(
                    entry.published_at.clone(),
                ))
            },
        ),
        ReleaseLookupResult::MissingMetadata => TargetAgeLookupResult::MissingMetadata,
        ReleaseLookupResult::LookupFailed(err) => TargetAgeLookupResult::LookupFailed(err),
    }
}

fn lookup_release_for_tool(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    tool: &PackageName,
    current: Option<&VersionText>,
) -> ReleaseLookupResult {
    if let Some(package) = tool.as_str().strip_prefix("npm:") {
        return npm_release_lookup(process, package, current);
    }

    match mise_release_timeline(process, http, env, tool.as_str(), current) {
        Ok(Some(timeline)) if timeline.versions.is_empty() => ReleaseLookupResult::MissingMetadata,
        Ok(Some(timeline)) => ReleaseLookupResult::Known(timeline),
        Ok(None) | Err(MiseError::MissingReleaseMetadata(_)) => {
            ReleaseLookupResult::MissingMetadata
        }
        Err(err) => ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string())),
    }
}

fn scan_input_for_release_lookup(
    tool: InstalledTool,
    lookup: ReleaseLookupResult,
) -> ManagerScanEvidenceInput {
    match lookup {
        ReleaseLookupResult::Known(timeline) => ManagerScanEvidenceInput::Installed {
            release_evidence: matching_entry(&timeline, &tool.installed_version).map(|entry| {
                VersionReleaseEvidence::new(
                    tool.installed_version.clone(),
                    entry.published_at.clone(),
                )
            }),
            tool,
        },
        ReleaseLookupResult::MissingMetadata | ReleaseLookupResult::LookupFailed(_) => {
            ManagerScanEvidenceInput::Installed {
                tool,
                release_evidence: None,
            }
        }
    }
}

fn npm_release_lookup(
    process: &ProcessRunner,
    package: &str,
    version: Option<&VersionText>,
) -> ReleaseLookupResult {
    let spec = version.map_or_else(
        || package.to_owned(),
        |version| format!("{package}@{version}"),
    );
    let output = match process.run(
        &CommandSpec::new("npm", ["view", &spec, "time", "--json"]),
        &CommandCheck::Success,
    ) {
        Ok(output) => output,
        Err(err) => {
            return ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string()));
        }
    };
    let raw = match output.stdout() {
        Ok(raw) => raw,
        Err(err) => {
            return ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string()));
        }
    };
    match npm_time_map_to_timeline(raw) {
        Ok(timeline) if timeline.versions.is_empty() => ReleaseLookupResult::MissingMetadata,
        Ok(timeline) => ReleaseLookupResult::Known(timeline),
        Err(err) => ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string())),
    }
}

fn npm_time_map_to_timeline(raw: &str) -> Result<ReleaseTimeline, MiseError> {
    let map: BTreeMap<String, String> =
        serde_json::from_str(raw).map_err(|err| MiseError::Json(err.to_string()))?;
    let mut releases = Vec::new();
    for (version, timestamp) in map {
        releases.push(release_entry(version, timestamp)?);
    }
    Ok(ReleaseTimeline::new(releases))
}

fn mise_release_timeline(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    tool: &str,
    current: Option<&VersionText>,
) -> Result<Option<ReleaseTimeline>, MiseError> {
    if tool.contains(':') {
        let timeline = ls_remote_timeline(process, tool)?;
        if !timeline.versions.is_empty()
            && current.is_none_or(|version| timeline_contains_version(&timeline, version))
        {
            return Ok(Some(timeline));
        }
        return versions_host_timeline_for_backend(process, http, env, tool, current);
    }

    for backend in registry_backends(process, tool)? {
        let timeline = ls_remote_timeline(process, &backend)?;
        if !timeline.versions.is_empty()
            && current.is_none_or(|version| timeline_contains_version(&timeline, version))
        {
            return Ok(Some(timeline));
        }
    }

    versions_host_timeline(http, env, tool)
}

fn registry_backends(process: &ProcessRunner, tool: &str) -> Result<Vec<String>, MiseError> {
    let output = process.run(
        &CommandSpec::new("mise", ["registry", tool, "--json"]),
        &CommandCheck::Success,
    )?;
    let parsed: MiseRegistryTool =
        serde_json::from_str(output.stdout()?).map_err(|err| MiseError::Json(err.to_string()))?;
    if parsed.backends.is_empty() {
        return Err(MiseError::MissingReleaseMetadata(format!(
            "mise registry returned no backends for {tool}"
        )));
    }
    Ok(parsed.backends)
}

fn registry_shorts_for_backend(
    process: &ProcessRunner,
    backend: &str,
) -> Result<Vec<String>, MiseError> {
    let output = process.run(
        &CommandSpec::new("mise", ["registry", "--json"]),
        &CommandCheck::Success,
    )?;
    let parsed: Vec<MiseRegistryEntry> =
        serde_json::from_str(output.stdout()?).map_err(|err| MiseError::Json(err.to_string()))?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for entry in parsed {
        if entry
            .backends
            .iter()
            .any(|candidate| backend_matches(candidate, backend))
            && seen.insert(entry.short.clone())
        {
            out.push(entry.short);
        }
    }
    Ok(out)
}

fn backend_matches(candidate: &str, installed: &str) -> bool {
    candidate == installed || strip_backend_options(candidate) == strip_backend_options(installed)
}

fn strip_backend_options(value: &str) -> &str {
    value
        .split_once('[')
        .map_or(value, |(backend_without_options, _)| {
            backend_without_options
        })
}

fn ls_remote_timeline(process: &ProcessRunner, tool: &str) -> Result<ReleaseTimeline, MiseError> {
    let output = process.run(
        &CommandSpec::new("mise", ["ls-remote", "--json", tool]),
        &CommandCheck::Success,
    )?;
    parse_ls_remote_json(tool, output.stdout()?)
}

fn versions_host_timeline_for_backend(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    backend: &str,
    current: Option<&VersionText>,
) -> Result<Option<ReleaseTimeline>, MiseError> {
    for short in registry_shorts_for_backend(process, backend)? {
        if let Some(timeline) = versions_host_timeline(http, env, &short)?
            && current.is_none_or(|version| timeline_contains_version(&timeline, version))
        {
            return Ok(Some(timeline));
        }
    }
    Ok(None)
}

fn versions_host_timeline(
    http: &HttpClient,
    env: &Env,
    tool: &str,
) -> Result<Option<ReleaseTimeline>, MiseError> {
    let base_url =
        upgate_infra::env_base_url(env, VERSIONS_HOST_BASE_URL_ENV, VERSIONS_HOST_BASE_URL);
    let url = format!("{base_url}/tools/{tool}.toml");
    let response = match http.get_text(&url) {
        Ok(response) => response,
        Err(InfraError::HttpStatus {
            status: 404 | 429, ..
        }) => return Ok(None),
        Err(err) => return Err(MiseError::from(err)),
    };
    let timeline = parse_versions_host_toml(&response.body)?;
    if timeline.versions.is_empty() {
        Ok(None)
    } else {
        Ok(Some(timeline))
    }
}

fn timeline_contains_version(timeline: &ReleaseTimeline, version: &VersionText) -> bool {
    matching_entry(timeline, version).is_some()
}

fn matching_entry<'a>(
    timeline: &'a ReleaseTimeline,
    version: &VersionText,
) -> Option<&'a ReleaseEntry> {
    timeline.versions.iter().find(|entry| {
        entry.version == *version
            || semver_versions_equivalent(entry.version.as_str(), version.as_str())
            || pep440_versions_equivalent(entry.version.as_str(), version.as_str())
    })
}

fn release_entry(version: String, timestamp: String) -> Result<ReleaseEntry, MiseError> {
    let parsed = parse_timestamp(&timestamp).ok_or_else(|| MiseError::InvalidTimestamp {
        version: version.clone(),
        value: timestamp,
    })?;
    Ok(ReleaseEntry::new(
        VersionText::new(version)?,
        ReleaseTimestamp::new(system_time_from_datetime(parsed)),
    ))
}

impl VersionsHostCreatedAt {
    fn to_timestamp_string(&self) -> String {
        match self {
            Self::Datetime(value) => value.to_string(),
            Self::String(value) => value.clone(),
        }
    }
}

fn parse_timestamp(value: &str) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(value).ok().or_else(|| {
        let trimmed = value.trim();
        if !timestamp_missing_timezone(trimmed) {
            return None;
        }
        DateTime::parse_from_rfc3339(&format!("{trimmed}Z")).ok()
    })
}

fn timestamp_missing_timezone(value: &str) -> bool {
    let Some((_date, time)) = value.split_once('T') else {
        return false;
    };
    !time.ends_with('Z') && !time.contains('+') && !time.contains('-')
}

fn system_time_from_datetime(datetime: DateTime<chrono::FixedOffset>) -> SystemTime {
    let timestamp = datetime.timestamp();
    if timestamp >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_secs(timestamp.unsigned_abs())
    } else {
        SystemTime::UNIX_EPOCH - Duration::from_secs(timestamp.unsigned_abs())
    }
}

fn semver_versions_equivalent(left: &str, right: &str) -> bool {
    let left = strip_v_prefix(left.trim());
    let right = strip_v_prefix(right.trim());
    if left == right {
        return true;
    }
    match (SemverVersion::parse(left), SemverVersion::parse(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn pep440_versions_equivalent(left: &str, right: &str) -> bool {
    let left = strip_v_prefix(left.trim());
    let right = strip_v_prefix(right.trim());
    if left == right {
        return true;
    }
    match (
        Pep440Version::from_str(left),
        Pep440Version::from_str(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn version_scheme(current: &VersionText, target: &VersionText) -> VersionScheme {
    if SemverVersion::parse(strip_v_prefix(current.as_str())).is_ok()
        && SemverVersion::parse(strip_v_prefix(target.as_str())).is_ok()
    {
        VersionScheme::SemVer
    } else if Pep440Version::from_str(strip_v_prefix(current.as_str())).is_ok()
        && Pep440Version::from_str(strip_v_prefix(target.as_str())).is_ok()
    {
        VersionScheme::Pep440
    } else {
        VersionScheme::ManagerNative
    }
}

fn strip_v_prefix(value: &str) -> &str {
    value.strip_prefix('v').unwrap_or(value)
}

fn installed_tool(tool: &MiseInstalledTool) -> Result<InstalledTool, MiseError> {
    let installed = InstalledTool::new(
        MiseManager::id(),
        ToolId::new(tool.tool.as_str().to_owned())?,
        tool.tool.clone(),
        ToolName::new(tool.tool.as_str().to_owned())?,
        tool.version.clone(),
    );
    Ok(match audit_subject_for_mise_tool(&tool.tool)? {
        Some(subject) => installed.with_audit_subject(subject),
        None => installed,
    })
}

fn installed_tool_with_registry_audit(
    process: &ProcessRunner,
    tool: &MiseInstalledTool,
) -> Result<InstalledTool, MiseError> {
    let installed = InstalledTool::new(
        MiseManager::id(),
        ToolId::new(tool.tool.as_str().to_owned())?,
        tool.tool.clone(),
        ToolName::new(tool.tool.as_str().to_owned())?,
        tool.version.clone(),
    );
    Ok(
        match audit_subject_for_mise_tool_with_registry(process, &tool.tool)? {
            Some(subject) => installed.with_audit_subject(subject),
            None => installed,
        },
    )
}

fn installed_tool_from_plan_item(
    process: &ProcessRunner,
    item: &MisePlanItem,
) -> Result<InstalledTool, MiseError> {
    let installed = InstalledTool::new(
        MiseManager::id(),
        ToolId::new(item.tool.as_str().to_owned())?,
        item.tool.clone(),
        ToolName::new(item.tool.as_str().to_owned())?,
        item.from_version.clone(),
    );
    Ok(
        match audit_subject_for_mise_tool_with_registry(process, &item.tool)? {
            Some(subject) => installed.with_audit_subject(subject),
            None => installed,
        },
    )
}

fn audit_subject_for_mise_tool(tool: &PackageName) -> Result<Option<AuditSubject>, MiseError> {
    let Some((backend, package)) = tool.as_str().split_once(':') else {
        return Ok(None);
    };
    if backend == "github" {
        return github_audit_subject_for_backend(tool.as_str());
    }
    let ecosystem = match backend {
        "npm" => OsvEcosystem::Npm,
        "pipx" | "uvx" => OsvEcosystem::Pypi,
        "cargo" => OsvEcosystem::CratesIo,
        "go" => OsvEcosystem::Go,
        "gem" => OsvEcosystem::RubyGems,
        _ => return Ok(None),
    };
    Ok(Some(AuditSubject::new(
        ecosystem,
        AuditPackageName::new(package.to_owned())?,
    )))
}

fn audit_subject_for_mise_tool_with_registry(
    process: &ProcessRunner,
    tool: &PackageName,
) -> Result<Option<AuditSubject>, MiseError> {
    if let Some(subject) = audit_subject_for_mise_tool(tool)? {
        return Ok(Some(subject));
    }
    let backends = match registry_backends(process, tool.as_str()) {
        Ok(backends) => backends,
        Err(err) if err.is_interruption() => return Err(err),
        Err(_) => return Ok(None),
    };
    if backends.len() != 1 {
        return Ok(None);
    }
    github_audit_subject_for_backend(&backends[0])
}

fn github_audit_subject_for_backend(backend: &str) -> Result<Option<AuditSubject>, MiseError> {
    let backend = strip_backend_options(backend.trim());
    let Some(repo) = backend.strip_prefix("github:") else {
        return Ok(None);
    };
    github_audit_subject(repo)
}

fn github_audit_subject(repo: &str) -> Result<Option<AuditSubject>, MiseError> {
    let repo = repo.trim();
    let Some((owner, name)) = repo.split_once('/') else {
        return Ok(None);
    };
    let name = name.trim_end_matches(".git");
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return Ok(None);
    }
    Ok(Some(AuditSubject::new(
        OsvEcosystem::Git,
        AuditPackageName::new(format!("https://github.com/{owner}/{name}.git"))?,
    )))
}

fn duration_arg(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds.is_multiple_of(86_400) {
        format!("{}d", seconds / 86_400)
    } else if seconds.is_multiple_of(3_600) {
        format!("{}h", seconds / 3_600)
    } else if seconds.is_multiple_of(60) {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

fn adapter_error(err: &MiseError) -> ManagerAdapterError {
    let kind = match err {
        MiseError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        MiseError::Json(_)
        | MiseError::Toml(_)
        | MiseError::Domain(_)
        | MiseError::InvalidTimestamp { .. }
        | MiseError::InvalidDryRun(_)
        | MiseError::MissingReleaseMetadata(_) => ManagerAdapterErrorKind::Parse,
        MiseError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        MiseError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        kind,
        detail: err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_upgrade_dry_run_keeps_uninstall_install_pair_versions() {
        let items = complete_dry_run_items_without_installed_lookup(
            parse_upgrade_dry_run_targets(
                "Would uninstall node@24.14.1\nWould install node@24.16.0\n",
            )
            .expect("paired dry-run output should parse"),
        )
        .expect("paired dry-run targets should complete");

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].tool.as_str(), "node");
        assert_eq!(items[0].from_version.as_str(), "24.14.1");
        assert_eq!(items[0].to_version.as_str(), "24.16.0");
    }

    #[test]
    fn complete_install_only_dry_run_from_installed_tools() {
        let items = parse_upgrade_dry_run_targets(
            "Would install node@24.16.0\nWould install npm:@anthropic-ai/claude-code@2.1.158\n",
        )
        .expect("install-only dry-run output should parse");
        let completed = complete_dry_run_items(
            items,
            vec![
                MiseInstalledTool {
                    tool: PackageName::new("node").expect("valid package"),
                    version: VersionText::new("24.14.1").expect("valid version"),
                },
                MiseInstalledTool {
                    tool: PackageName::new("npm:@anthropic-ai/claude-code").expect("valid package"),
                    version: VersionText::new("2.1.156").expect("valid version"),
                },
            ],
        )
        .expect("install-only targets should complete from installed tools");

        assert_eq!(completed.len(), 2);
        assert_eq!(completed[0].tool.as_str(), "node");
        assert_eq!(completed[0].from_version.as_str(), "24.14.1");
        assert_eq!(completed[0].to_version.as_str(), "24.16.0");
        assert_eq!(completed[1].tool.as_str(), "npm:@anthropic-ai/claude-code");
        assert_eq!(completed[1].from_version.as_str(), "2.1.156");
        assert_eq!(completed[1].to_version.as_str(), "2.1.158");
    }

    #[test]
    fn parse_installed_json_prefers_active_version() {
        let tools = parse_installed_json(
            r#"{
                "node": [
                    {"version": "20.19.4", "installed": true, "active": false},
                    {"version": "24.14.1", "installed": true, "active": true},
                    {"version": "22.22.3", "installed": true, "active": false}
                ]
            }"#,
        )
        .expect("installed JSON should parse");

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool.as_str(), "node");
        assert_eq!(tools[0].version.as_str(), "24.14.1");
    }

    #[test]
    fn malformed_recognized_dry_run_line_still_fails() {
        let err = parse_upgrade_dry_run_targets("Would install node\n")
            .expect_err("malformed install line should fail");

        assert!(matches!(err, MiseError::InvalidDryRun(_)));
        assert!(
            err.to_string()
                .contains("invalid mise dry-run install line")
        );
    }
}
