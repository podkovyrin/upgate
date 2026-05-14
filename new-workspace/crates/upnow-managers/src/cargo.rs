use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use semver::Version;
use serde::Deserialize;
use upnow_domain::{
    DomainError, ExecutionSupport, InstalledTool, ManagerConfig, ManagerId, ManagerMetadata,
    ManagerScanInput, ManagerUpdateInput, PackageName, ReleaseEntry, ReleaseLookupError,
    ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp, ToolId, ToolName, UpdateSeed,
    VersionScheme, VersionText,
};
use upnow_execution::{
    ExecutionCommand, ExecutionCommandIntent, ExecutionCommandItem, ResolvedExecutionItem,
    ResolvedExecutionPlan,
};
use upnow_infra::{CommandCheck, CommandSpec, Env, HttpClient, InfraError, ProcessRunner};
use upnow_release::newest_semver_version;

use crate::adapter::{
    ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind, ManagerCapabilities,
    ReleaseLookupSubject, validate_version_policy,
};

pub const MANAGER_ID: &str = "cargo";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CargoError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    SearchParse(String),
    LedgerRead(String),
    InvalidTimestamp { version: String, value: String },
    MissingReleaseMetadata(String),
    UnsupportedCommandIntent(String),
}

impl Display for CargoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Infra(detail)
            | Self::Interrupted(detail)
            | Self::Json(detail)
            | Self::Domain(detail)
            | Self::SearchParse(detail)
            | Self::LedgerRead(detail)
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
                    "unsupported cargo execution command intent `{kind}`"
                )
            }
        }
    }
}

impl std::error::Error for CargoError {}

impl From<InfraError> for CargoError {
    fn from(value: InfraError) -> Self {
        if value.is_interruption() {
            Self::Interrupted(value.to_string())
        } else {
            Self::Infra(value.to_string())
        }
    }
}

impl From<DomainError> for CargoError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
    }
}

