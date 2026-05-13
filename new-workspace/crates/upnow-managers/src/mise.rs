use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display};
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use pep440_rs::Version as Pep440Version;
use semver::Version as SemverVersion;
use serde::Deserialize;
use upnow_domain::{
    DomainError, ExecutionEligibility, InstalledTool, ManagerConfig, ManagerId, ManagerMetadata,
    ManagerScanInput, ManagerSelectedTarget, ManagerUpdateInput, PackageName, ReleaseEntry,
    ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp, TargetAgeEvidence,
    TargetAgeLookupResult, ToolId, ToolName, UpdateSeed, VersionPolicy, VersionScheme, VersionText,
};
use upnow_execution::{
    ExecutionCommand, ExecutionCommandIntent, ExecutionCommandItem, ResolvedExecutionItem,
    ResolvedExecutionPlan,
};
use upnow_infra::{CommandCheck, CommandSpec, Env, HttpClient, InfraError, ProcessRunner};

use crate::adapter::{
    ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind, ManagerCapabilities,
    ReleaseLookupSubject,
};

pub const MANAGER_ID: &str = "mise";

const VERSIONS_HOST_BASE_URL: &str = "https://mise-versions.jdx.dev";
const VERSIONS_HOST_BASE_URL_ENV: &str = "UPNOW_MISE_VERSIONS_BASE_URL";

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
pub struct MiseManager {
    config: ManagerConfig,
}

