use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use semver::Version;
use serde::Deserialize;
use upnow_domain::{
    DomainError, ExecutionSupport, InstalledTool, ManagerConfig, ManagerId, ManagerMetadata,
    ManagerScanInput, ManagerUpdateInput, PackageName, ReleaseEntry, ReleaseLookupError,
    ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp, ToolId, ToolName, UpdateSeed,
    VersionPolicy, VersionScheme, VersionText,
};
use upnow_execution::{
    ExecutionCommand, ExecutionCommandIntent, ExecutionCommandItem, ResolvedExecutionItem,
    ResolvedExecutionPlan,
};
use upnow_infra::{CommandCheck, CommandSpec, Env, HttpClient, InfraError, ProcessRunner};

use crate::adapter::{
    ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind, ManagerCapabilities,
    ManagerConfigDefaults, ReleaseLookupSubject, validate_version_policy,
};

pub const MANAGER_ID: &str = "gem";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GemError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    InvalidTimestamp { version: String, value: String },
    MissingReleaseMetadata(String),
    RubyVersionParse(String),
    UnsupportedCommandIntent(String),
}

impl Display for GemError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Infra(detail)
            | Self::Interrupted(detail)
            | Self::Json(detail)
            | Self::Domain(detail)
            | Self::MissingReleaseMetadata(detail)
            | Self::RubyVersionParse(detail) => formatter.write_str(detail),
            Self::InvalidTimestamp { version, value } => {
                write!(
                    formatter,
                    "invalid RubyGems release timestamp `{value}` for version `{version}`"
                )
            }
            Self::UnsupportedCommandIntent(kind) => {
                write!(
                    formatter,
                    "unsupported gem execution command intent `{kind}`"
                )
            }
        }
    }
}

impl std::error::Error for GemError {}

impl From<InfraError> for GemError {
    fn from(value: InfraError) -> Self {
        if value.is_interruption() {
            Self::Interrupted(value.to_string())
        } else {
            Self::Infra(value.to_string())
        }
    }
}

impl From<DomainError> for GemError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
    }
}

