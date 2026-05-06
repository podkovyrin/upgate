use std::fmt::{self, Display};
use std::io::Read;
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use flate2::read::GzDecoder;
use serde::Deserialize;
use upnow_domain::{
    DomainError, InstalledTool, ManagerId, ManagerMetadata, ManagerScanInput, ManagerUpdateInput,
    PackageName, ReleaseEntry, ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline,
    ReleaseTimestamp, ToolId, ToolName, UpdateCandidate, UpdateSeed, VersionPolicy, VersionScheme,
    VersionText,
};
use upnow_execution::{ExecutionCommandIntent, ResolvedExecutionItem, ResolvedExecutionPlan};
use upnow_infra::{CommandCheck, CommandSpec, Env, HttpClient, InfraError, ProcessRunner};
use upnow_release::newest_semver_version;

use crate::adapter::{
    CommandBuildSettings, ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind,
    ManagerCapabilities, ManagerExecutionCommand, ManagerExecutionCommandItem,
    ReleaseLookupSubject,
};

pub const MANAGER_ID: &str = "dotnet";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DotnetError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    InvalidTimestamp { version: String, value: String },
    MissingReleaseMetadata(String),
    UnsupportedCommandIntent(String),
}

impl Display for DotnetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Infra(detail)
            | Self::Interrupted(detail)
            | Self::Json(detail)
            | Self::Domain(detail)
            | Self::MissingReleaseMetadata(detail) => formatter.write_str(detail),
            Self::InvalidTimestamp { version, value } => {
                write!(
                    formatter,
                    "invalid NuGet published timestamp `{value}` for version `{version}`"
                )
            }
            Self::UnsupportedCommandIntent(kind) => {
                write!(
                    formatter,
                    "unsupported dotnet execution command intent `{kind}`"
                )
            }
        }
    }
}

impl std::error::Error for DotnetError {}

impl From<InfraError> for DotnetError {
    fn from(value: InfraError) -> Self {
        if value.is_interruption() {
            Self::Interrupted(value.to_string())
        } else {
            Self::Infra(value.to_string())
        }
    }
}

impl From<DomainError> for DotnetError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
    }
}

impl DotnetError {
    #[must_use]
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DotnetToolPackage {
    pub package_id: PackageName,
    pub version: VersionText,
}

#[derive(Debug, Deserialize)]
struct DotnetToolListRoot {
    #[serde(default)]
    data: Vec<DotnetToolListEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DotnetToolListEntry {
    package_id: String,
    version: String,
}

#[derive(Debug, Deserialize)]
struct NugetRegistrationIndex {
    #[serde(default)]
    items: Vec<NugetRegistrationPageRef>,
}

#[derive(Debug, Deserialize)]
struct NugetRegistrationPageRef {
    #[serde(rename = "@id")]
    id: String,
}

#[derive(Debug, Deserialize)]
struct NugetRegistrationPage {
    #[serde(default)]
    items: Vec<NugetRegistrationLeaf>,
}

#[derive(Debug, Deserialize)]
struct NugetRegistrationLeaf {
    #[serde(rename = "catalogEntry")]
    catalog_entry: NugetCatalogEntry,
}

#[derive(Debug, Deserialize)]
struct NugetCatalogEntry {
    version: String,
    published: Option<String>,
    #[serde(default)]
    listed: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DotnetManager;

impl ManagerAdapter for DotnetManager {
    fn id(&self) -> &'static str {
        MANAGER_ID
    }

    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new(true, false)
    }

    fn supports_version_policy(&self, _policy: VersionPolicy) -> bool {
        true
    }

    fn scan_inputs(
        &self,
        process: &ProcessRunner,
        _env: &Env,
    ) -> Result<Vec<ManagerScanInput>, ManagerAdapterError> {
        installed_global(process)
            .map(|tools| tools.into_iter().map(ManagerScanInput::Installed).collect())
            .map_err(|err| adapter_error(&err))
    }

    fn release_lookup(
        &self,
        _process: &ProcessRunner,
        http: &HttpClient,
        env: &Env,
        subject: ReleaseLookupSubject<'_>,
    ) -> Result<ReleaseLookupResult, ManagerAdapterError> {
        Ok(lookup_release(http, env, subject.package_name()))
    }

    fn update_inputs(
        &self,
        process: &ProcessRunner,
        http: &HttpClient,
        env: &Env,
        version_policy: VersionPolicy,
        _min_release_age: Duration,
        _now: SystemTime,
    ) -> Result<Vec<ManagerUpdateInput>, ManagerAdapterError> {
        self.validate_version_policy(version_policy)?;
        update_inputs(process, http, env).map_err(|err| adapter_error(&err))
    }

