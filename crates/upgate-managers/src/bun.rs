use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use serde::Deserialize;
use upgate_domain::{
    AuditPackageName, AuditSubject, DomainError, ExecutionSupport, InstalledTool, ManagerConfig,
    ManagerId, ManagerScanEvidenceInput, ManagerScanInput, ManagerUpdateInput, OsvEcosystem,
    PackageName, ReleaseEntry, ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline,
    ReleaseTimestamp, ToolId, ToolName, UpdateSeed, VersionScheme, VersionText,
};
use upgate_execution::{
    ExecutionCommand, ExecutionCommandIntent, ExecutionCommandItem, ResolvedExecutionPlan,
};
use upgate_infra::{
    CommandCheck, CommandSpec, Env, HttpClient, InfraError, ProcessRunner, effective_parallelism,
    run_ordered_parallel,
};
use upgate_release::{newest_semver_version, release_evidence_for_version};

use crate::adapter::{
    ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind, ManagerCapabilities,
    ReleaseLookupSubject, validate_version_policy,
};

const MANAGER_ID: &str = "bun";
const BUN_MAX_PARALLEL_CHECKS: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
enum BunError {
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct BunInstalledPackage {
    name: PackageName,
    version: VersionText,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BunManager {
    config: ManagerConfig,
}

impl BunManager {
    pub const fn new(config: ManagerConfig) -> Self {
        Self { config }
    }

    pub fn id() -> ManagerId {
        ManagerId::from_static(MANAGER_ID)
    }
}
impl ManagerAdapter for BunManager {
    fn required_executable() -> &'static str {
        MANAGER_ID
    }

    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new().with_native_global_update(true)
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

    fn scan_inputs_with_release_evidence(
        &self,
        process: &ProcessRunner,
        _http: &HttpClient,
        env: &Env,
        _max_parallel_checks_per_manager: usize,
    ) -> Result<Vec<ManagerScanEvidenceInput>, ManagerAdapterError> {
        let installed = installed_global(process).map_err(|err| adapter_error(&err))?;
        installed
            .into_iter()
            .map(|tool| {
                let release_evidence = match lookup_release(process, env, &tool.package_name)
                    .map_err(|err| adapter_error(&err))?
                {
                    ReleaseLookupResult::Known(timeline) => {
                        release_evidence_for_version(&timeline, &tool.installed_version)
                    }
                    ReleaseLookupResult::MissingMetadata | ReleaseLookupResult::LookupFailed(_) => {
                        None
                    }
                };
                Ok(ManagerScanEvidenceInput::Installed {
                    tool,
                    release_evidence,
                })
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
        lookup_release(process, env, subject.package_name()).map_err(|err| adapter_error(&err))
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
        _: &ProcessRunner,
        _env: &Env,
        plan: &ResolvedExecutionPlan,
    ) -> Result<Vec<ExecutionCommand>, ManagerAdapterError> {
        commands_for_execution_plan(plan, self.config.min_release_age)
            .map_err(|err| adapter_error(&err))
    }
}

/// Parses `bun pm ls -g --json`.
///
/// # Errors
///
/// Returns an error when JSON is malformed or a package/version is blank.
fn parse_pm_ls_json(raw: &str) -> Result<Vec<BunInstalledPackage>, BunError> {
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
fn reports_empty_global_install(text: &str) -> bool {
    text.lines().any(|line| {
        matches!(
            line.trim(),
            "error: missing lockfile, nothing to list"
                | "error: missing lockfile, nothing outdated"
                | "error: missing package.json"
        )
    })
}

/// Reads installed Bun global packages.
///
/// # Errors
///
/// Returns an error when the command fails unexpectedly or output cannot be parsed.
fn installed_global(process: &ProcessRunner) -> Result<Vec<InstalledTool>, BunError> {
    let output = process.run(
        &CommandSpec::new(MANAGER_ID, ["pm", "ls", "-g", "--json"]),
        &CommandCheck::IgnoreStatus,
    )?;
    let stdout = output.stdout()?;
    let stderr = output.stderr().unwrap_or_default();
    if reports_empty_global_install(stdout) || reports_empty_global_install(stderr) {
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
fn update_inputs(
    process: &ProcessRunner,
    env: &Env,
    max_parallel_checks_per_manager: usize,
) -> Result<Vec<ManagerUpdateInput>, BunError> {
    let tools = installed_global(process)?;
    let threads = effective_parallelism(max_parallel_checks_per_manager, BUN_MAX_PARALLEL_CHECKS);
    run_ordered_parallel(tools, threads, MANAGER_ID, |tool| {
        let lookup = lookup_release(process, env, &tool.package_name)?;
        Ok(update_input(tool, lookup))
    })?
    .into_iter()
    .collect()
}

/// Looks up Bun registry release metadata.
///
/// # Errors
///
/// Returns an error only when command execution is interrupted.
fn lookup_release(
    process: &ProcessRunner,
    env: &Env,
    package: &PackageName,
) -> Result<ReleaseLookupResult, BunError> {
    let Some(cwd) = bun_global_cwd(env) else {
        return Ok(ReleaseLookupResult::MissingMetadata);
    };
    let command = CommandSpec::new(
        MANAGER_ID,
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
        Ok(output) => {
            if !output.status().success() && output.status().code().is_none() {
                return Err(BunError::Interrupted(
                    "bun pm view time --json failed (exit signal)".to_owned(),
                ));
            }
            match output.stdout() {
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
            }
        }
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
fn parse_bun_time_json(package: &PackageName, raw: &str) -> Result<ReleaseTimeline, BunError> {
    let timestamps: BTreeMap<String, String> =
        serde_json::from_str(raw).map_err(|err| BunError::Json(err.to_string()))?;
    time_map_to_timeline(package, timestamps)
}
/// Creates Bun commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when the resolved execution mode is not supported by Bun.
fn commands_for_execution_plan(
    plan: &ResolvedExecutionPlan,
    min_release_age: Duration,
) -> Result<Vec<ExecutionCommand>, BunError> {
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
                commands.push(ExecutionCommand {
                    items: items.iter().map(ExecutionCommandItem::from).collect(),
                    command: global_update_command(min_release_age),
                });
            }
            ExecutionCommandIntent::Exact(item) => {
                let target_version = item.known_target_version().ok_or_else(|| {
                    BunError::UnsupportedCommandIntent("exact-without-known-target".to_owned())
                })?;
                commands.push(ExecutionCommand {
                    items: vec![ExecutionCommandItem::from(item)],
                    command: exact_command(
                        &item.package_name,
                        target_version,
                        min_release_age,
                        item.bypass_min_release_age,
                    ),
                });
            }
            ExecutionCommandIntent::NativeSelected(item) => {
                commands.push(ExecutionCommand {
                    items: vec![ExecutionCommandItem::from(item)],
                    command: selected_native_update_command(
                        &item.package_name,
                        min_release_age,
                        item.bypass_min_release_age,
                    ),
                });
            }
        }
    }
    Ok(commands)
}
fn exact_command(
    package_name: &PackageName,
    target_version: &VersionText,
    min_release_age: Duration,
    bypass_min_release_age: bool,
) -> CommandSpec {
    let spec = format!("{package_name}@{target_version}");
    let mut args = vec!["update".to_owned(), "-g".to_owned(), spec];
    if !bypass_min_release_age {
        args.push("--minimum-release-age".to_owned());
        args.push(min_release_age.as_secs().to_string());
    }
    CommandSpec::new(MANAGER_ID, args).mutating()
}
fn global_update_command(min_release_age: Duration) -> CommandSpec {
    let min_age_secs = min_release_age.as_secs().to_string();
    CommandSpec::new(
        MANAGER_ID,
        [
            "update",
            "-g",
            "--minimum-release-age",
            min_age_secs.as_str(),
        ],
    )
    .mutating()
}

fn selected_native_update_command(
    package_name: &PackageName,
    min_release_age: Duration,
    bypass_min_release_age: bool,
) -> CommandSpec {
    let mut args = vec![
        "update".to_owned(),
        "-g".to_owned(),
        package_name.as_str().to_owned(),
    ];
    if !bypass_min_release_age {
        args.push("--minimum-release-age".to_owned());
        args.push(min_release_age.as_secs().to_string());
    }
    CommandSpec::new(MANAGER_ID, args).mutating()
}

fn installed_tool(package: BunInstalledPackage) -> Result<InstalledTool, BunError> {
    Ok(InstalledTool::new(
        BunManager::id(),
        ToolId::new(package.name.as_str().to_owned())?,
        package.name.clone(),
        ToolName::new(package.name.as_str().to_owned())?,
        package.version,
    )
    .with_audit_subject(AuditSubject::new(
        OsvEcosystem::Npm,
        AuditPackageName::new(package.name.as_str().to_owned())?,
    )))
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
        ExecutionSupport::exact_or_native_global(),
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
    env.non_empty_var("BUN_INSTALL")
        .map(|path| format!("{path}/install/global"))
        .or_else(|| {
            env.non_empty_var("HOME")
                .map(|path| format!("{path}/.bun/install/global"))
        })
}

fn adapter_error(err: &BunError) -> ManagerAdapterError {
    let detail = err.to_string();
    let kind = match err {
        BunError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        BunError::Json(_)
        | BunError::Domain(_)
        | BunError::InvalidTimestamp { .. }
        | BunError::MissingReleaseMetadata(_) => ManagerAdapterErrorKind::Parse,
        BunError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        BunError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager { kind, detail }
}
