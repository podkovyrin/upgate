use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use semver::Version;
use serde::Deserialize;
use upnow_domain::{
    DomainError, InstalledTool, ManagerId, ManagerMetadata, PackageName, PlanItem, PlanSelection,
    ReleaseEntry, ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp,
    ToolId, ToolName, UpdateCandidate, UpdatePlan, UpdateSeed, VersionPolicy, VersionScheme,
    VersionText,
};
use upnow_infra::{CommandCheck, CommandSpec, InfraError, ProcessRunner};

use crate::adapter::{
    CommandBuildSettings, ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind,
    ManagerCapabilities, ManagerExecutionCommand, ManagerExecutionCommandItem,
};

pub const MANAGER_ID: &str = "bun";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BunError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    HomeUnavailable,
    UnknownPlanItem(String),
    ItemNotExecutable(String),
    ExactTargetUnsupported(String),
    InvalidTimestamp { version: String, value: String },
    EmptyTimeMap { package: String },
}

impl Display for BunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Infra(detail)
            | Self::Interrupted(detail)
            | Self::Json(detail)
            | Self::Domain(detail) => formatter.write_str(detail),
            Self::HomeUnavailable => formatter.write_str("HOME env var is not set"),
            Self::UnknownPlanItem(id) => write!(formatter, "unknown selected plan item `{id}`"),
            Self::ItemNotExecutable(id) => write!(formatter, "plan item `{id}` is not executable"),
            Self::ExactTargetUnsupported(id) => {
                write!(
                    formatter,
                    "plan item `{id}` does not support exact target execution"
                )
            }
            Self::InvalidTimestamp { version, value } => {
                write!(
                    formatter,
                    "invalid timestamp `{value}` for version `{version}`"
                )
            }
            Self::EmptyTimeMap { package } => {
                write!(formatter, "bun pm view time JSON is empty for {package}")
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

type BunTimeMap = BTreeMap<String, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BunManager;

impl ManagerAdapter for BunManager {
    fn id(&self) -> &'static str {
        MANAGER_ID
    }

    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new(true, false).with_native_global_update(true)
    }

    fn supports_version_policy(&self, _policy: VersionPolicy) -> bool {
        true
    }

    fn installed_tools(
        &self,
        process: &ProcessRunner,
    ) -> Result<Vec<InstalledTool>, ManagerAdapterError> {
        installed_global(process).map_err(|err| adapter_error(&err))
    }

    fn release_lookup(
        &self,
        process: &ProcessRunner,
        package: &PackageName,
    ) -> Result<ReleaseLookupResult, ManagerAdapterError> {
        release_lookup(process, package).map_err(|err| adapter_error(&err))
    }

    fn update_seeds(
        &self,
        process: &ProcessRunner,
        version_policy: VersionPolicy,
    ) -> Result<Vec<UpdateSeed>, ManagerAdapterError> {
        self.validate_version_policy(version_policy)?;
        update_seeds(process).map_err(|err| adapter_error(&err))
    }

    fn commands_for_selection(
        &self,
        process: &ProcessRunner,
        plan: &UpdatePlan,
        selection: &PlanSelection,
        settings: CommandBuildSettings,
    ) -> Result<Vec<ManagerExecutionCommand>, ManagerAdapterError> {
        commands_for_selection(process, plan, selection, settings.min_release_age)
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

/// Parses `bun pm view <name> time --json`.
///
/// # Errors
///
/// Returns an error when JSON or timestamps are invalid.
pub fn parse_time_json(package: &PackageName, raw: &str) -> Result<ReleaseTimeline, BunError> {
    let timestamps: BunTimeMap =
        serde_json::from_str(raw).map_err(|err| BunError::Json(err.to_string()))?;
    time_map_to_timeline(package, timestamps)
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

/// Creates update seeds for Bun.
///
/// # Errors
///
/// Returns an error when discovery fails.
pub fn update_seeds(process: &ProcessRunner) -> Result<Vec<UpdateSeed>, BunError> {
    let runtime = BunRuntime::resolve(process);
    let installed = installed_global_with_bun(process, runtime.executable())?;
    let mut seeds = Vec::new();
    for tool in installed {
        let lookup = release_lookup_with_runtime(process, &runtime, &tool.package_name)?;
        let discovered_target = match &lookup {
            ReleaseLookupResult::Known(timeline) => {
                newest_semver(timeline).unwrap_or_else(|| tool.installed_version.clone())
            }
            ReleaseLookupResult::MissingMetadata | ReleaseLookupResult::LookupFailed(_) => {
                tool.installed_version.clone()
            }
        };
        seeds.push(UpdateSeed::new(
            tool,
            discovered_target,
            VersionScheme::SemVer,
            lookup,
        ));
    }
    Ok(seeds)
}

/// Creates exact Bun commands for a typed selection.
///
/// # Errors
///
/// Returns an error when the selected item is unknown or not exact-executable.
pub fn commands_for_selection(
    process: &ProcessRunner,
    plan: &UpdatePlan,
    selection: &PlanSelection,
    min_release_age: Duration,
) -> Result<Vec<ManagerExecutionCommand>, BunError> {
    let runtime = BunRuntime::resolve(process);
    let selected = selected_candidates(plan, selection)?;
    if should_use_native_global_update(plan, &selected) {
        return Ok(vec![ManagerExecutionCommand {
            items: selected
                .into_iter()
                .map(|(plan_item_id, candidate, _forced)| execution_item(plan_item_id, candidate))
                .collect(),
            command: global_update_command(runtime.executable(), min_release_age),
        }]);
    }

    let mut commands = Vec::new();
    for (plan_item_id, candidate, forced) in selected {
        if !candidate.execution_eligibility.supports_exact_target() {
            return Err(BunError::ExactTargetUnsupported(
                plan_item_id.as_str().to_owned(),
            ));
        }
        commands.push(ManagerExecutionCommand {
            items: vec![execution_item(plan_item_id, candidate)],
            command: exact_command_with_program(
                runtime.executable(),
                candidate,
                min_release_age,
                forced,
            ),
        });
    }
    Ok(commands)
}

fn selected_candidates<'a>(
    plan: &'a UpdatePlan,
    selection: &'a PlanSelection,
) -> Result<Vec<(upnow_domain::PlanItemId, &'a UpdateCandidate, bool)>, BunError> {
    let mut candidates = Vec::new();
    for selected in &selection.selected_items {
        let item = plan
            .item(&selected.plan_item_id)
            .ok_or_else(|| BunError::UnknownPlanItem(selected.plan_item_id.as_str().to_owned()))?;
        let candidate = executable_candidate(item, selected.forced)?;
        candidates.push((selected.plan_item_id.clone(), candidate, selected.forced));
    }
    Ok(candidates)
}

fn should_use_native_global_update(
    plan: &UpdatePlan,
    selected: &[(upnow_domain::PlanItemId, &UpdateCandidate, bool)],
) -> bool {
    if selected.is_empty() || selected.iter().any(|(_, _, forced)| *forced) {
        return false;
    }
    let update_ids = plan
        .items
        .iter()
        .filter_map(|item| match item {
            PlanItem::Update { id, .. } => Some(id),
            _ => None,
        })
        .collect::<Vec<_>>();
    update_ids.len() == selected.len()
        && update_ids.iter().all(|id| {
            selected
                .iter()
                .any(|(selected_id, _, _)| selected_id == *id)
        })
}

fn execution_item(
    plan_item_id: upnow_domain::PlanItemId,
    candidate: &UpdateCandidate,
) -> ManagerExecutionCommandItem {
    ManagerExecutionCommandItem {
        plan_item_id,
        package_name: candidate.package_name.clone(),
        installed_version: candidate.installed_version.clone(),
        target_version: candidate.target_version.clone(),
    }
}

#[must_use]
pub fn exact_command(
    candidate: &UpdateCandidate,
    min_release_age: Duration,
    bypass_min_release_age: bool,
) -> CommandSpec {
    exact_command_with_program("bun", candidate, min_release_age, bypass_min_release_age)
}

#[must_use]
pub fn exact_command_with_program(
    bun: &str,
    candidate: &UpdateCandidate,
    min_release_age: Duration,
    bypass_min_release_age: bool,
) -> CommandSpec {
    let spec = format!(
        "{}@{}",
        candidate.package_name.as_str(),
        candidate.target_version.as_str()
    );
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

fn bun_global_cwd() -> Result<String, BunError> {
    bun_global_cwd_from_values(
        std::env::var("BUN_INSTALL").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
    .ok_or(BunError::HomeUnavailable)
}

fn trim_non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn time_map_to_timeline(
    package: &PackageName,
    timestamps: BTreeMap<String, String>,
) -> Result<ReleaseTimeline, BunError> {
    if timestamps.is_empty() {
        return Err(BunError::EmptyTimeMap {
            package: package.as_str().to_owned(),
        });
    }
    let mut entries = Vec::new();
    for (version, timestamp) in timestamps {
        if version == "created" || version == "modified" {
            continue;
        }
        let parsed =
            DateTime::parse_from_rfc3339(&timestamp).map_err(|_| BunError::InvalidTimestamp {
                version: version.clone(),
                value: timestamp.clone(),
            })?;
        entries.push(ReleaseEntry::new(
            VersionText::new(version)?,
            ReleaseTimestamp::new(system_time_from_datetime(parsed)),
        ));
    }
    if entries.is_empty() {
        return Err(BunError::EmptyTimeMap {
            package: package.as_str().to_owned(),
        });
    }
    Ok(ReleaseTimeline::new(entries))
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

fn release_lookup(
    process: &ProcessRunner,
    package: &PackageName,
) -> Result<ReleaseLookupResult, BunError> {
    let runtime = BunRuntime::resolve(process);
    release_lookup_with_runtime(process, &runtime, package)
}

fn release_lookup_with_runtime(
    process: &ProcessRunner,
    runtime: &BunRuntime,
    package: &PackageName,
) -> Result<ReleaseLookupResult, BunError> {
    let cwd = match bun_global_cwd() {
        Ok(cwd) => cwd,
        Err(BunError::HomeUnavailable) => return Ok(ReleaseLookupResult::MissingMetadata),
        Err(err) => return Err(err),
    };
    match process.run(
        &CommandSpec::new(
            runtime.executable(),
            [
                "pm",
                "view",
                package.as_str(),
                "time",
                "--json",
                "--cwd",
                &cwd,
            ],
        ),
        &CommandCheck::IgnoreStatus,
    ) {
        Ok(output) if !output.status().success() => {
            let detail = output.stderr().unwrap_or_default().to_owned();
            Ok(ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(
                detail,
            )))
        }
        Ok(output) => match output.stdout() {
            Ok(stdout) => match parse_time_json(package, stdout) {
                Ok(timeline) => Ok(ReleaseLookupResult::Known(timeline)),
                Err(BunError::EmptyTimeMap { .. }) => Ok(ReleaseLookupResult::MissingMetadata),
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

fn newest_semver(timeline: &ReleaseTimeline) -> Option<VersionText> {
    timeline
        .versions
        .iter()
        .filter_map(|entry| {
            Version::parse(entry.version.as_str())
                .ok()
                .map(|version| (version, entry.version.clone()))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, version)| version)
}

fn executable_candidate(item: &PlanItem, forced: bool) -> Result<&UpdateCandidate, BunError> {
    match item {
        PlanItem::Update { candidate, .. } => Ok(candidate),
        PlanItem::Delayed { candidate, .. } if forced => Ok(candidate),
        _ => Err(BunError::ItemNotExecutable(item.id().as_str().to_owned())),
    }
}

fn adapter_error(err: &BunError) -> ManagerAdapterError {
    let detail = err.to_string();
    let kind = match err {
        &BunError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        &BunError::Json(_)
        | &BunError::Domain(_)
        | &BunError::InvalidTimestamp { .. }
        | &BunError::EmptyTimeMap { .. } => ManagerAdapterErrorKind::Parse,
        &BunError::UnknownPlanItem(_)
        | &BunError::ItemNotExecutable(_)
        | &BunError::ExactTargetUnsupported(_) => ManagerAdapterErrorKind::CommandConstruction,
        &BunError::Infra(_) | &BunError::HomeUnavailable => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail,
    }
}

fn system_time_from_datetime(datetime: DateTime<chrono::FixedOffset>) -> SystemTime {
    let timestamp = datetime.timestamp();
    if timestamp >= 0 {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(timestamp.unsigned_abs())
    } else {
        SystemTime::UNIX_EPOCH - std::time::Duration::from_secs(timestamp.unsigned_abs())
    }
}