impl GemError {
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GemInstalledPackage {
    pub name: PackageName,
    pub version: VersionText,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GemOutdatedPackage {
    pub name: PackageName,
    pub current: VersionText,
}

#[derive(Debug, Deserialize)]
struct RubyGemsVersionItem {
    number: String,
    created_at: String,
    #[serde(default)]
    ruby_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GemManager {
    config: ManagerConfig,
}

impl GemManager {
    pub const fn new(config: ManagerConfig) -> Self {
        Self { config }
    }

    pub fn id() -> ManagerId {
        ManagerId::from_static(MANAGER_ID)
    }
}
impl ManagerAdapter for GemManager {
    fn default_config() -> ManagerConfigDefaults {
        ManagerConfigDefaults::off_after_days(7)
    }

    fn supports_version_policy(policy: VersionPolicy) -> bool {
        matches!(policy, VersionPolicy::None | VersionPolicy::Stable)
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
        process: &ProcessRunner,
        http: &HttpClient,
        env: &Env,
        subject: ReleaseLookupSubject<'_>,
    ) -> Result<ReleaseLookupResult, ManagerAdapterError> {
        let ruby_runtime = ruby_runtime_version(process).map_err(|err| adapter_error(&err))?;
        Ok(lookup_release(
            http,
            env,
            subject.package_name(),
            Some(&ruby_runtime),
        ))
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
        _env: &Env,
        plan: &ResolvedExecutionPlan,
    ) -> Result<Vec<ExecutionCommand>, ManagerAdapterError> {
        commands_for_execution_plan(plan).map_err(|err| adapter_error(&err))
    }
}

/// Parses `gem list`.
///
/// # Errors
///
/// Returns an error when a parsed gem name or version is blank.
pub fn parse_gem_list(raw: &str) -> Result<Vec<GemInstalledPackage>, GemError> {
    let mut packages = BTreeMap::<String, GemInstalledPackage>::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        let Some((name, rest)) = trimmed.split_once(" (") else {
            continue;
        };
        let Some(inner) = rest.strip_suffix(')') else {
            continue;
        };
        let mut version = None::<String>;
        let mut is_default = false;
        for part in inner.split(',').map(str::trim) {
            if let Some(default_version) = part.strip_prefix("default:") {
                is_default = true;
                let default_version = default_version.trim();
                if !default_version.is_empty() {
                    version = Some(default_version.to_owned());
                }
            } else if version.is_none() && part.chars().next().is_some_and(|ch| ch.is_ascii_digit())
            {
                version = Some(part.to_owned());
            }
        }
        let Some(version) = version else {
            continue;
        };
        let package = GemInstalledPackage {
            name: PackageName::new(name.to_owned())?,
            version: VersionText::new(version)?,
            is_default,
        };
        packages
            .entry(name.to_owned())
            .and_modify(|existing| {
                existing.is_default |= package.is_default;
                if existing.version.as_str().is_empty() {
                    existing.version = package.version.clone();
                }
            })
            .or_insert(package);
    }
    Ok(packages.into_values().collect())
}

/// Parses current versions from `gem outdated`.
///
/// # Errors
///
/// Returns an error when a parsed gem name or version is blank.
pub fn parse_gem_outdated(raw: &str) -> Result<Vec<GemOutdatedPackage>, GemError> {
    let mut packages = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((name, rest)) = trimmed.split_once(" (") else {
            continue;
        };
        let Some(inner) = rest.strip_suffix(')') else {
            continue;
        };
        let Some((current, _latest)) = inner.split_once(" < ") else {
            continue;
        };
        let current = current
            .trim()
            .strip_prefix("default:")
            .map_or_else(|| current.trim(), str::trim);
        if current.is_empty() {
            continue;
        }
        packages.insert(
            name.to_owned(),
            GemOutdatedPackage {
                name: PackageName::new(name.to_owned())?,
                current: VersionText::new(current.to_owned())?,
            },
        );
    }
    Ok(packages.into_values().collect())
}

/// Reads non-default installed gems.
///
/// # Errors
///
/// Returns an error when command output cannot be parsed.
pub fn installed_global(process: &ProcessRunner) -> Result<Vec<InstalledTool>, GemError> {
    let output = process.run(&CommandSpec::new("gem", ["list"]), &CommandCheck::Success)?;
    parse_gem_list(output.stdout()?)?
        .into_iter()
        .filter(|package| !package.is_default)
        .map(installed_tool)
        .collect()
}

/// Builds planning inputs for non-default outdated gems.
///
/// # Errors
///
/// Returns an error when discovery fails or Ruby runtime cannot be parsed.
pub fn update_inputs(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
) -> Result<Vec<ManagerUpdateInput>, GemError> {
    let installed = parse_gem_list(
        process
            .run(&CommandSpec::new("gem", ["list"]), &CommandCheck::Success)?
            .stdout()?,
    )?;
    let installed_defaults = installed
        .into_iter()
        .map(|package| (package.name.as_str().to_owned(), package.is_default))
        .collect::<BTreeMap<_, _>>();
    let outdated = parse_gem_outdated(
        process
            .run(
                &CommandSpec::new("gem", ["outdated"]),
                &CommandCheck::Success,
            )?
            .stdout()?,
    )?;
    let candidates = outdated
        .into_iter()
        .filter(|package| {
            !installed_defaults
                .get(package.name.as_str())
                .copied()
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let ruby_runtime = ruby_runtime_version(process)?;
    let mut inputs = Vec::new();
    for package in candidates {
        let discovered_target = package.current.clone();
        let tool = installed_tool_from_outdated(package)?;
        let lookup = lookup_release(http, env, &tool.package_name, Some(&ruby_runtime));
        inputs.push(update_input(tool, discovered_target, lookup));
    }
    Ok(inputs)
}

/// Looks up `RubyGems` release metadata.
pub fn lookup_release(
    http: &HttpClient,
    env: &Env,
    package: &PackageName,
    ruby_runtime: Option<&Version>,
) -> ReleaseLookupResult {
    let base_url =
        upnow_infra::env_base_url(env, "UPNOW_GEM_RUBYGEMS_BASE_URL", "https://rubygems.org");
    let url = format!("{base_url}/api/v1/versions/{}.json", package.as_str());
    match http.get_text(&url) {
        Ok(response) => match parse_rubygems_json(package, &response.body, ruby_runtime) {
            Ok(timeline) => ReleaseLookupResult::Known(timeline),
            Err(GemError::MissingReleaseMetadata(_) | GemError::InvalidTimestamp { .. }) => {
                ReleaseLookupResult::MissingMetadata
            }
            Err(err) => ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string())),
        },
        Err(err) => ReleaseLookupResult::LookupFailed(ReleaseLookupError::new(err.to_string())),
    }
}

/// Parses `RubyGems` version metadata into a release timeline.
///
/// # Errors
///
/// Returns an error when JSON or timestamps are invalid, or no compatible
/// version timestamps are present.
pub fn parse_rubygems_json(
    package: &PackageName,
    raw: &str,
    ruby_runtime: Option<&Version>,
) -> Result<ReleaseTimeline, GemError> {
    let versions: Vec<RubyGemsVersionItem> =
        serde_json::from_str(raw).map_err(|err| GemError::Json(err.to_string()))?;
    let mut timestamps = BTreeMap::new();
    for item in versions {
        if ruby_runtime
            .is_some_and(|runtime| !ruby_requirement_allows(runtime, item.ruby_version.as_deref()))
        {
            continue;
        }
        if parse_version_for_compare(&item.number).is_none() {
            continue;
        }
        timestamps.insert(item.number, item.created_at);
    }
    time_map_to_timeline(package, timestamps.into_iter().collect())
}

/// Creates gem commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when the resolved execution mode is not supported.
pub fn commands_for_execution_plan(
    plan: &ResolvedExecutionPlan,
) -> Result<Vec<ExecutionCommand>, GemError> {
    let mut commands = Vec::new();
    for intent in &plan.intents {
        match intent {
            ExecutionCommandIntent::Exact(item) => {
                commands.push(ExecutionCommand {
                    items: vec![ExecutionCommandItem::from(item)],
                    command: exact_command_for_item(item),
                });
            }
            ExecutionCommandIntent::NativeSelected(_) => {
                return Err(GemError::UnsupportedCommandIntent(
                    "native-selected".to_owned(),
                ));
            }
            ExecutionCommandIntent::ResolverNative(_) => {
                return Err(GemError::UnsupportedCommandIntent(
                    "resolver-native".to_owned(),
                ));
            }
            ExecutionCommandIntent::ResolverNativeGlobal(_) => {
                return Err(GemError::UnsupportedCommandIntent(
                    "resolver-native-global".to_owned(),
                ));
            }
            ExecutionCommandIntent::GroupedNative(_) => {
                return Err(GemError::UnsupportedCommandIntent(
                    "grouped-native".to_owned(),
                ));
            }
            ExecutionCommandIntent::NativeGlobal(_) => {
                return Err(GemError::UnsupportedCommandIntent(
                    "native-global".to_owned(),
                ));
            }
        }
    }
    Ok(commands)
}

fn ruby_runtime_version(process: &ProcessRunner) -> Result<Version, GemError> {
    let output = process.run(
        &CommandSpec::new("ruby", ["-e", "print RUBY_VERSION"]),
        &CommandCheck::Success,
    )?;
    parse_version_for_compare(output.stdout()?).ok_or_else(|| {
        GemError::RubyVersionParse(format!(
            "failed to parse runtime Ruby version: {}",
            output.stdout_string_lossy()
        ))
    })
}

fn ruby_requirement_allows(runtime: &Version, requirement_raw: Option<&str>) -> bool {
    let Some(raw) = requirement_raw.map(str::trim) else {
        return true;
    };
    if raw.is_empty() {
        return true;
    }
    for token in raw
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let Some(matches) = requirement_token_matches(runtime, token) else {
            return false;
        };
        if !matches {
            return false;
        }
    }
    true
}