impl MiseManager {
    pub const fn new(config: ManagerConfig) -> Self {
        Self { config }
    }
}
impl ManagerAdapter for MiseManager {
    fn id(&self) -> &'static str {
        MANAGER_ID
    }

    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new().with_resolver_native_global_update(true)
    }

    fn supports_version_policy(&self, policy: VersionPolicy) -> bool {
        policy == VersionPolicy::None
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
                    .collect::<Result<Vec<_>, _>>()
            })
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
    ) -> Result<Vec<ManagerUpdateInput>, ManagerAdapterError> {
        self.validate_version_policy(self.config.version_policy)?;
        update_inputs(process, http, env, self.config.min_release_age)
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
        for entry in entries {
            if let Some(version) = entry.version {
                tools.insert(tool.clone(), version);
            }
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

/// Parses `mise upgrade --dry-run --before <age>` output.
///
/// # Errors
///
/// Returns an error when recognized dry-run action lines are malformed or
/// uninstall/install pairs do not match.
pub fn parse_upgrade_dry_run(raw: &str) -> Result<Vec<MisePlanItem>, MiseError> {
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
            let from_version = old_versions.remove(tool).ok_or_else(|| {
                MiseError::InvalidDryRun(format!(
                    "mise dry-run install for {tool} was not preceded by matching uninstall"
                ))
            })?;
            items.push(MisePlanItem {
                tool: PackageName::new(tool.to_owned())?,
                from_version: VersionText::new(from_version)?,
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
) -> Result<Vec<ManagerUpdateInput>, MiseError> {
    let min_age_arg = duration_arg(min_release_age);
    let plan_items = upgrade_dry_run(process, &min_age_arg)?;
    if plan_items.is_empty() {
        return Ok(Vec::new());
    }
    let advisory_latest = advisory_latest_map(process)?;

    let mut inputs = Vec::new();
    for item in plan_items {
        let installed = installed_tool_from_plan_item(&item)?;
        let target_age = lookup_target_age(
            process,
            http,
            env,
            &item.tool,
            &item.from_version,
            &item.to_version,
        );
        let mut selected = ManagerSelectedTarget::new(item.to_version.clone(), target_age);
        if let Some(latest) = advisory_latest.get(&item.tool)
            && latest != &item.to_version
        {
            selected = selected.with_advisory_release_lookup(
                latest.clone(),
                lookup_release_for_tool(process, http, env, &item.tool, Some(latest)),
            );
        }
        inputs.push(ManagerUpdateInput::Seed(UpdateSeed::manager_selected(
            installed,
            selected,
            version_scheme(&item.from_version, &item.to_version),
            ExecutionEligibility::ResolverNativeOnly,
        )));
    }
    Ok(inputs)
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
                    items: vec![execution_item(item)],
                    command: selected_upgrade_command(&min_age_arg, &item.package_name),
                });
            }
            ExecutionCommandIntent::ResolverNativeGlobal(items) => {
                commands.push(ExecutionCommand {
                    items: items.iter().map(execution_item).collect(),
                    command: global_upgrade_command(&min_age_arg),
                });
            }
            ExecutionCommandIntent::Exact(_) => {
                return Err(MiseError::UnsupportedCommandIntent("exact".to_owned()));
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
fn selected_upgrade_command(min_age_arg: &str, tool: &PackageName) -> CommandSpec {
    CommandSpec::new("mise", ["upgrade", "--before", min_age_arg, tool.as_str()]).mutating()
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
    parse_upgrade_dry_run(output.stdout()?)
}

fn advisory_latest_map(
    process: &ProcessRunner,
) -> Result<BTreeMap<PackageName, VersionText>, MiseError> {
    let output = match process.run(
        &CommandSpec::new("mise", ["outdated", "--json"]),
        &CommandCheck::Success,
    ) {
        Ok(output) => output,
        Err(err) if err.is_interruption() => return Err(MiseError::from(err)),
        Err(_) => return Ok(BTreeMap::new()),
    };
    parse_outdated_json(output.stdout()?).or_else(|_| Ok(BTreeMap::new()))
}

fn lookup_target_age(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    tool: &PackageName,
    current: &VersionText,
    target: &VersionText,
) -> TargetAgeLookupResult {
    match lookup_target_release_for_tool(process, http, env, tool, current, target) {
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

fn lookup_target_release_for_tool(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    tool: &PackageName,
    current: &VersionText,
    target: &VersionText,
) -> ReleaseLookupResult {
    if tool.as_str().starts_with("npm:") {
        return lookup_release_for_tool(process, http, env, tool, Some(target));
    }

    match mise_target_release_timeline(process, http, env, tool.as_str(), current, target) {
        Ok(Some(timeline)) if timeline.versions.is_empty() => ReleaseLookupResult::MissingMetadata,
        Ok(Some(timeline)) => ReleaseLookupResult::Known(timeline),
        Ok(None) | Err(MiseError::MissingReleaseMetadata(_)) => {
            ReleaseLookupResult::MissingMetadata
        }
        Err(err) => ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string())),
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

fn npm_release_lookup(
    process: &ProcessRunner,
    package: &str,
    version: Option<&VersionText>,
) -> ReleaseLookupResult {
    let spec = version.map_or_else(
        || package.to_owned(),
        |version| format!("{package}@{}", version.as_str()),
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

fn mise_target_release_timeline(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    tool: &str,
    current: &VersionText,
    target: &VersionText,
) -> Result<Option<ReleaseTimeline>, MiseError> {
    if tool.contains(':') {
        let timeline = ls_remote_timeline(process, tool)?;
        if matching_entry(&timeline, target).is_some() {
            return Ok(Some(timeline));
        }
        return versions_host_timeline_for_backend(process, http, env, tool, Some(current));
    }

    for backend in registry_backends(process, tool)? {
        let timeline = ls_remote_timeline(process, &backend)?;
        if matching_entry(&timeline, target).is_some() {
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
        upnow_infra::env_base_url(env, VERSIONS_HOST_BASE_URL_ENV, VERSIONS_HOST_BASE_URL);
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
    Ok(InstalledTool::new(
        manager_id(),
        ToolId::new(tool.tool.as_str().to_owned())?,
        tool.tool.clone(),
        ToolName::new(tool.tool.as_str().to_owned())?,
        tool.version.clone(),
        ManagerMetadata::empty(),
    ))
}

fn installed_tool_from_plan_item(item: &MisePlanItem) -> Result<InstalledTool, MiseError> {
    Ok(InstalledTool::new(
        manager_id(),
        ToolId::new(item.tool.as_str().to_owned())?,
        item.tool.clone(),
        ToolName::new(item.tool.as_str().to_owned())?,
        item.from_version.clone(),
        ManagerMetadata::empty(),
    ))
}

fn execution_item(item: &ResolvedExecutionItem) -> ExecutionCommandItem {
    ExecutionCommandItem {
        plan_item_id: item.plan_item_id.clone(),
        package_name: item.package_name.clone(),
        installed_version: item.installed_version.clone(),
        target_version: item.target_version.clone(),
    }
}

fn manager_id() -> ManagerId {
    ManagerId::new(MANAGER_ID).expect("static mise manager id should be valid")
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
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail: err.to_string(),
    }
}
