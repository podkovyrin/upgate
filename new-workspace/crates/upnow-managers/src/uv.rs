use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use pep440_rs::Version as Pep440Version;
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

pub const MANAGER_ID: &str = "uv";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UvError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    InvalidTimestamp { version: String, value: String },
    MissingReleaseMetadata(String),
    UnsupportedCommandIntent(String),
}

impl Display for UvError {
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
                    "invalid timestamp `{value}` for version `{version}`"
                )
            }
            Self::UnsupportedCommandIntent(kind) => {
                write!(
                    formatter,
                    "unsupported uv execution command intent `{kind}`"
                )
            }
        }
    }
}

impl std::error::Error for UvError {}

impl From<InfraError> for UvError {
    fn from(value: InfraError) -> Self {
        if value.is_interruption() {
            Self::Interrupted(value.to_string())
        } else {
            Self::Infra(value.to_string())
        }
    }
}

impl From<DomainError> for UvError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
    }
}

impl UvError {
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UvTool {
    pub name: PackageName,
    pub current: VersionText,
    pub python_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UvManager {
    config: ManagerConfig,
}

impl UvManager {
    pub const fn new(config: ManagerConfig) -> Self {
        Self { config }
    }
}
impl ManagerAdapter for UvManager {
    fn id(&self) -> &'static str {
        MANAGER_ID
    }

    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new()
    }

    fn supports_version_policy(&self, policy: VersionPolicy) -> bool {
        policy == VersionPolicy::None
    }

    fn scan_inputs(
        &self,
        process: &ProcessRunner,
        _env: &Env,
    ) -> Result<Vec<ManagerScanInput>, ManagerAdapterError> {
        installed_global(process)
            .map(|tools| {
                tools
                    .into_iter()
                    .map(|tool| installed_tool(&tool).map(ManagerScanInput::Installed))
                    .collect::<Result<Vec<_>, _>>()
            })
            .and_then(|items| items)
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
pub fn parse_installed_tool_line(line: &str, tool_dir: &str) -> Option<UvTool> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return None;
    }

    let mut parts = trimmed.split_whitespace();
    let name = PackageName::new(parts.next()?).ok()?;
    let current = VersionText::new(strip_v_prefix(parts.next()?)).ok()?;
    let python_path = uv_tool_python_path(tool_dir, name.as_str());
    Some(UvTool {
        name,
        current,
        python_path,
    })
}
pub fn parse_install_target_for_package(
    text: &str,
    package_name: &PackageName,
) -> Option<VersionText> {
    let package_norm = normalize_package_name(package_name.as_str());
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("+ ") else {
            continue;
        };
        let Some((name, version)) = rest.split_once("==") else {
            continue;
        };
        if normalize_package_name(name) == package_norm {
            return VersionText::new(version.trim()).ok();
        }
    }
    None
}

/// Reads installed uv tools.
///
/// # Errors
///
/// Returns an error when `uv tool dir`, list parsing, or receipt fallback fails.
pub fn installed_global(process: &ProcessRunner) -> Result<Vec<UvTool>, UvError> {
    let tool_dir = uv_tool_dir(process)?;
    let output = process.run(
        &CommandSpec::new("uv", ["tool", "list", "--show-version-specifiers"]),
        &CommandCheck::Success,
    )?;
    let parsed = output
        .stdout()?
        .lines()
        .filter_map(|line| parse_installed_tool_line(line, &tool_dir))
        .collect::<Vec<_>>();
    if parsed.is_empty() {
        installed_from_receipts(process, &tool_dir)
    } else {
        Ok(parsed)
    }
}