fn requirement_token_matches(runtime: &Version, token: &str) -> Option<bool> {
    if let Some(rest) = token.strip_prefix("~>") {
        let lower_raw = rest.trim();
        let lower = parse_version_for_compare(lower_raw)?;
        let upper = pessimistic_upper_bound(lower_raw)?;
        return Some(runtime >= &lower && runtime < &upper);
    }
    for op in [">=", "<=", "==", "!=", ">", "<", "="] {
        if let Some(rest) = token.strip_prefix(op) {
            let rhs = parse_version_for_compare(rest.trim())?;
            return Some(match op {
                ">=" => runtime >= &rhs,
                "<=" => runtime <= &rhs,
                "==" | "=" => runtime == &rhs,
                "!=" => runtime != &rhs,
                ">" => runtime > &rhs,
                "<" => runtime < &rhs,
                _ => false,
            });
        }
    }
    let rhs = parse_version_for_compare(token)?;
    Some(runtime == &rhs)
}

fn pessimistic_upper_bound(raw: &str) -> Option<Version> {
    let segments = raw.trim().split('.').collect::<Vec<_>>();
    if segments.is_empty() || segments.iter().any(|segment| segment.is_empty()) {
        return None;
    }
    let original_len = segments.len();
    let mut nums = segments
        .iter()
        .map(|segment| segment.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    while nums.len() < 3 {
        nums.push(0);
    }
    if original_len <= 2 {
        nums[0] = nums[0].saturating_add(1);
        nums[1] = 0;
    } else {
        nums[1] = nums[1].saturating_add(1);
    }
    nums[2] = 0;
    Version::parse(&format!("{}.{}.{}", nums[0], nums[1], nums[2])).ok()
}

fn parse_version_for_compare(raw: &str) -> Option<Version> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    if let Ok(version) = Version::parse(trimmed) {
        return Some(version);
    }
    let parts = trimmed.split('.').collect::<Vec<_>>();
    if parts.is_empty()
        || parts.len() > 3
        || parts.iter().any(|part| part.is_empty())
        || !parts
            .iter()
            .all(|part| part.chars().all(|ch| ch.is_ascii_digit()))
    {
        return None;
    }
    let mut nums = parts
        .iter()
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    while nums.len() < 3 {
        nums.push(0);
    }
    Version::parse(&format!("{}.{}.{}", nums[0], nums[1], nums[2])).ok()
}

