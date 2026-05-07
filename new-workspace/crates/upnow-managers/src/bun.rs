use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

use chrono::DateTime;
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
    ManagerCapabilities, ManagerDefaultMode, ManagerDefaults, ManagerExecutionCommand,
    ManagerExecutionCommandItem, ReleaseLookupSubject,
};

pub const MANAGER_ID: &str = "bun";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BunError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    InvalidTimestamp { version: String, value: String },
    MissingReleaseMetadata(String),
    UnsupportedCommandIntent(String),
}

impl Display for BunError {
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

    fn defaults(&self) -> ManagerDefaults {
        ManagerDefaults {
            min_release_age: Duration::from_secs(7 * 24 * 60 * 60),
            mode: ManagerDefaultMode::Apply,
        }
    }

    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new(true, false).with_native_global_update(true)
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
        process: &ProcessRunner,
        _http: &HttpClient,
        env: &Env,
        subject: ReleaseLookupSubject<'_>,
    ) -> Result<ReleaseLookupResult, ManagerAdapterError> {
        let runtime = BunRuntime::resolve(process);
        lookup_release_with_bun(process, env, runtime.executable(), subject.package_name())
            .map_err(|err| adapter_error(&err))
    }

    fn update_inputs(
        &self,
        process: &ProcessRunner,
        _http: &HttpClient,
        env: &Env,
        version_policy: VersionPolicy,
        _min_release_age: Duration,
        _no_update: bool,
    ) -> Result<Vec<ManagerUpdateInput>, ManagerAdapterError> {
        self.validate_version_policy(version_policy)?;
        update_inputs(process, env).map_err(|err| adapter_error(&err))
    }

    fn commands_for_execution_plan(
        &self,
        process: &ProcessRunner,
        _env: &Env,
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
pub fn update_inputs(
    process: &ProcessRunner,
    env: &Env,
) -> Result<Vec<ManagerUpdateInput>, BunError> {
    let runtime = BunRuntime::resolve(process);
    let mut inputs = Vec::new();
    for tool in installed_global_with_bun(process, runtime.executable())? {
        let lookup =
            lookup_release_with_bun(process, env, runtime.executable(), &tool.package_name)?;
        inputs.push(update_input(tool, lookup));
    }
    Ok(inputs)
}

/// Looks up Bun registry release metadata.
///
/// # Errors
///
/// Returns an error only when command execution is interrupted.
pub fn lookup_release_with_bun(
    process: &ProcessRunner,
    env: &Env,
    bun: &str,
    package: &PackageName,
) -> Result<ReleaseLookupResult, BunError> {
    let Some(cwd) = bun_global_cwd(env) else {
        return Ok(ReleaseLookupResult::MissingMetadata);
    };
    let command = CommandSpec::new(
        bun,
        [
            "pm",
            "view",
            package.as_str(),
            "time",
            "--json",
            "--cwd",
            &cwd,
        ],
    );
    match process.run(&command, &CommandCheck::IgnoreStatus) {
        Ok(output) if !output.status().success() => {
            let detail = output.stderr().unwrap_or_default().to_owned();
            Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
                detail,
            )))
        }
        Ok(output) => match output.stdout() {
            Ok(stdout) => match parse_bun_time_json(package, stdout) {
                Ok(timeline) => Ok(ReleaseLookupResult::Known(timeline)),
                Err(BunError::MissingReleaseMetadata(_)) => {
                    Ok(ReleaseLookupResult::MissingMetadata)
                }
                Err(err) => Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
                    err.to_string(),
                ))),
            },
            Err(err) => Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
                err.to_string(),
            ))),
        },
        Err(err) if err.is_interruption() => Err(BunError::from(err)),
        Err(err) => Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
            err.to_string(),
        ))),
    }
}

/// Parses Bun registry `time` JSON metadata.
///
/// # Errors
///
/// Returns an error when JSON or timestamps are invalid, or no version timestamps are present.
pub fn parse_bun_time_json(package: &PackageName, raw: &str) -> Result<ReleaseTimeline, BunError> {
    let timestamps: BTreeMap<String, String> =
        serde_json::from_str(raw).map_err(|err| BunError::Json(err.to_string()))?;
    time_map_to_timeline(package, timestamps)
}

#[must_use]
pub fn bun_global_cwd_from_values(bun_install: Option<&str>, home: Option<&str>) -> Option<String> {
    bun_install
        .and_then(trim_non_empty)
        .map(|path| format!("{path}/install/global"))
        .or_else(|| {
            home.and_then(trim_non_empty)
                .map(|path| format!("{path}/.bun/install/global"))
        })
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
            ExecutionCommandIntent::ResolverNative(_) => {
                return Err(BunError::UnsupportedCommandIntent(
                    "resolver-native".to_owned(),
                ));
            }
            ExecutionCommandIntent::ResolverNativeGlobal(_) => {
                return Err(BunError::UnsupportedCommandIntent(
                    "resolver-native-global".to_owned(),
                ));
            }
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

fn time_map_to_timeline(
    package: &PackageName,
    timestamps: BTreeMap<String, String>,
) -> Result<ReleaseTimeline, BunError> {
    if timestamps.is_empty() {
        return Err(BunError::MissingReleaseMetadata(format!(
            "registry time metadata is empty for {}",
            package.as_str()
        )));
    }

    let mut entries = Vec::new();
    for (version, timestamp) in timestamps {
        if version == "created" || version == "modified" {
            continue;
        }
        let parsed = parse_timestamp(&timestamp).ok_or_else(|| BunError::InvalidTimestamp {
            version: version.clone(),
            value: timestamp.clone(),
        })?;
        entries.push(ReleaseEntry::new(
            VersionText::new(version)?,
            ReleaseTimestamp::new(system_time_from_datetime(parsed)),
        ));
    }
    if entries.is_empty() {
        return Err(BunError::MissingReleaseMetadata(format!(
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

fn bun_global_cwd(env: &Env) -> Option<String> {
    bun_global_cwd_from_values(
        env.var("BUN_INSTALL").as_deref(),
        env.var("HOME").as_deref(),
    )
}

fn adapter_error(err: &BunError) -> ManagerAdapterError {
    let detail = err.to_string();
    let kind = match err {
        &BunError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        &BunError::Json(_)
        | &BunError::Domain(_)
        | &BunError::InvalidTimestamp { .. }
        | &BunError::MissingReleaseMetadata(_) => ManagerAdapterErrorKind::Parse,
        &BunError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        &BunError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail,
    }
}