/// Builds uv planning inputs by resolving each installed tool with uv's dry-run solver.
///
/// # Errors
///
/// Returns an error when installed discovery fails.
pub fn update_inputs(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    min_release_age: Duration,
) -> Result<Vec<ManagerUpdateInput>, UvError> {
    let min_age_arg = duration_arg(min_release_age);
    let installed = installed_global(process)?;
    let mut inputs = Vec::new();
    for tool in installed {
        let installed = installed_tool(&tool)?;
        let target = match resolve_target_with_exclude_newer(process, &tool, &min_age_arg) {
            Ok(target) => target,
            Err(err @ UvError::Interrupted(_)) => return Err(err),
            Err(err) => {
                inputs.push(ManagerUpdateInput::ResolverError {
                    installed,
                    message: err.to_string(),
                });
                continue;
            }
        };
        let selected_target = lookup_uv_selected_target(http, env, &tool.name, target);
        inputs.push(ManagerUpdateInput::Seed(UpdateSeed::manager_selected(
            installed,
            selected_target,
            VersionScheme::Pep440,
            ExecutionEligibility::ResolverNativeOnly,
        )));
    }
    Ok(inputs)
}

/// Looks up `PyPI` release metadata for a uv tool.
pub fn lookup_release(http: &HttpClient, env: &Env, package: &PackageName) -> ReleaseLookupResult {
    let base_url = upnow_infra::env_base_url(env, "UPNOW_UV_PYPI_BASE_URL", "https://pypi.org");
    let url = format!("{base_url}/pypi/{}/json", package.as_str());
    match http.get_text(&url) {
        Ok(response) => match parse_pypi_json(package, &response.body) {
            Ok(timeline) => ReleaseLookupResult::Known(timeline),
            Err(UvError::MissingReleaseMetadata(_)) => ReleaseLookupResult::MissingMetadata,
            Err(err) => ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string())),
        },
        Err(err) => ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string())),
    }
}

fn lookup_uv_selected_target(
    http: &HttpClient,
    env: &Env,
    package: &PackageName,
    target: VersionText,
) -> ManagerSelectedTarget {
    let lookup = lookup_release(http, env, package);
    let target_age = match &lookup {
        ReleaseLookupResult::Known(timeline) => matching_versions(timeline, &target)
            .first()
            .map_or(TargetAgeLookupResult::MissingMetadata, |entry| {
                TargetAgeLookupResult::Known(TargetAgeEvidence::PublishedAt(
                    entry.published_at.clone(),
                ))
            }),
        ReleaseLookupResult::MissingMetadata => TargetAgeLookupResult::MissingMetadata,
        ReleaseLookupResult::LookupFailed(err) => TargetAgeLookupResult::LookupFailed(err.clone()),
    };
    ManagerSelectedTarget::new(target, target_age)
}

fn matching_versions(timeline: &ReleaseTimeline, target: &VersionText) -> Vec<ReleaseEntry> {
    let parsed_target = Pep440Version::from_str(target.as_str()).ok();
    timeline
        .versions
        .iter()
        .filter(|entry| {
            parsed_target.as_ref().map_or_else(
                || entry.version.as_str() == target.as_str(),
                |target| {
                    Pep440Version::from_str(entry.version.as_str())
                        .is_ok_and(|version| version == *target)
                },
            )
        })
        .cloned()
        .collect()
}

/// Parses `PyPI` package metadata into a release timeline.
///
/// # Errors
///
/// Returns an error when JSON or timestamps are invalid, or no version
/// timestamps are present.
pub fn parse_pypi_json(package: &PackageName, raw: &str) -> Result<ReleaseTimeline, UvError> {
    let root: PypiRoot = serde_json::from_str(raw).map_err(|err| UvError::Json(err.to_string()))?;
    let mut timestamps = BTreeMap::new();
    for (version, files) in root.releases {
        if Pep440Version::from_str(&version).is_err() {
            continue;
        }
        let Some(timestamp) = newest_pypi_upload_timestamp(&version, files)? else {
            continue;
        };
        timestamps.insert(version, timestamp);
    }
    time_map_to_timeline(package, timestamps)
}