fn exact_command_for_item(item: &ResolvedExecutionItem) -> CommandSpec {
    exact_command_parts(
        &item.package_name,
        item.known_target_version()
            .expect("exact command requires known target"),
    )
}

fn exact_command_parts(package_name: &PackageName, target_version: &VersionText) -> CommandSpec {
    CommandSpec::new(
        "gem",
        [
            "install",
            package_name.as_str(),
            "-v",
            target_version.as_str(),
        ],
    )
    .mutating()
}

fn installed_tool(package: GemInstalledPackage) -> Result<InstalledTool, GemError> {
    Ok(InstalledTool::new(
        GemManager::id(),
        ToolId::new(package.name.as_str().to_owned())?,
        package.name.clone(),
        ToolName::new(package.name.as_str().to_owned())?,
        package.version,
        ManagerMetadata::empty(),
    ))
}

fn installed_tool_from_outdated(package: GemOutdatedPackage) -> Result<InstalledTool, GemError> {
    Ok(InstalledTool::new(
        GemManager::id(),
        ToolId::new(package.name.as_str().to_owned())?,
        package.name.clone(),
        ToolName::new(package.name.as_str().to_owned())?,
        package.current,
        ManagerMetadata::empty(),
    ))
}

const fn update_input(
    tool: InstalledTool,
    discovered_target: VersionText,
    lookup: ReleaseLookupResult,
) -> ManagerUpdateInput {
    ManagerUpdateInput::Seed(UpdateSeed::new(
        tool,
        discovered_target,
        VersionScheme::SemVer,
        lookup,
        ExecutionSupport::exact_only(),
    ))
}

fn time_map_to_timeline(
    package: &PackageName,
    timestamps: Vec<(String, String)>,
) -> Result<ReleaseTimeline, GemError> {
    if timestamps.is_empty() {
        return Err(GemError::MissingReleaseMetadata(format!(
            "registry time metadata is empty for {}",
            package.as_str()
        )));
    }
    let mut entries = Vec::new();
    for (version, timestamp) in timestamps {
        let parsed = parse_timestamp(&timestamp).ok_or_else(|| GemError::InvalidTimestamp {
            version: version.clone(),
            value: timestamp.clone(),
        })?;
        entries.push(ReleaseEntry::new(
            VersionText::new(version)?,
            ReleaseTimestamp::new(system_time_from_datetime(parsed)),
        ));
    }
    if entries.is_empty() {
        return Err(GemError::MissingReleaseMetadata(format!(
            "registry time metadata is empty for {}",
            package.as_str()
        )));
    }
    Ok(ReleaseTimeline::new(entries))
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

fn adapter_error(err: &GemError) -> ManagerAdapterError {
    let kind = match err {
        GemError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        GemError::Json(_)
        | GemError::Domain(_)
        | GemError::InvalidTimestamp { .. }
        | GemError::MissingReleaseMetadata(_)
        | GemError::RubyVersionParse(_) => ManagerAdapterErrorKind::Parse,
        GemError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        GemError::Infra(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail: err.to_string(),
    }
}