    fn commands_for_execution_plan(
        &self,
        _process: &ProcessRunner,
        _env: &Env,
        plan: &ResolvedExecutionPlan,
        _settings: CommandBuildSettings,
    ) -> Result<Vec<ManagerExecutionCommand>, ManagerAdapterError> {
        commands_for_execution_plan(plan).map_err(|err| adapter_error(&err))
    }
}

/// Parses `dotnet tool list --global --format json`.
///
/// # Errors
///
/// Returns an error when JSON is malformed or a package/version is blank.
pub fn parse_tool_list_json(raw: &str) -> Result<Vec<DotnetToolPackage>, DotnetError> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let parsed: DotnetToolListRoot =
        serde_json::from_str(raw).map_err(|err| DotnetError::Json(err.to_string()))?;
    parsed
        .data
        .into_iter()
        .map(|entry| {
            Ok(DotnetToolPackage {
                package_id: PackageName::new(entry.package_id)?,
                version: VersionText::new(entry.version)?,
            })
        })
        .collect()
}

/// Reads installed .NET global tools.
///
/// # Errors
///
/// Returns an error when command output cannot be parsed.
pub fn installed_global(process: &ProcessRunner) -> Result<Vec<InstalledTool>, DotnetError> {
    let output = process.run(
        &CommandSpec::new("dotnet", ["tool", "list", "--global", "--format", "json"]),
        &CommandCheck::IgnoreStatus,
    )?;
    if !output.status().success() {
        let stdout = output.stdout_string_lossy();
        let stderr = output.stderr_string_lossy();
        return Err(DotnetError::Infra(format!(
            "dotnet tool list --global --format json failed: {}",
            if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            }
        )));
    }
    parse_tool_list_json(output.stdout()?)?
        .into_iter()
        .map(installed_tool)
        .collect()
}

/// Builds planning inputs for .NET global tools.
///
/// # Errors
///
/// Returns an error when installed discovery fails.
pub fn update_inputs(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
) -> Result<Vec<ManagerUpdateInput>, DotnetError> {
    let mut inputs = Vec::new();
    for tool in installed_global(process)? {
        let lookup = lookup_release(http, env, &tool.package_name);
        inputs.push(update_input(tool, lookup));
    }
    Ok(inputs)
}

/// Looks up `NuGet` release metadata.
#[must_use]
pub fn lookup_release(http: &HttpClient, env: &Env, package: &PackageName) -> ReleaseLookupResult {
    match nuget_timeline(http, env, package) {
        Ok(timeline) => ReleaseLookupResult::Known(timeline),
        Err(DotnetError::MissingReleaseMetadata(_)) => ReleaseLookupResult::MissingMetadata,
        Err(err) => ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string())),
    }
}

/// Parses one `NuGet` registration page into release entries.
///
/// # Errors
///
/// Returns an error when JSON or timestamps are invalid.
pub fn parse_nuget_page_json(raw: &str) -> Result<Vec<ReleaseEntry>, DotnetError> {
    let page: NugetRegistrationPage =
        serde_json::from_str(raw).map_err(|err| DotnetError::Json(err.to_string()))?;
    let mut entries = Vec::new();
    for leaf in page.items {
        let entry = leaf.catalog_entry;
        if entry.listed == Some(false) {
            continue;
        }
        let Some(published) = entry.published else {
            continue;
        };
        let parsed = parse_timestamp(&published).ok_or_else(|| DotnetError::InvalidTimestamp {
            version: entry.version.clone(),
            value: published.clone(),
        })?;
        entries.push(ReleaseEntry::new(
            VersionText::new(entry.version)?,
            ReleaseTimestamp::new(system_time_from_datetime(parsed)),
        ));
    }
    Ok(entries)
}