/// Creates uv commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when the resolved execution mode is not supported.
pub fn commands_for_execution_plan(
    plan: &ResolvedExecutionPlan,
    min_release_age: Duration,
) -> Result<Vec<ExecutionCommand>, UvError> {
    let min_age_arg = duration_arg(min_release_age);
    let mut commands = Vec::new();
    for intent in &plan.intents {
        match intent {
            ExecutionCommandIntent::ResolverNative(item) => {
                commands.push(ExecutionCommand {
                    items: vec![execution_item(item)],
                    command: tool_install_command(&item.package_name, &min_age_arg),
                });
            }
            ExecutionCommandIntent::ResolverNativeGlobal(_) => {
                return Err(UvError::UnsupportedCommandIntent(
                    "resolver-native-global".to_owned(),
                ));
            }
            ExecutionCommandIntent::GroupedNative(_) => {
                return Err(UvError::UnsupportedCommandIntent(
                    "grouped-native".to_owned(),
                ));
            }
            ExecutionCommandIntent::Exact(_) => {
                return Err(UvError::UnsupportedCommandIntent("exact".to_owned()));
            }
            ExecutionCommandIntent::NativeGlobal(_) => {
                return Err(UvError::UnsupportedCommandIntent(
                    "native-global".to_owned(),
                ));
            }
            ExecutionCommandIntent::NativeSelected(_) => {
                return Err(UvError::UnsupportedCommandIntent(
                    "native-selected".to_owned(),
                ));
            }
        }
    }
    Ok(commands)
}
fn tool_install_command(package_name: &PackageName, min_age_arg: &str) -> CommandSpec {
    CommandSpec::new(
        "uv",
        [
            "tool",
            "install",
            "--upgrade",
            "--exclude-newer",
            min_age_arg,
            package_name.as_str(),
        ],
    )
    .mutating()
}

fn uv_tool_dir(process: &ProcessRunner) -> Result<String, UvError> {
    let output = process.run(
        &CommandSpec::new("uv", ["tool", "dir"]),
        &CommandCheck::Success,
    )?;
    let path = output.stdout()?.trim();
    if path.is_empty() {
        return Err(UvError::Infra(
            "uv tool dir returned an empty path".to_owned(),
        ));
    }
    Ok(path.to_owned())
}

fn installed_from_receipts(
    process: &ProcessRunner,
    tool_dir: &str,
) -> Result<Vec<UvTool>, UvError> {
    let read_dir = std::fs::read_dir(tool_dir)
        .map_err(|err| UvError::Infra(format!("failed to read uv tool directory: {err}")))?;
    let mut tools = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|err| {
            UvError::Infra(format!("failed to read uv tool directory entry: {err}"))
        })?;
        let path = entry.path();
        if !path.is_dir() || !path.join("uv-receipt.toml").exists() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let package = PackageName::new(name.to_owned())?;
        let python_path = uv_tool_python_path(tool_dir, package.as_str());
        let current = python_package_version(process, &python_path, &package)?;
        tools.push(UvTool {
            name: package,
            current,
            python_path,
        });
    }
    Ok(tools)
}

fn python_package_version(
    process: &ProcessRunner,
    python_path: &str,
    package: &PackageName,
) -> Result<VersionText, UvError> {
    let script = "import importlib.metadata as m\nimport sys\nprint(m.version(sys.argv[1]))\n";
    let output = process.run(
        &CommandSpec::new(python_path, ["-c", script, package.as_str()]),
        &CommandCheck::Success,
    )?;
    let version = output.stdout()?.trim();
    if version.is_empty() {
        return Err(UvError::Infra(format!(
            "python returned empty version for uv tool '{}'",
            package.as_str()
        )));
    }
    VersionText::new(version).map_err(UvError::from)
}

fn resolve_target_with_exclude_newer(
    process: &ProcessRunner,
    tool: &UvTool,
    min_age_arg: &str,
) -> Result<VersionText, UvError> {
    let requirement = if Pep440Version::from_str(tool.current.as_str()).is_ok() {
        format!("{}>={}", tool.name.as_str(), tool.current.as_str())
    } else {
        tool.name.as_str().to_owned()
    };
    let output = process.run(
        &CommandSpec::new(
            "uv",
            [
                "pip",
                "install",
                "--dry-run",
                "-p",
                &tool.python_path,
                "--upgrade",
                "--exclude-newer",
                min_age_arg,
                &requirement,
            ],
        ),
        &CommandCheck::Success,
    )?;
    let stdout = output.stdout()?;
    let stderr = output.stderr().unwrap_or_default();
    let combined = format!("{stdout}\n{stderr}");
    Ok(parse_install_target_for_package(&combined, &tool.name)
        .unwrap_or_else(|| tool.current.clone()))
}