impl CargoError {
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledCrate {
    pub name: PackageName,
    pub version: VersionText,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CargoInstallMeta {
    pub bins: Vec<String>,
    pub features: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
}

#[derive(Debug, Deserialize)]
struct CargoInstallLedger {
    #[serde(default)]
    installs: BTreeMap<String, CargoInstallLedgerEntry>,
}

#[derive(Debug, Deserialize)]
struct CargoInstallLedgerEntry {
    #[serde(default)]
    bins: Vec<String>,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    all_features: bool,
    #[serde(default)]
    no_default_features: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoManager {
    config: ManagerConfig,
}

impl CargoManager {
    pub const fn new(config: ManagerConfig) -> Self {
        Self { config }
    }

    pub fn id() -> ManagerId {
        ManagerId::from_static(MANAGER_ID)
    }
}
impl ManagerAdapter for CargoManager {
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
    ) -> Result<Vec<ManagerUpdateInput>, ManagerAdapterError> {
        validate_version_policy(
            &self.config.manager_id,
            Self::supports_version_policy(self.config.version_policy),
            self.config.version_policy,
        )?;
        update_inputs(process, http, env).map_err(|err| adapter_error(&err))
    }

    fn commands_for_execution_plan(
        &self,
        _process: &ProcessRunner,
        env: &Env,
        plan: &ResolvedExecutionPlan,
    ) -> Result<Vec<ExecutionCommand>, ManagerAdapterError> {
        commands_for_execution_plan(env, plan).map_err(|err| adapter_error(&err))
    }
}

/// Parses `cargo install --list`.
///
/// # Errors
///
/// Returns an error when a parsed crate name or version is blank.
pub fn parse_install_list(raw: &str) -> Result<Vec<InstalledCrate>, CargoError> {
    let mut crates = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if !trimmed.ends_with(':') {
            continue;
        }
        let Some((name, version)) = trimmed.trim_end_matches(':').split_once(" v") else {
            continue;
        };
        if name.is_empty() || version.is_empty() {
            continue;
        }
        crates.insert(name.to_owned(), version.to_owned());
    }

    crates
        .into_iter()
        .map(|(name, version)| {
            Ok(InstalledCrate {
                name: PackageName::new(name)?,
                version: VersionText::new(version)?,
            })
        })
        .collect()
}

/// Parses Cargo's `.crates2.json` install ledger.
///
/// # Errors
///
/// Returns an error when JSON is malformed.
pub fn parse_install_ledger(raw: &str) -> Result<BTreeMap<String, CargoInstallMeta>, CargoError> {
    let parsed: CargoInstallLedger =
        serde_json::from_str(raw).map_err(|err| CargoError::Json(err.to_string()))?;
    let mut out = BTreeMap::new();
    for (key, value) in parsed.installs {
        if let Some(crate_name) = parse_ledger_key_name(&key) {
            out.insert(
                crate_name,
                CargoInstallMeta {
                    bins: value.bins,
                    features: value.features,
                    all_features: value.all_features,
                    no_default_features: value.no_default_features,
                },
            );
        }
    }
    Ok(out)
}
pub fn parse_ledger_key_name(key: &str) -> Option<String> {
    let (left, _) = key.split_once(" (")?;
    let (name, _) = left.rsplit_once(' ')?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

/// Parses the exact crate row from `cargo search <crate> --limit 1`.
///
/// # Errors
///
/// Returns an error when no exact row is found or the version is not `SemVer`.
pub fn parse_search_latest_version(
    crate_name: &PackageName,
    raw: &str,
) -> Result<Version, CargoError> {
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("...") || trimmed.starts_with("note:") {
            continue;
        }
        let prefix = format!("{} = \"", crate_name.as_str());
        if let Some(rest) = trimmed.strip_prefix(&prefix)
            && let Some((version, _)) = rest.split_once('"')
        {
            return Version::parse(version).map_err(|err| {
                CargoError::SearchParse(format!(
                    "failed to parse cargo search version for {}: {err}",
                    crate_name.as_str()
                ))
            });
        }
    }
    Err(CargoError::SearchParse(format!(
        "failed to parse cargo search latest version for {}",
        crate_name.as_str()
    )))
}

/// Reads installed Cargo crates.
///
/// # Errors
///
/// Returns an error when command output cannot be parsed.
pub fn installed_global(process: &ProcessRunner) -> Result<Vec<InstalledTool>, CargoError> {
    let output = process.run(
        &CommandSpec::new("cargo", ["install", "--list"]),
        &CommandCheck::Success,
    )?;
    parse_install_list(output.stdout()?)?
        .into_iter()
        .map(installed_tool)
        .collect()
}

/// Discovers Cargo crates that need release metadata before planning.
///
/// # Errors
///
/// Returns an error when installed discovery fails.
pub fn update_inputs(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
) -> Result<Vec<ManagerUpdateInput>, CargoError> {
    let mut inputs = Vec::new();
    for tool in installed_global(process)? {
        match search_latest_version(process, &tool.package_name) {
            Ok(_target) => {
                let lookup = lookup_release(http, env, &tool.package_name);
                let target = discovered_target(&tool, &lookup);
                inputs.push(ManagerUpdateInput::Seed(UpdateSeed::new(
                    tool,
                    target,
                    VersionScheme::SemVer,
                    lookup,
                    ExecutionSupport::exact_only(),
                )));
            }
            Err(err) if err.is_interruption() => return Err(err),
            Err(err) => inputs.push(ManagerUpdateInput::ResolverError {
                installed: tool,
                message: err.to_string(),
            }),
        }
    }
    Ok(inputs)
}

/// Looks up crates.io release metadata.
pub fn lookup_release(http: &HttpClient, env: &Env, package: &PackageName) -> ReleaseLookupResult {
    let base_url =
        upnow_infra::env_base_url(env, "UPNOW_CARGO_CRATES_IO_BASE_URL", "https://crates.io");
    let url = format!("{base_url}/api/v1/crates/{}", package.as_str());
    match http.get_text(&url) {
        Ok(response) => match parse_crates_io_json(package, &response.body) {
            Ok(timeline) => ReleaseLookupResult::Known(timeline),
            Err(CargoError::MissingReleaseMetadata(_)) => ReleaseLookupResult::MissingMetadata,
            Err(err) => ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string())),
        },
        Err(err) => ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string())),
    }
}

