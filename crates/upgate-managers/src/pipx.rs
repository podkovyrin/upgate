use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use pep440_rs::Version as Pep440Version;
use serde::Deserialize;
use upgate_domain::{
    AuditPackageName, AuditSubject, DomainError, ExecutionSupport, InstalledTool, ManagerConfig,
    ManagerId, ManagerScanInput, ManagerUpdateInput, OsvEcosystem, PackageName, ReleaseEntry,
    ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp, SkipReason, ToolId,
    ToolName, UpdateSeed, VersionScheme, VersionText,
};
use upgate_execution::{
    ExecutionCommand, ExecutionCommandIntent, ExecutionCommandItem, ResolvedExecutionItem,
    ResolvedExecutionPlan,
};
use upgate_infra::{
    CommandCheck, CommandSpec, Env, HttpClient, InfraError, ProcessRunner, effective_parallelism,
    run_ordered_parallel,
};
use upgate_release::newest_pep440_version;

use crate::adapter::{
    ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind, ManagerCapabilities,
    ReleaseLookupSubject, validate_version_policy,
};

const MANAGER_ID: &str = "pipx";
const PIPX_MAX_PARALLEL_CHECKS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
enum PipxError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    InvalidTimestamp { version: String, value: String },
    MissingReleaseMetadata(String),
    UnsupportedCommandIntent(&'static str),
}

impl Display for PipxError {
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
                    "unsupported pipx execution command intent `{kind}`"
                )
            }
        }
    }
}

impl std::error::Error for PipxError {}

impl From<InfraError> for PipxError {
    fn from(value: InfraError) -> Self {
        if value.is_interruption() {
            Self::Interrupted(value.to_string())
        } else {
            Self::Infra(value.to_string())
        }
    }
}

impl From<DomainError> for PipxError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PipxInstalledPackage {
    name: PackageName,
    version: VersionText,
    package_or_url: Option<String>,
    pip_args: Vec<String>,
    pinned: bool,
    locked: bool,
    suffix: String,
}

#[derive(Debug, Deserialize)]
struct PipxListRoot {
    #[serde(default)]
    venvs: BTreeMap<String, PipxVenv>,
}

#[derive(Debug, Deserialize)]
struct PipxVenv {
    metadata: PipxMetadata,
}

#[derive(Debug, Deserialize)]
struct PipxMetadata {
    main_package: PipxMainPackage,
}

#[derive(Debug, Deserialize)]
struct PipxMainPackage {
    package: String,
    package_version: String,
    #[serde(default)]
    package_or_url: Option<String>,
    #[serde(default)]
    pip_args: Vec<String>,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    lock_file: Option<serde_json::Value>,
    #[serde(default)]
    suffix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipxManager {
    config: ManagerConfig,
}

impl PipxManager {
    pub const fn new(config: ManagerConfig) -> Self {
        Self { config }
    }

    pub fn id() -> ManagerId {
        ManagerId::from_static(MANAGER_ID)
    }
}
impl ManagerAdapter for PipxManager {
    fn required_executable() -> &'static str {
        MANAGER_ID
    }

    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new()
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
        max_parallel_checks_per_manager: usize,
    ) -> Result<Vec<ManagerUpdateInput>, ManagerAdapterError> {
        validate_version_policy(
            &self.config.manager_id,
            Self::supports_version_policy(self.config.version_policy),
            self.config.version_policy,
        )?;
        update_inputs(process, http, env, max_parallel_checks_per_manager)
            .map_err(|err| adapter_error(&err))
    }

    fn commands_for_execution_plan(
        &self,
        _process: &ProcessRunner,
        _env: &Env,
        plan: &ResolvedExecutionPlan,
    ) -> Result<Vec<ExecutionCommand>, ManagerAdapterError> {
        commands_for_execution_plan(plan).map_err(|err| adapter_error(&err))
    }
}

/// Parses `pipx list --json`.
///
/// # Errors
///
/// Returns an error when JSON is malformed or a package/version is blank.
fn parse_list_json(raw: &str) -> Result<Vec<PipxInstalledPackage>, PipxError> {
    let parsed: PipxListRoot =
        serde_json::from_str(raw).map_err(|err| PipxError::Json(err.to_string()))?;
    let mut packages = BTreeMap::new();
    for venv in parsed.venvs.into_values() {
        let package = PipxInstalledPackage {
            name: PackageName::new(venv.metadata.main_package.package)?,
            version: VersionText::new(venv.metadata.main_package.package_version)?,
            package_or_url: venv.metadata.main_package.package_or_url,
            pip_args: venv.metadata.main_package.pip_args,
            pinned: venv.metadata.main_package.pinned,
            locked: venv.metadata.main_package.lock_file.is_some(),
            suffix: venv.metadata.main_package.suffix,
        };
        packages
            .entry(package.name.as_str().to_owned())
            .or_insert(package);
    }
    Ok(packages.into_values().collect())
}