fn installed_tool(tool: &UvTool) -> Result<InstalledTool, UvError> {
    Ok(InstalledTool::new(
        manager_id(),
        ToolId::new(tool.name.as_str().to_owned())?,
        tool.name.clone(),
        ToolName::new(tool.name.as_str().to_owned())?,
        tool.current.clone(),
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
    ManagerId::new(MANAGER_ID).expect("static uv manager id should be valid")
}

#[derive(Debug, Deserialize)]
struct PypiRoot {
    #[serde(default)]
    releases: BTreeMap<String, Vec<PypiReleaseFile>>,
}

#[derive(Debug, Deserialize)]
struct PypiReleaseFile {
    upload_time_iso_8601: Option<String>,
    upload_time: Option<String>,
}

fn time_map_to_timeline(
    package: &PackageName,
    timestamps: BTreeMap<String, String>,
) -> Result<ReleaseTimeline, UvError> {
    if timestamps.is_empty() {
        return Err(UvError::MissingReleaseMetadata(format!(
            "registry time metadata is empty for {}",
            package.as_str()
        )));
    }

    let mut entries = Vec::new();
    for (version, timestamp) in timestamps {
        let parsed = parse_timestamp(&timestamp).ok_or_else(|| UvError::InvalidTimestamp {
            version: version.clone(),
            value: timestamp.clone(),
        })?;
        entries.push(ReleaseEntry::new(
            VersionText::new(version)?,
            ReleaseTimestamp::new(system_time_from_datetime(parsed)),
        ));
    }
    Ok(ReleaseTimeline::new(entries))
}

fn newest_pypi_upload_timestamp(
    version: &str,
    files: Vec<PypiReleaseFile>,
) -> Result<Option<String>, UvError> {
    let mut newest = None::<(DateTime<chrono::FixedOffset>, String)>;
    for file in files {
        let Some(timestamp) = file
            .upload_time_iso_8601
            .or(file.upload_time)
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let parsed = parse_timestamp(&timestamp).ok_or_else(|| UvError::InvalidTimestamp {
            version: version.to_owned(),
            value: timestamp.clone(),
        })?;
        if newest.as_ref().is_none_or(|(current, _)| parsed > *current) {
            newest = Some((parsed, timestamp));
        }
    }
    Ok(newest.map(|(_, timestamp)| timestamp))
}

fn parse_timestamp(value: &str) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(value).ok().or_else(|| {
        let naive = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
        Some(DateTime::from_naive_utc_and_offset(
            naive,
            chrono::FixedOffset::east_opt(0)?,
        ))
    })
}

fn system_time_from_datetime(datetime: DateTime<chrono::FixedOffset>) -> SystemTime {
    let timestamp = datetime.timestamp();
    if timestamp >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_secs(timestamp.unsigned_abs())
    } else {
        SystemTime::UNIX_EPOCH - Duration::from_secs(timestamp.unsigned_abs())
    }
}

fn uv_tool_python_path(tool_dir: &str, tool_name: &str) -> String {
    let unix = PathBuf::from(tool_dir)
        .join(tool_name)
        .join("bin")
        .join("python");
    if unix.exists() {
        return unix.to_string_lossy().to_string();
    }
    Path::new(tool_dir)
        .join(tool_name)
        .join("bin")
        .join("python")
        .to_string_lossy()
        .to_string()
}

fn normalize_package_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

fn strip_v_prefix(value: &str) -> &str {
    value.strip_prefix('v').unwrap_or(value)
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

fn adapter_error(err: &UvError) -> ManagerAdapterError {
    let kind = match err {
        UvError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        UvError::Json(_)
        | UvError::Domain(_)
        | UvError::InvalidTimestamp { .. }
        | UvError::MissingReleaseMetadata(_) => ManagerAdapterErrorKind::Parse,
        UvError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        UvError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail: err.to_string(),
    }
}
