use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use serde::Deserialize;
use upnow_domain::{
    DomainError, ExecutionSupport, InstalledTool, ManagerConfig, ManagerId, ManagerMetadata,
    ManagerScanInput, ManagerUpdateInput, PackageName, ReleaseEntry, ReleaseLookupError,
    ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp, ScanItem, ToolId, ToolName, UpdateSeed,
    VersionScheme, VersionText,
};
use upnow_execution::{
    ExecutionCommand, ExecutionCommandIntent, ExecutionCommandItem, ResolvedExecutionPlan,
};
use upnow_infra::{CommandCheck, CommandSpec, Env, HttpClient, InfraError, ProcessRunner};
use upnow_release::{newest_semver_version, release_age_for_version};

use crate::adapter::{
    ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind, ManagerCapabilities,
    ReleaseLookupSubject, validate_version_policy,
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

    fn scan_items_with_release_evidence(
        &self,
        process: &ProcessRunner,
        _http: &HttpClient,
        env: &Env,
        now: SystemTime,
        _max_parallel_checks: usize,
    ) -> Result<Vec<ScanItem>, ManagerAdapterError> {
        let runtime = BunRuntime::resolve(process);
        let installed = installed_global_with_bun(process, runtime.executable())
            .map_err(|err| adapter_error(&err))?;
        installed
            .into_iter()
            .map(|tool| {
                match lookup_release_with_bun(
                    process,
                    env,
                    runtime.executable(),
                    &tool.package_name,
                )
                .map_err(|err| adapter_error(&err))?
                {
                    ReleaseLookupResult::Known(timeline) => {
                        match release_age_for_version(&timeline, &tool.installed_version, now) {
                            Some(age) => Ok(ScanItem::InstalledWithReleaseAge { tool, age }),
                            None => Ok(ScanItem::Installed(tool)),
                        }
                    }
                    ReleaseLookupResult::MissingMetadata | ReleaseLookupResult::LookupFailed(_) => {
                        Ok(ScanItem::Installed(tool))
                    }
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
        let runtime = BunRuntime::resolve(process);
        lookup_release_with_bun(process, env, runtime.executable(), subject.package_name())
            .map_err(|err| adapter_error(&err))
    }

    fn update_inputs(
        &self,
        process: &ProcessRunner,
        _http: &HttpClient,
        env: &Env,
    ) -> Result<Vec<ManagerUpdateInput>, ManagerAdapterError> {
        validate_version_policy(
            &self.config.manager_id,
            Self::supports_version_policy(self.config.version_policy),
            self.config.version_policy,
        )?;
        update_inputs(process, env).map_err(|err| adapter_error(&err))
    }

    fn commands_for_execution_plan(
        &self,
        process: &ProcessRunner,
        _env: &Env,
        plan: &ResolvedExecutionPlan,
    ) -> Result<Vec<ExecutionCommand>, ManagerAdapterError> {
        commands_for_execution_plan(process, plan, self.config.min_release_age)
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
pub fn parse_bun_time_json(package: &PackageName, raw: &str) -> Result<ReleaseTimeline, BunError> {
    let timestamps: BTreeMap<String, String> =
        serde_json::from_str(raw).map_err(|err| BunError::Json(err.to_string()))?;
    time_map_to_timeline(package, timestamps)
}
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
) -> Result<Vec<ExecutionCommand>, BunError> {
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
            ExecutionCommandIntent::GroupedNative(_) => {
                return Err(BunError::UnsupportedCommandIntent(
                    "grouped-native".to_owned(),
                ));
            }
            ExecutionCommandIntent::NativeGlobal(items) => {
                commands.push(ExecutionCommand {
                    items: items.iter().map(ExecutionCommandItem::from).collect(),
                    command: global_update_command(runtime.executable(), min_release_age),
                });
            }
            ExecutionCommandIntent::Exact(item) => {
                commands.push(ExecutionCommand {
                    items: vec![ExecutionCommandItem::from(item)],
                    command: exact_command_with_program(
                        runtime.executable(),
                        &item.package_name,
                        item.known_target_version()
                            .expect("exact command requires known target"),
                        min_release_age,
                        item.bypass_min_release_age,
                    ),
                });
            }
            ExecutionCommandIntent::NativeSelected(item) => {
                commands.push(ExecutionCommand {
                    items: vec![ExecutionCommandItem::from(item)],
                    command: selected_native_update_command(
                        runtime.executable(),
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
fn exact_command_with_program(
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
fn global_update_command(bun: &str, min_release_age: Duration) -> CommandSpec {
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

fn selected_native_update_command(
    bun: &str,
    package_name: &PackageName,
    min_release_age: Duration,
    bypass_min_release_age: bool,
) -> CommandSpec {
    let min_age_secs = min_release_age.as_secs().to_string();
    let mut args = vec![
        "update".to_owned(),
        "-g".to_owned(),
        package_name.as_str().to_owned(),
    ];
    if !bypass_min_release_age {
        args.push("--minimum-release-age".to_owned());
        args.push(min_age_secs);
    }
    CommandSpec::new(bun, args).mutating()
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

fn bun_executable(process: &ProcessRunner) -> String {
    if let Ok(path) = std::env::var("UPNOW_BUN_BIN")
        && let Some(trimmed) = trim_non_empty(&path)
    {
        return trimmed.to_owned();
    }
    process
        .run(
            &CommandSpec::new("mise", ["which", "bun"]),
            &CommandCheck::Success,
        )
        .map_or_else(
            |_| MANAGER_ID.to_owned(),
            |output| {
                output
                    .stdout()
                    .ok()
                    .and_then(trim_non_empty)
                    .map_or_else(|| MANAGER_ID.to_owned(), ToOwned::to_owned)
            },
        )
}

fn trim_non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn installed_tool(package: BunInstalledPackage) -> Result<InstalledTool, BunError> {
    Ok(InstalledTool::new(
        BunManager::id(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use upnow_domain::{ExecutionTargetKind, PlanItemId};
    use upnow_execution::{ResolvedExecutionItem, ResolvedExecutionTarget};
    use upnow_infra::CommandOutput;

    #[test]
    fn update_input_declares_exact_and_native_global_support() {
        let input = update_input(
            installed_tool_for_test("typescript"),
            ReleaseLookupResult::MissingMetadata,
        );
        let ManagerUpdateInput::Seed(seed) = input else {
            panic!("update input should be a seed");
        };

        assert_eq!(
            seed.execution_support,
            ExecutionSupport::exact_or_native_global()
        );
    }

    #[test]
    fn forced_exact_update_omits_min_release_age() {
        let process = bun_runtime_process();
        let plan = ResolvedExecutionPlan {
            intents: vec![ExecutionCommandIntent::Exact(resolved_item(
                "typescript",
                ResolvedExecutionTarget::Known(VersionText::new("5.5.0").expect("valid version")),
                true,
            ))],
        };

        let commands =
            commands_for_execution_plan(&process, &plan, Duration::from_secs(7 * 24 * 60 * 60))
                .expect("exact command should be supported");

        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].command.display(),
            "/opt/bin/bun update -g typescript@5.5.0"
        );
    }

    #[test]
    fn exact_and_global_command_shapes_are_unchanged() {
        let process = bun_runtime_process();
        let plan = ResolvedExecutionPlan {
            intents: vec![
                ExecutionCommandIntent::Exact(resolved_item(
                    "typescript",
                    ResolvedExecutionTarget::Known(
                        VersionText::new("5.5.0").expect("valid version"),
                    ),
                    false,
                )),
                ExecutionCommandIntent::NativeGlobal(vec![resolved_item(
                    "eslint",
                    ResolvedExecutionTarget::Known(
                        VersionText::new("9.0.0").expect("valid version"),
                    ),
                    false,
                )]),
            ],
        };

        let commands =
            commands_for_execution_plan(&process, &plan, Duration::from_secs(7 * 24 * 60 * 60))
                .expect("exact and global commands should still be supported");

        assert_eq!(commands.len(), 2);
        assert_eq!(
            commands[0].command.display(),
            "/opt/bin/bun update -g typescript@5.5.0 --minimum-release-age 604800"
        );
        assert_eq!(
            commands[1].command.display(),
            "/opt/bin/bun update -g --minimum-release-age 604800"
        );
    }

    #[test]
    fn release_lookup_parses_valid_stdout_from_nonzero_status() {
        let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(
            failure_status(),
            r#"{"1.0.0":"2024-01-01T00:00:00.000Z"}"#,
            "bun reported a lookup warning",
        ))]);
        let env = Env::fixed([("HOME".to_owned(), "/tmp/home".to_owned())]);
        let package = PackageName::new("typescript").expect("valid package");

        let lookup = lookup_release_with_bun(&process, &env, "/opt/bin/bun", &package)
            .expect("non-signal command failure should not abort lookup");

        let ReleaseLookupResult::Known(timeline) = lookup else {
            panic!("valid JSON stdout should produce a release timeline");
        };
        assert_eq!(timeline.versions.len(), 1);
        assert_eq!(timeline.versions[0].version.as_str(), "1.0.0");
    }

    fn resolved_item(
        package: &str,
        target: ResolvedExecutionTarget,
        bypass_min_release_age: bool,
    ) -> ResolvedExecutionItem {
        ResolvedExecutionItem {
            plan_item_id: PlanItemId::new(format!("bun:{package}")).expect("valid plan item id"),
            package_name: PackageName::new(package).expect("valid package"),
            installed_version: VersionText::new("1.0.0").expect("valid version"),
            target,
            execution_support: ExecutionSupport::exact_or_native_global(),
            execution_target_kind: ExecutionTargetKind::Standard,
            exact_target_required: false,
            bypass_min_release_age,
        }
    }

    fn installed_tool_for_test(package: &str) -> InstalledTool {
        InstalledTool::new(
            BunManager::id(),
            ToolId::new(format!("bun:{package}")).expect("valid tool id"),
            PackageName::new(package).expect("valid package"),
            ToolName::new(package).expect("valid tool name"),
            VersionText::new("1.0.0").expect("valid version"),
            ManagerMetadata::empty(),
        )
    }

    fn bun_runtime_process() -> ProcessRunner {
        ProcessRunner::fake([Ok(CommandOutput::from_parts(
            success_status(),
            "/opt/bin/bun\n",
            "",
        ))])
    }

    #[cfg(unix)]
    fn success_status() -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(0)
    }

    #[cfg(windows)]
    fn success_status() -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(0)
    }

    #[cfg(unix)]
    fn failure_status() -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1 << 8)
    }

    #[cfg(windows)]
    fn failure_status() -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1)
    }
}