/// Reads installed pipx main packages.
///
/// # Errors
///
/// Returns an error when command output cannot be parsed.
fn installed_global(process: &ProcessRunner) -> Result<Vec<InstalledTool>, PipxError> {
    let output = process.run(
        &CommandSpec::new("pipx", ["list", "--json"]),
        &CommandCheck::Success,
    )?;
    parse_list_json(output.stdout()?)?
        .into_iter()
        .map(installed_tool)
        .collect()
}

/// Discovers pipx packages that need release metadata before planning.
///
/// # Errors
///
/// Returns an error when installed discovery fails.
fn update_inputs(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    max_parallel_checks_per_manager: usize,
) -> Result<Vec<ManagerUpdateInput>, PipxError> {
    let output = process.run(
        &CommandSpec::new("pipx", ["list", "--json"]),
        &CommandCheck::Success,
    )?;
    let packages = parse_list_json(output.stdout()?)?;
    let threads = effective_parallelism(max_parallel_checks_per_manager, PIPX_MAX_PARALLEL_CHECKS);
    run_ordered_parallel(packages, threads, MANAGER_ID, |package| {
        let constraint = pipx_constraint(&package);
        let tool = installed_tool(package)?;
        let cutoff = match constraint {
            Ok(cutoff) => cutoff,
            Err(reason) => {
                return Ok(ManagerUpdateInput::Skipped {
                    installed: tool,
                    reason,
                });
            }
        };
        let lookup = apply_upload_cutoff(lookup_release(http, env, &tool.package_name), cutoff);
        Ok(update_input(tool, lookup))
    })?
    .into_iter()
    .collect()
}

fn pipx_constraint(package: &PipxInstalledPackage) -> Result<Option<SystemTime>, SkipReason> {
    if package.pinned {
        return Err(SkipReason::Pinned);
    }
    if package.locked {
        return Err(SkipReason::ManagerRule(
            "pipx package is controlled by a lock file".to_owned(),
        ));
    }
    if !package.suffix.is_empty() {
        return Err(SkipReason::ManagerRule(format!(
            "pipx package uses unsupported suffix `{}`",
            package.suffix
        )));
    }
    if package
        .package_or_url
        .as_deref()
        .is_some_and(|source| !is_bare_package_identity(source, package.name.as_str()))
    {
        return Err(SkipReason::ManagerRule(
            "pipx package was installed from a URL, path, or versioned requirement".to_owned(),
        ));
    }

    let mut cutoff = None;
    let mut args = package.pip_args.iter();
    while let Some(arg) = args.next() {
        let cutoff_value = if arg == "--uploaded-prior-to" {
            Some(
                args.next()
                    .ok_or_else(|| {
                        SkipReason::ManagerRule(
                            "pipx has an incomplete --uploaded-prior-to constraint".to_owned(),
                        )
                    })?
                    .as_str(),
            )
        } else {
            arg.strip_prefix("--uploaded-prior-to=")
        };
        if let Some(value) = cutoff_value {
            let parsed = parse_timestamp(value).ok_or_else(|| {
                SkipReason::ManagerRule(format!(
                    "pipx has an invalid --uploaded-prior-to constraint `{value}`"
                ))
            })?;
            let parsed = system_time_from_datetime(parsed);
            cutoff = Some(cutoff.map_or(parsed, |current: SystemTime| current.min(parsed)));
            continue;
        }
        if changes_package_source(arg) {
            return Err(SkipReason::ManagerRule(format!(
                "pipx package uses unsupported package-source argument `{arg}`"
            )));
        }
    }
    Ok(cutoff)
}

fn is_bare_package_identity(source: &str, package: &str) -> bool {
    source
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
        && normalize_python_name(source) == normalize_python_name(package)
}

fn normalize_python_name(value: &str) -> String {
    let mut normalized = String::new();
    let mut separator = false;
    for character in value.chars() {
        if matches!(character, '-' | '_' | '.') {
            if !separator {
                normalized.push('-');
            }
            separator = true;
        } else {
            normalized.push(character.to_ascii_lowercase());
            separator = false;
        }
    }
    normalized
}

fn changes_package_source(arg: &str) -> bool {
    [
        "-i",
        "-f",
        "--index-url",
        "--extra-index-url",
        "--find-links",
        "--no-index",
    ]
    .iter()
    .any(|option| arg == *option || arg.starts_with(&format!("{option}=")))
}

fn apply_upload_cutoff(
    lookup: ReleaseLookupResult,
    cutoff: Option<SystemTime>,
) -> ReleaseLookupResult {
    match (lookup, cutoff) {
        (ReleaseLookupResult::Known(timeline), Some(cutoff)) => {
            ReleaseLookupResult::Known(ReleaseTimeline::new(
                timeline
                    .versions
                    .into_iter()
                    .filter(|release| release.published_at.as_system_time() <= cutoff)
                    .collect(),
            ))
        }
        (lookup, _) => lookup,
    }
}