/// Parses crates.io crate metadata into a release timeline.
///
/// # Errors
///
/// Returns an error when JSON or timestamps are invalid, or no non-yanked
/// version timestamps are present.
pub fn parse_crates_io_json(
    package: &PackageName,
    raw: &str,
) -> Result<ReleaseTimeline, CargoError> {
    let root: CratesIoRoot =
        serde_json::from_str(raw).map_err(|err| CargoError::Json(err.to_string()))?;
    let mut timestamps = BTreeMap::new();
    for version in root.versions {
        if !version.yanked {
            timestamps.insert(version.num, version.created_at);
        }
    }
    time_map_to_timeline(package, timestamps)
}

/// Creates Cargo commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when the resolved execution mode is not supported.
pub fn commands_for_execution_plan(
    env: &Env,
    plan: &ResolvedExecutionPlan,
) -> Result<Vec<ExecutionCommand>, CargoError> {
    let install_meta = install_tracking_map(env).unwrap_or_default();
    let mut commands = Vec::new();
    for intent in &plan.intents {
        match intent {
            ExecutionCommandIntent::Exact(item) => {
                let meta = install_meta.get(item.package_name.as_str());
                commands.push(ExecutionCommand {
                    items: vec![ExecutionCommandItem::from(item)],
                    command: exact_command_for_item(item, meta),
                });
            }
            ExecutionCommandIntent::NativeSelected(_) => {
                return Err(CargoError::UnsupportedCommandIntent(
                    "native-selected".to_owned(),
                ));
            }
            ExecutionCommandIntent::ResolverNative(_) => {
                return Err(CargoError::UnsupportedCommandIntent(
                    "resolver-native".to_owned(),
                ));
            }
            ExecutionCommandIntent::ResolverNativeGlobal(_) => {
                return Err(CargoError::UnsupportedCommandIntent(
                    "resolver-native-global".to_owned(),
                ));
            }
            ExecutionCommandIntent::GroupedNative(_) => {
                return Err(CargoError::UnsupportedCommandIntent(
                    "grouped-native".to_owned(),
                ));
            }
            ExecutionCommandIntent::NativeGlobal(_) => {
                return Err(CargoError::UnsupportedCommandIntent(
                    "native-global".to_owned(),
                ));
            }
        }
    }
    Ok(commands)
}

/// Reads Cargo's registry-selected latest version from `cargo search`.
///
/// # Errors
///
/// Returns an error when the command fails or the exact search row cannot be
/// parsed.
pub fn search_latest_version(
    process: &ProcessRunner,
    package: &PackageName,
) -> Result<VersionText, CargoError> {
    let output = process.run(
        &CommandSpec::new("cargo", ["search", package.as_str(), "--limit", "1"]),
        &CommandCheck::Success,
    )?;
    Ok(VersionText::new(
        parse_search_latest_version(package, output.stdout()?)?.to_string(),
    )?)
}

fn exact_command_for_item(
    item: &ResolvedExecutionItem,
    meta: Option<&CargoInstallMeta>,
) -> CommandSpec {
    exact_command_parts(
        &item.package_name,
        item.known_target_version()
            .expect("exact command requires known target"),
        meta,
    )
}

fn exact_command_parts(
    package_name: &PackageName,
    target_version: &VersionText,
    meta: Option<&CargoInstallMeta>,
) -> CommandSpec {
    let mut args = vec!["install".to_owned(), "--force".to_owned()];
    add_install_meta_args(&mut args, meta);
    args.push(format!(
        "{}@{}",
        package_name.as_str(),
        target_version.as_str()
    ));
    CommandSpec::new("cargo", args).mutating()
}