/// Creates dotnet commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when the resolved execution mode is not supported.
pub fn commands_for_execution_plan(
    plan: &ResolvedExecutionPlan,
) -> Result<Vec<ManagerExecutionCommand>, DotnetError> {
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
                return Err(DotnetError::UnsupportedCommandIntent(
                    "native-selected".to_owned(),
                ));
            }
            ExecutionCommandIntent::NativeGlobal(_) => {
                return Err(DotnetError::UnsupportedCommandIntent(
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

fn nuget_timeline(
    http: &HttpClient,
    env: &Env,
    package: &PackageName,
) -> Result<ReleaseTimeline, DotnetError> {
    let id_lower = package.as_str().to_ascii_lowercase();
    let base_url =
        upnow_infra::env_base_url(env, "UPNOW_DOTNET_NUGET_BASE_URL", "https://api.nuget.org");
    let semver2 = format!("{base_url}/v3/registration5-gz-semver2/{id_lower}/index.json");
    let mut entries = nuget_entries_from_index(http, package, &semver2, true)?;
    if entries.is_empty() {
        let semver1 = format!("{base_url}/v3/registration5-semver1/{id_lower}/index.json");
        entries = nuget_entries_from_index(http, package, &semver1, false)?;
    }
    if entries.is_empty() {
        return Err(DotnetError::MissingReleaseMetadata(format!(
            "no NuGet versions with publish times found for {}",
            package.as_str()
        )));
    }
    Ok(ReleaseTimeline::new(entries))
}

fn nuget_entries_from_index(
    http: &HttpClient,
    package: &PackageName,
    index_url: &str,
    gzipped: bool,
) -> Result<Vec<ReleaseEntry>, DotnetError> {
    let index_body = fetch_registration_text(http, index_url, gzipped)?;
    let index: NugetRegistrationIndex =
        serde_json::from_str(&index_body).map_err(|err| DotnetError::Json(err.to_string()))?;
    let mut entries = Vec::new();
    for page_ref in index.items {
        let page_body = fetch_registration_text(http, &page_ref.id, gzipped)?;
        entries.extend(parse_nuget_page_json(&page_body).map_err(|err| match err {
            DotnetError::InvalidTimestamp { version, value } => {
                DotnetError::InvalidTimestamp { version, value }
            }
            other => DotnetError::Json(format!(
                "failed to parse NuGet registration page for {}: {other}",
                package.as_str()
            )),
        })?);
    }
    Ok(entries)
}

fn fetch_registration_text(
    http: &HttpClient,
    url: &str,
    gzipped: bool,
) -> Result<String, DotnetError> {
    if !gzipped {
        return Ok(http
            .get_text(url)
            .map_err(|err| DotnetError::Infra(err.to_string()))?
            .body);
    }

    let bytes = http
        .get_bytes(url)
        .map_err(|err| DotnetError::Infra(err.to_string()))?
        .body;
    let mut decoder = GzDecoder::new(std::io::Cursor::new(bytes));
    let mut out = String::new();
    decoder.read_to_string(&mut out).map_err(|err| {
        DotnetError::Infra(format!("failed to gunzip NuGet payload from {url}: {err}"))
    })?;
    Ok(out)
}

fn exact_command_for_item(item: &ResolvedExecutionItem) -> CommandSpec {
    exact_command_parts(&item.package_name, &item.target_version)
}

fn exact_command_parts(package_name: &PackageName, target_version: &VersionText) -> CommandSpec {
    CommandSpec::new(
        "dotnet",
        [
            "tool",
            "update",
            "--global",
            package_name.as_str(),
            "--version",
            target_version.as_str(),
            "--allow-downgrade",
        ],
    )
    .mutating()
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
    ManagerId::new(MANAGER_ID).expect("static dotnet manager id should be valid")
}

fn installed_tool(package: DotnetToolPackage) -> Result<InstalledTool, DotnetError> {
    Ok(InstalledTool::new(
        manager_id(),
        ToolId::new(package.package_id.as_str().to_owned())?,
        package.package_id.clone(),
        ToolName::new(package.package_id.as_str().to_owned())?,
        package.version,
        ManagerMetadata::empty(),
    ))
}

fn update_input(tool: InstalledTool, lookup: ReleaseLookupResult) -> ManagerUpdateInput {
    let discovered_target = match &lookup {
        ReleaseLookupResult::Known(timeline) => {
            newest_semver_version(timeline).unwrap_or_else(|| tool.installed_version.clone())
        }
        ReleaseLookupResult::MissingMetadata | ReleaseLookupResult::LookupFailed(_) => {
            tool.installed_version.clone()
        }
    };
    ManagerUpdateInput::Seed(UpdateSeed::new(
        tool,
        discovered_target,
        VersionScheme::SemVer,
        lookup,
    ))
}

fn parse_timestamp(value: &str) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(value).ok()
}

fn system_time_from_datetime(datetime: DateTime<chrono::FixedOffset>) -> SystemTime {
    let timestamp = datetime.timestamp();
    if timestamp >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_secs(timestamp.unsigned_abs())
    } else {
        SystemTime::UNIX_EPOCH - Duration::from_secs(timestamp.unsigned_abs())
    }
}

fn adapter_error(err: &DotnetError) -> ManagerAdapterError {
    let kind = match err {
        DotnetError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        DotnetError::Json(_)
        | DotnetError::Domain(_)
        | DotnetError::InvalidTimestamp { .. }
        | DotnetError::MissingReleaseMetadata(_) => ManagerAdapterErrorKind::Parse,
        DotnetError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        DotnetError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail: err.to_string(),
    }
}