/// Looks up `PyPI` release metadata.
fn lookup_release(http: &HttpClient, env: &Env, package: &PackageName) -> ReleaseLookupResult {
    let base_url = upgate_infra::env_base_url(env, "upgate_PIPX_PYPI_BASE_URL", "https://pypi.org");
    let url = format!("{base_url}/pypi/{package}/json");
    match http.get_text(&url) {
        Ok(response) => match parse_pypi_json(package, &response.body) {
            Ok(timeline) => ReleaseLookupResult::Known(timeline),
            Err(PipxError::MissingReleaseMetadata(_)) => ReleaseLookupResult::MissingMetadata,
            Err(err) => ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string())),
        },
        Err(err) => ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string())),
    }
}

/// Parses `PyPI` package metadata into a release timeline.
///
/// # Errors
///
/// Returns an error when JSON or timestamps are invalid, or no version
/// timestamps are present.
fn parse_pypi_json(package: &PackageName, raw: &str) -> Result<ReleaseTimeline, PipxError> {
    let root: PypiRoot =
        serde_json::from_str(raw).map_err(|err| PipxError::Json(err.to_string()))?;
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

/// Creates pipx commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when the resolved execution mode is not supported.
fn commands_for_execution_plan(
    plan: &ResolvedExecutionPlan,
) -> Result<Vec<ExecutionCommand>, PipxError> {
    let mut commands = Vec::new();
    for intent in &plan.intents {
        match intent {
            ExecutionCommandIntent::Exact(item) => {
                commands.push(ExecutionCommand {
                    items: vec![ExecutionCommandItem::from(item)],
                    command: exact_command_for_item(item)?,
                });
            }
            ExecutionCommandIntent::NativeSelected(_) => {
                return Err(PipxError::UnsupportedCommandIntent("native-selected"));
            }
            ExecutionCommandIntent::ResolverNative(_) => {
                return Err(PipxError::UnsupportedCommandIntent("resolver-native"));
            }
            ExecutionCommandIntent::ResolverNativeGlobal(_) => {
                return Err(PipxError::UnsupportedCommandIntent(
                    "resolver-native-global",
                ));
            }
            ExecutionCommandIntent::NativeGlobal(_) => {
                return Err(PipxError::UnsupportedCommandIntent("native-global"));
            }
        }
    }
    Ok(commands)
}

fn exact_command_for_item(item: &ResolvedExecutionItem) -> Result<CommandSpec, PipxError> {
    let target_version = item
        .known_target_version()
        .ok_or(PipxError::UnsupportedCommandIntent(
            "exact-without-known-target",
        ))?;
    let spec = format!("{}=={target_version}", item.package_name);
    Ok(CommandSpec::new(
        "pipx",
        ["install", "--upgrade", "--skip-maintenance", &spec],
    )
    .mutating())
}

fn installed_tool(package: PipxInstalledPackage) -> Result<InstalledTool, PipxError> {
    Ok(InstalledTool::new(
        PipxManager::id(),
        ToolId::new(package.name.as_str())?,
        package.name.clone(),
        ToolName::new(package.name.as_str())?,
        package.version,
    )
    .with_audit_subject(AuditSubject::new(
        OsvEcosystem::Pypi,
        AuditPackageName::new(package.name.as_str())?,
    )))
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

fn update_input(tool: InstalledTool, lookup: ReleaseLookupResult) -> ManagerUpdateInput {
    let discovered_target = match &lookup {
        ReleaseLookupResult::Known(timeline) => {
            newest_pep440_version(timeline).unwrap_or_else(|| tool.installed_version.clone())
        }
        ReleaseLookupResult::MissingMetadata | ReleaseLookupResult::LookupFailed(_) => {
            tool.installed_version.clone()
        }
    };
    ManagerUpdateInput::Seed(UpdateSeed::new(
        tool,
        discovered_target,
        VersionScheme::Pep440,
        lookup,
        ExecutionSupport::exact_only(),
    ))
}

fn time_map_to_timeline(
    package: &PackageName,
    timestamps: BTreeMap<String, String>,
) -> Result<ReleaseTimeline, PipxError> {
    if timestamps.is_empty() {
        return Err(PipxError::MissingReleaseMetadata(format!(
            "registry time metadata is empty for {package}"
        )));
    }

    let mut entries = Vec::new();
    for (version, timestamp) in timestamps {
        let parsed = parse_timestamp(&timestamp).ok_or_else(|| PipxError::InvalidTimestamp {
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
) -> Result<Option<String>, PipxError> {
    let mut newest = None::<(DateTime<chrono::FixedOffset>, String)>;
    for file in files {
        let Some(timestamp) = file
            .upload_time_iso_8601
            .or(file.upload_time)
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let parsed = parse_timestamp(&timestamp).ok_or_else(|| PipxError::InvalidTimestamp {
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

fn adapter_error(err: &PipxError) -> ManagerAdapterError {
    let kind = match err {
        PipxError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        PipxError::Json(_)
        | PipxError::Domain(_)
        | PipxError::InvalidTimestamp { .. }
        | PipxError::MissingReleaseMetadata(_) => ManagerAdapterErrorKind::Parse,
        PipxError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        PipxError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        kind,
        detail: err.to_string(),
    }
}