fn add_install_meta_args(args: &mut Vec<String>, meta: Option<&CargoInstallMeta>) {
    let Some(meta) = meta else {
        return;
    };
    if !meta.bins.is_empty() {
        if meta.bins.len() == 1 {
            args.push("--bin".to_owned());
            args.push(meta.bins[0].clone());
        } else {
            args.push("--bins".to_owned());
        }
    }
    if meta.all_features {
        args.push("--all-features".to_owned());
    } else if !meta.features.is_empty() {
        args.push("--features".to_owned());
        args.push(meta.features.join(","));
    }
    if meta.no_default_features {
        args.push("--no-default-features".to_owned());
    }
}

fn discovered_target(tool: &InstalledTool, lookup: &ReleaseLookupResult) -> VersionText {
    match lookup {
        ReleaseLookupResult::Known(timeline) => {
            newest_semver_version(timeline).unwrap_or_else(|| tool.installed_version.clone())
        }
        ReleaseLookupResult::MissingMetadata | ReleaseLookupResult::LookupFailed(_) => {
            tool.installed_version.clone()
        }
    }
}

fn install_tracking_map(env: &Env) -> Result<BTreeMap<String, CargoInstallMeta>, CargoError> {
    let Some(path) = cargo_install_ledger_path(env) else {
        return Ok(BTreeMap::new());
    };
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let raw = fs::read_to_string(&path).map_err(|err| {
        CargoError::LedgerRead(format!("failed to read {}: {err}", path.display()))
    })?;
    parse_install_ledger(&raw)
}

fn cargo_install_ledger_path(env: &Env) -> Option<PathBuf> {
    env.var("CARGO_HOME")
        .and_then(|value| trimmed(&value).map(PathBuf::from))
        .or_else(|| {
            env.var("HOME")
                .and_then(|home| trimmed(&home).map(|home| PathBuf::from(home).join(".cargo")))
        })
        .map(|cargo_home| cargo_home.join(".crates2.json"))
}

fn trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn installed_tool(package: InstalledCrate) -> Result<InstalledTool, CargoError> {
    Ok(InstalledTool::new(
        CargoManager::id(),
        ToolId::new(package.name.as_str().to_owned())?,
        package.name.clone(),
        ToolName::new(package.name.as_str().to_owned())?,
        package.version,
        ManagerMetadata::empty(),
    ))
}

#[derive(Debug, Deserialize)]
struct CratesIoRoot {
    #[serde(default)]
    versions: Vec<CratesIoVersion>,
}

#[derive(Debug, Deserialize)]
struct CratesIoVersion {
    num: String,
    created_at: String,
    yanked: bool,
}

fn time_map_to_timeline(
    package: &PackageName,
    timestamps: BTreeMap<String, String>,
) -> Result<ReleaseTimeline, CargoError> {
    if timestamps.is_empty() {
        return Err(CargoError::MissingReleaseMetadata(format!(
            "registry time metadata is empty for {}",
            package.as_str()
        )));
    }

    let mut entries = Vec::new();
    for (version, timestamp) in timestamps {
        let parsed = parse_timestamp(&timestamp).ok_or_else(|| CargoError::InvalidTimestamp {
            version: version.clone(),
            value: timestamp.clone(),
        })?;
        entries.push(ReleaseEntry::new(
            VersionText::new(version)?,
            ReleaseTimestamp::new(system_time_from_datetime(parsed)),
        ));
    }
    if entries.is_empty() {
        return Err(CargoError::MissingReleaseMetadata(format!(
            "registry time metadata is empty for {}",
            package.as_str()
        )));
    }
    Ok(ReleaseTimeline::new(entries))
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

fn adapter_error(err: &CargoError) -> ManagerAdapterError {
    let kind = match err {
        CargoError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        CargoError::Json(_)
        | CargoError::Domain(_)
        | CargoError::SearchParse(_)
        | CargoError::LedgerRead(_)
        | CargoError::InvalidTimestamp { .. }
        | CargoError::MissingReleaseMetadata(_) => ManagerAdapterErrorKind::Parse,
        CargoError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        CargoError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail: err.to_string(),
    }
}
