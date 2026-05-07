use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

use chrono::DateTime;
use serde::Deserialize;
use upnow_domain::{
    DomainError, ExecutionEligibility, ExecutionTargetKind, InstalledTool, ManagerId,
    ManagerMetadata, ManagerMetadataField, ManagerMetadataKey, ManagerMetadataValue,
    ManagerScanInput, ManagerSelectedTarget, ManagerUpdateInput, PackageName, ReleaseEntry,
    ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp, SkipReason,
    TargetAgeEvidence, TargetAgeLookupResult, ToolId, ToolName, UpdateSeed, VersionPolicy,
    VersionScheme, VersionText,
};
use upnow_execution::{ExecutionCommandIntent, ResolvedExecutionItem, ResolvedExecutionPlan};
use upnow_infra::{CommandCheck, CommandSpec, Env, HttpClient, InfraError, ProcessRunner};

use crate::adapter::{
    CommandBuildSettings, ManagerAdapter, ManagerAdapterError, ManagerAdapterErrorKind,
    ManagerCapabilities, ManagerDefaultMode, ManagerDefaults, ManagerExecutionCommand,
    ManagerExecutionCommandItem, ReleaseLookupSubject,
};

pub const MANAGER_ID: &str = "brew";

const META_TAP: &str = "brew_tap";
const META_SOURCE_PATH: &str = "brew_source_path";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrewError {
    Infra(String),
    Interrupted(String),
    Json(String),
    Domain(String),
    MissingReleaseMetadata(String),
    ReleaseLookup(String),
    UnsupportedCommandIntent(String),
}

impl Display for BrewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Infra(detail)
            | Self::Interrupted(detail)
            | Self::Json(detail)
            | Self::Domain(detail)
            | Self::MissingReleaseMetadata(detail)
            | Self::ReleaseLookup(detail) => formatter.write_str(detail),
            Self::UnsupportedCommandIntent(kind) => {
                write!(
                    formatter,
                    "unsupported brew execution command intent `{kind}`"
                )
            }
        }
    }
}

impl std::error::Error for BrewError {}

impl From<InfraError> for BrewError {
    fn from(value: InfraError) -> Self {
        if value.is_interruption() {
            Self::Interrupted(value.to_string())
        } else {
            Self::Infra(value.to_string())
        }
    }
}

impl From<DomainError> for BrewError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value.to_string())
    }
}

impl BrewError {
    #[must_use]
    pub const fn is_interruption(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrewPackageKind {
    Formula,
    Cask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrewInstalledPackage {
    pub name: PackageName,
    pub version: VersionText,
    pub kind: BrewPackageKind,
    pub tap: Option<String>,
    pub source_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrewOutdatedPackage {
    pub name: PackageName,
    pub installed: VersionText,
    pub target: VersionText,
    pub kind: BrewPackageKind,
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrewPackageMetadata {
    tap: Option<String>,
    source_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TapMetadata {
    path: String,
    remote: Option<String>,
    branch: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OutdatedRoot {
    #[serde(default)]
    formulae: Vec<OutdatedFormula>,
    #[serde(default)]
    casks: Vec<OutdatedCask>,
}

#[derive(Debug, Deserialize)]
struct OutdatedFormula {
    name: String,
    #[serde(default)]
    installed_versions: Vec<String>,
    current_version: String,
    #[serde(default)]
    pinned: bool,
}

#[derive(Debug, Deserialize)]
struct OutdatedCask {
    name: String,
    #[serde(default)]
    installed_versions: Vec<String>,
    current_version: String,
}

#[derive(Debug, Deserialize)]
struct InfoRoot {
    #[serde(default)]
    formulae: Vec<FormulaInfo>,
    #[serde(default)]
    casks: Vec<CaskInfo>,
}

#[derive(Debug, Deserialize)]
struct FormulaInfo {
    full_name: String,
    tap: Option<String>,
    ruby_source_path: Option<String>,
    #[serde(default)]
    installed: Vec<FormulaInstalledInfo>,
}

#[derive(Debug, Deserialize)]
struct FormulaInstalledInfo {
    version: String,
    #[serde(default)]
    installed_on_request: bool,
    #[serde(default)]
    installed_as_dependency: bool,
}

#[derive(Debug, Deserialize)]
struct CaskInfo {
    token: String,
    tap: Option<String>,
    ruby_source_path: Option<String>,
    installed: Option<CaskInstalledVersions>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CaskInstalledVersions {
    Single(String),
    Multiple(Vec<String>),
}

impl CaskInstalledVersions {
    fn latest(&self) -> Option<&str> {
        match self {
            Self::Single(version) => Some(version.as_str()),
            Self::Multiple(versions) => versions.last().map(String::as_str),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TapInfo {
    name: String,
    path: String,
    remote: Option<String>,
    branch: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubCommitItem {
    commit: GitHubCommit,
}

#[derive(Debug, Deserialize)]
struct GitHubCommit {
    author: Option<GitHubCommitPerson>,
    committer: Option<GitHubCommitPerson>,
}

#[derive(Debug, Deserialize)]
struct GitHubCommitPerson {
    date: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrewManager;

impl ManagerAdapter for BrewManager {
    fn id(&self) -> &'static str {
        MANAGER_ID
    }

    fn defaults(&self) -> ManagerDefaults {
        ManagerDefaults {
            min_release_age: Duration::from_secs(12 * 60 * 60),
            mode: ManagerDefaultMode::Apply,
        }
    }

    fn capabilities(&self) -> ManagerCapabilities {
        ManagerCapabilities::new().with_native_global_update(true)
    }

    fn supports_version_policy(&self, _policy: VersionPolicy) -> bool {
        true
    }

    fn scan_inputs(
        &self,
        process: &ProcessRunner,
        _env: &Env,
    ) -> Result<Vec<ManagerScanInput>, ManagerAdapterError> {
        installed_packages(process)
            .map(|packages| {
                packages
                    .into_iter()
                    .map(|package| installed_tool(&package).map(ManagerScanInput::Installed))
                    .collect::<Result<Vec<_>, _>>()
            })
            .and_then(|items| items)
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
            ReleaseLookupSubject::Package(_) => ReleaseLookupResult::MissingMetadata,
            ReleaseLookupSubject::Installed(tool) => {
                release_lookup_for_installed(process, http, env, tool)
            }
        })
    }

    fn update_inputs(
        &self,
        process: &ProcessRunner,
        http: &HttpClient,
        env: &Env,
        version_policy: VersionPolicy,
        _min_release_age: Duration,
        no_update: bool,
    ) -> Result<Vec<ManagerUpdateInput>, ManagerAdapterError> {
        self.validate_version_policy(version_policy)?;
        update_inputs(process, http, env, no_update).map_err(|err| adapter_error(&err))
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

/// Parses `brew outdated --json=v2`.
///
/// # Errors
///
/// Returns an error when JSON is malformed or a package/version is blank.
pub fn parse_outdated_json(raw: &str) -> Result<Vec<BrewOutdatedPackage>, BrewError> {
    let parsed: OutdatedRoot =
        serde_json::from_str(raw).map_err(|err| BrewError::Json(err.to_string()))?;
    let mut packages = Vec::new();
    for formula in parsed.formulae {
        packages.push(BrewOutdatedPackage {
            name: PackageName::new(formula.name)?,
            installed: VersionText::new(first_version(formula.installed_versions))?,
            target: VersionText::new(formula.current_version)?,
            kind: BrewPackageKind::Formula,
            pinned: formula.pinned,
        });
    }
    for cask in parsed.casks {
        packages.push(BrewOutdatedPackage {
            name: PackageName::new(cask.name)?,
            installed: VersionText::new(first_version(cask.installed_versions))?,
            target: VersionText::new(cask.current_version)?,
            kind: BrewPackageKind::Cask,
            pinned: false,
        });
    }
    Ok(packages)
}

/// Parses `brew info --json=v2 --installed`.
///
/// # Errors
///
/// Returns an error when JSON is malformed or package fields are blank.
pub fn parse_installed_info_json(raw: &str) -> Result<Vec<BrewInstalledPackage>, BrewError> {
    let parsed: InfoRoot =
        serde_json::from_str(raw).map_err(|err| BrewError::Json(err.to_string()))?;
    let mut packages = Vec::new();
    for formula in parsed.formulae {
        let explicitly_installed = formula.installed.is_empty()
            || formula
                .installed
                .iter()
                .any(|item| item.installed_on_request || !item.installed_as_dependency);
        if !explicitly_installed {
            continue;
        }
        let version = formula
            .installed
            .last()
            .map_or_else(|| "unknown".to_owned(), |item| item.version.clone());
        packages.push(BrewInstalledPackage {
            name: PackageName::new(formula.full_name)?,
            version: VersionText::new(version)?,
            kind: BrewPackageKind::Formula,
            tap: formula.tap,
            source_path: formula.ruby_source_path,
        });
    }
    for cask in parsed.casks {
        let version = cask
            .installed
            .as_ref()
            .and_then(CaskInstalledVersions::latest)
            .map_or_else(|| "unknown".to_owned(), ToOwned::to_owned);
        packages.push(BrewInstalledPackage {
            name: PackageName::new(cask.token)?,
            version: VersionText::new(version)?,
            kind: BrewPackageKind::Cask,
            tap: cask.tap,
            source_path: cask.ruby_source_path,
        });
    }
    Ok(packages)
}

/// Parses `brew info --json=v2 <names>`.
///
/// # Errors
///
/// Returns an error when JSON is malformed or package fields are blank.
fn parse_package_info_json(
    raw: &str,
) -> Result<BTreeMap<PackageName, BrewPackageMetadata>, BrewError> {
    let parsed: InfoRoot =
        serde_json::from_str(raw).map_err(|err| BrewError::Json(err.to_string()))?;
    let mut packages = BTreeMap::new();
    for formula in parsed.formulae {
        packages.insert(
            PackageName::new(formula.full_name)?,
            BrewPackageMetadata {
                tap: formula.tap,
                source_path: formula.ruby_source_path,
            },
        );
    }
    for cask in parsed.casks {
        packages.insert(
            PackageName::new(cask.token)?,
            BrewPackageMetadata {
                tap: cask.tap,
                source_path: cask.ruby_source_path,
            },
        );
    }
    Ok(packages)
}

/// Builds Brew planning inputs using Homebrew-selected outdated targets.
///
/// # Errors
///
/// Returns an error when Brew discovery fails.
pub fn update_inputs(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    no_update: bool,
) -> Result<Vec<ManagerUpdateInput>, BrewError> {
    if !no_update {
        let _ = process.run(
            &CommandSpec::new("brew", ["update", "--quiet"]).mutating(),
            &CommandCheck::Success,
        );
    }

    let outdated = outdated_packages(process)?;
    if outdated.is_empty() {
        return Ok(Vec::new());
    }
    let (package_info, package_info_error) = match package_info_for_outdated(process, &outdated) {
        Ok(package_info) => (package_info, None),
        Err(err) if err.is_interruption() => return Err(err),
        Err(err) => (BTreeMap::new(), Some(err.to_string())),
    };
    let tap_metadata = if package_info_error.is_some() {
        BTreeMap::new()
    } else {
        tap_metadata(process).unwrap_or_default()
    };

    let mut inputs = Vec::new();
    for package in outdated {
        let metadata = package_info.get(&package.name);
        let installed = installed_tool_for_outdated(&package, metadata)?;
        if package.pinned {
            inputs.push(ManagerUpdateInput::Skipped {
                installed,
                reason: SkipReason::Pinned,
            });
            continue;
        }
        let target_age = package_info_error.as_ref().map_or_else(
            || lookup_target_age(process, http, env, metadata, &tap_metadata),
            |detail| {
                TargetAgeLookupResult::LookupFailed(ReleaseLookupError::new(format!(
                    "failed to read brew package metadata: {detail}"
                )))
            },
        );
        let selected = ManagerSelectedTarget::new(package.target.clone(), target_age);
        inputs.push(ManagerUpdateInput::Seed(
            UpdateSeed::manager_selected(
                installed,
                selected,
                VersionScheme::ManagerNative,
                ExecutionEligibility::NativeOnly,
            )
            .with_execution_target_kind(execution_target_kind(package.kind)),
        ));
    }
    Ok(inputs)
}

/// Creates Brew commands for a resolved execution plan.
///
/// # Errors
///
/// Returns an error when the resolved execution mode is unsupported by Brew.
pub fn commands_for_execution_plan(
    plan: &ResolvedExecutionPlan,
) -> Result<Vec<ManagerExecutionCommand>, BrewError> {
    let mut formulae = Vec::new();
    let mut casks = Vec::new();
    for intent in &plan.intents {
        match intent {
            ExecutionCommandIntent::NativeSelected(item) => {
                push_brew_item(item, &mut formulae, &mut casks)?;
            }
            ExecutionCommandIntent::NativeGlobal(items) => {
                for item in items {
                    push_brew_item(item, &mut formulae, &mut casks)?;
                }
            }
            ExecutionCommandIntent::Exact(_) => {
                return Err(BrewError::UnsupportedCommandIntent("exact".to_owned()));
            }
            ExecutionCommandIntent::ResolverNative(_) => {
                return Err(BrewError::UnsupportedCommandIntent(
                    "resolver-native".to_owned(),
                ));
            }
            ExecutionCommandIntent::ResolverNativeGlobal(_) => {
                return Err(BrewError::UnsupportedCommandIntent(
                    "resolver-native-global".to_owned(),
                ));
            }
        }
    }

    let mut commands = Vec::new();
    if !formulae.is_empty() {
        commands.push(grouped_upgrade_command("--formula", &formulae));
    }
    if !casks.is_empty() {
        commands.push(grouped_upgrade_command("--cask", &casks));
    }
    Ok(commands)
}

fn outdated_packages(process: &ProcessRunner) -> Result<Vec<BrewOutdatedPackage>, BrewError> {
    let output = process.run(
        &CommandSpec::new("brew", ["outdated", "--json=v2"]),
        &CommandCheck::Success,
    )?;
    parse_outdated_json(output.stdout()?)
}

fn installed_packages(process: &ProcessRunner) -> Result<Vec<BrewInstalledPackage>, BrewError> {
    let output = process.run(
        &CommandSpec::new("brew", ["info", "--json=v2", "--installed"]),
        &CommandCheck::Success,
    )?;
    parse_installed_info_json(output.stdout()?)
}

fn package_info_for_outdated(
    process: &ProcessRunner,
    packages: &[BrewOutdatedPackage],
) -> Result<BTreeMap<PackageName, BrewPackageMetadata>, BrewError> {
    let mut args = vec!["info".to_owned(), "--json=v2".to_owned()];
    args.extend(
        packages
            .iter()
            .map(|package| package.name.as_str().to_owned()),
    );
    let output = process.run(&CommandSpec::new("brew", args), &CommandCheck::Success)?;
    parse_package_info_json(output.stdout()?)
}

fn tap_metadata(process: &ProcessRunner) -> Result<BTreeMap<String, TapMetadata>, BrewError> {
    let output = process.run(
        &CommandSpec::new("brew", ["tap-info", "--json", "--installed"]),
        &CommandCheck::Success,
    )?;
    let taps: Vec<TapInfo> =
        serde_json::from_str(output.stdout()?).map_err(|err| BrewError::Json(err.to_string()))?;
    Ok(taps
        .into_iter()
        .map(|tap| {
            (
                tap.name,
                TapMetadata {
                    path: tap.path,
                    remote: tap.remote,
                    branch: tap.branch,
                },
            )
        })
        .collect())
}

fn lookup_target_age(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    metadata: Option<&BrewPackageMetadata>,
    taps: &BTreeMap<String, TapMetadata>,
) -> TargetAgeLookupResult {
    let Some(metadata) = metadata else {
        return TargetAgeLookupResult::MissingMetadata;
    };
    let Some(tap) = metadata.tap.as_deref() else {
        return TargetAgeLookupResult::MissingMetadata;
    };
    let Some(source_path) = metadata.source_path.as_deref() else {
        return TargetAgeLookupResult::MissingMetadata;
    };

    if let Some(tap_metadata) = taps.get(tap) {
        match git_last_commit_timestamp(process, tap_metadata, source_path) {
            Ok(timestamp) => return known_target_age(timestamp),
            Err(local_err) => {
                if let Some((remote, branch)) = fallback_remote_branch(tap, Some(tap_metadata)) {
                    return github_target_age(http, env, &remote, branch.as_deref(), source_path)
                        .unwrap_or_else(|remote_err| {
                            TargetAgeLookupResult::LookupFailed(ReleaseLookupError::new(format!(
                                "local git failed ({local_err}); GitHub lookup failed ({remote_err})"
                            )))
                        });
                }
                return TargetAgeLookupResult::LookupFailed(ReleaseLookupError::new(format!(
                    "local git failed ({local_err}) and no remote fallback is available"
                )));
            }
        }
    }

    if let Some((remote, branch)) = fallback_remote_branch(tap, None) {
        return github_target_age(http, env, &remote, branch.as_deref(), source_path)
            .unwrap_or_else(|err| {
                TargetAgeLookupResult::LookupFailed(ReleaseLookupError::new(err))
            });
    }

    TargetAgeLookupResult::MissingMetadata
}

fn release_lookup_for_installed(
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    tool: &InstalledTool,
) -> ReleaseLookupResult {
    let metadata = BrewPackageMetadata {
        tap: metadata_text(&tool.metadata, META_TAP),
        source_path: metadata_text(&tool.metadata, META_SOURCE_PATH),
    };
    let taps = tap_metadata(process).unwrap_or_default();
    match lookup_target_age(process, http, env, Some(&metadata), &taps) {
        TargetAgeLookupResult::Known(evidence) => {
            ReleaseLookupResult::Known(ReleaseTimeline::new(vec![ReleaseEntry::new(
                tool.installed_version.clone(),
                evidence.timestamp().clone(),
            )]))
        }
        TargetAgeLookupResult::MissingMetadata => ReleaseLookupResult::MissingMetadata,
        TargetAgeLookupResult::LookupFailed(err) => ReleaseLookupResult::LookupFailed(err),
    }
}

fn git_last_commit_timestamp(
    process: &ProcessRunner,
    tap: &TapMetadata,
    source_path: &str,
) -> Result<ReleaseTimestamp, BrewError> {
    let mut last_err = String::new();
    for git_ref in git_refs_for_tap(tap) {
        match git_log_timestamp_for_ref(process, tap, source_path, &git_ref) {
            Ok(timestamp) => return Ok(timestamp),
            Err(err) => last_err = format!("{git_ref}: {err}"),
        }
    }

    Err(BrewError::ReleaseLookup(format!(
        "git log failed for all refs ({last_err})"
    )))
}

fn git_refs_for_tap(tap: &TapMetadata) -> Vec<String> {
    let mut refs = Vec::new();
    if let Some(branch) = tap.branch.as_ref().filter(|branch| !branch.is_empty()) {
        refs.push(format!("origin/{branch}"));
    }
    for fallback in ["origin/HEAD", "FETCH_HEAD", "HEAD"] {
        if !refs.iter().any(|existing| existing == fallback) {
            refs.push(fallback.to_owned());
        }
    }
    refs
}

fn git_log_timestamp_for_ref(
    process: &ProcessRunner,
    tap: &TapMetadata,
    source_path: &str,
    git_ref: &str,
) -> Result<ReleaseTimestamp, BrewError> {
    let output = process.run(
        &CommandSpec::new(
            "git",
            [
                "-C",
                tap.path.as_str(),
                "log",
                "-1",
                "--format=%ct",
                git_ref,
                "--",
                source_path,
            ],
        ),
        &CommandCheck::Success,
    )?;
    let seconds = output.stdout()?.trim().parse::<u64>().map_err(|err| {
        BrewError::ReleaseLookup(format!("invalid git timestamp for {source_path}: {err}"))
    })?;
    Ok(timestamp_from_unix_seconds(seconds))
}

fn github_target_age(
    http: &HttpClient,
    env: &Env,
    remote: &str,
    branch: Option<&str>,
    source_path: &str,
) -> Result<TargetAgeLookupResult, String> {
    let (owner, repo) = parse_github_remote(remote)
        .ok_or_else(|| format!("unsupported non-GitHub remote `{remote}`"))?;
    let base_url = upnow_infra::env_base_url(
        env,
        "UPNOW_BREW_GITHUB_API_BASE_URL",
        "https://api.github.com",
    );
    let mut url = reqwest::Url::parse(&format!("{base_url}/repos/{owner}/{repo}/commits"))
        .map_err(|err| err.to_string())?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("path", source_path);
        if let Some(branch) = branch.filter(|branch| !branch.is_empty()) {
            query.append_pair("sha", branch);
        }
        query.append_pair("per_page", "1");
    }
    let response = http.get_text(url.as_str()).map_err(|err| err.to_string())?;
    let commits: Vec<GitHubCommitItem> =
        serde_json::from_str(&response.body).map_err(|err| err.to_string())?;
    let first = commits
        .first()
        .ok_or_else(|| "GitHub API returned no commits for this file".to_owned())?;
    let date = first
        .commit
        .committer
        .as_ref()
        .map(|person| person.date.as_str())
        .or_else(|| {
            first
                .commit
                .author
                .as_ref()
                .map(|person| person.date.as_str())
        })
        .ok_or_else(|| "GitHub commit payload missing date".to_owned())?;
    let parsed = DateTime::parse_from_rfc3339(date).map_err(|err| err.to_string())?;
    let seconds = u64::try_from(parsed.timestamp())
        .map_err(|_| "GitHub commit timestamp is negative".to_owned())?;
    Ok(known_target_age(timestamp_from_unix_seconds(seconds)))
}

fn parse_github_remote(remote: &str) -> Option<(String, String)> {
    let rest = if let Some(rest) = remote.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = remote.strip_prefix("http://github.com/") {
        rest
    } else if let Some(rest) = remote.strip_prefix("git@github.com:") {
        rest
    } else if let Some(rest) = remote.strip_prefix("ssh://git@github.com/") {
        rest
    } else {
        return None;
    };
    let cleaned = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = cleaned.split('/');
    Some((parts.next()?.to_owned(), parts.next()?.to_owned()))
}

fn fallback_remote_branch(
    tap: &str,
    tap_metadata: Option<&TapMetadata>,
) -> Option<(String, Option<String>)> {
    if let Some(metadata) = tap_metadata
        && let Some(remote) = metadata.remote.clone()
    {
        return Some((remote, metadata.branch.clone()));
    }
    match tap {
        "homebrew/core" => Some((
            "https://github.com/Homebrew/homebrew-core".to_owned(),
            Some("main".to_owned()),
        )),
        "homebrew/cask" => Some((
            "https://github.com/Homebrew/homebrew-cask".to_owned(),
            Some("main".to_owned()),
        )),
        _ => None,
    }
}

fn known_target_age(timestamp: ReleaseTimestamp) -> TargetAgeLookupResult {
    TargetAgeLookupResult::Known(TargetAgeEvidence::ManagerNativeTimestamp(timestamp))
}

fn timestamp_from_unix_seconds(seconds: u64) -> ReleaseTimestamp {
    ReleaseTimestamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
}

fn first_version(versions: Vec<String>) -> String {
    versions
        .into_iter()
        .next()
        .unwrap_or_else(|| "unknown".to_owned())
}

fn installed_tool(package: &BrewInstalledPackage) -> Result<InstalledTool, BrewError> {
    Ok(InstalledTool::new(
        manager_id(),
        ToolId::new(package.name.as_str().to_owned())?,
        package.name.clone(),
        ToolName::new(package.name.as_str().to_owned())?,
        package.version.clone(),
        metadata(package.tap.clone(), package.source_path.clone())?,
    ))
}

fn installed_tool_for_outdated(
    package: &BrewOutdatedPackage,
    metadata: Option<&BrewPackageMetadata>,
) -> Result<InstalledTool, BrewError> {
    Ok(InstalledTool::new(
        manager_id(),
        ToolId::new(package.name.as_str().to_owned())?,
        package.name.clone(),
        ToolName::new(package.name.as_str().to_owned())?,
        package.installed.clone(),
        metadata
            .map(metadata_fields)
            .transpose()?
            .unwrap_or_else(ManagerMetadata::empty),
    ))
}

fn execution_target_kind(kind: BrewPackageKind) -> ExecutionTargetKind {
    match kind {
        BrewPackageKind::Formula => ExecutionTargetKind::BrewFormula,
        BrewPackageKind::Cask => ExecutionTargetKind::BrewCask,
    }
}

fn metadata(
    tap: Option<String>,
    source_path: Option<String>,
) -> Result<ManagerMetadata, BrewError> {
    metadata_fields(&BrewPackageMetadata { tap, source_path })
}

fn metadata_fields(metadata: &BrewPackageMetadata) -> Result<ManagerMetadata, BrewError> {
    let mut fields = Vec::new();
    if let Some(tap) = &metadata.tap {
        fields.push(ManagerMetadataField::new(
            ManagerMetadataKey::new(META_TAP)?,
            ManagerMetadataValue::Text(tap.clone()),
        ));
    }
    if let Some(source_path) = &metadata.source_path {
        fields.push(ManagerMetadataField::new(
            ManagerMetadataKey::new(META_SOURCE_PATH)?,
            ManagerMetadataValue::Text(source_path.clone()),
        ));
    }
    Ok(ManagerMetadata::new(fields))
}

fn metadata_text(metadata: &ManagerMetadata, key: &str) -> Option<String> {
    metadata.fields.iter().find_map(|field| {
        if field.key.as_str() == key
            && let ManagerMetadataValue::Text(value) = &field.value
        {
            return Some(value.clone());
        }
        None
    })
}

fn push_brew_item(
    item: &ResolvedExecutionItem,
    formulae: &mut Vec<ResolvedExecutionItem>,
    casks: &mut Vec<ResolvedExecutionItem>,
) -> Result<(), BrewError> {
    match item.execution_target_kind {
        ExecutionTargetKind::BrewFormula => {
            formulae.push(item.clone());
            Ok(())
        }
        ExecutionTargetKind::BrewCask => {
            casks.push(item.clone());
            Ok(())
        }
        ExecutionTargetKind::Standard => Err(BrewError::UnsupportedCommandIntent(
            "standard-target-kind".to_owned(),
        )),
    }
}

fn grouped_upgrade_command(
    kind_arg: &str,
    items: &[ResolvedExecutionItem],
) -> ManagerExecutionCommand {
    let mut args = vec!["upgrade".to_owned(), kind_arg.to_owned()];
    args.extend(
        items
            .iter()
            .map(|item| item.package_name.as_str().to_owned()),
    );
    ManagerExecutionCommand {
        items: items.iter().map(execution_item).collect(),
        command: CommandSpec::new("brew", args).mutating(),
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

fn manager_id() -> ManagerId {
    ManagerId::new(MANAGER_ID).expect("static brew manager id should be valid")
}

fn adapter_error(err: &BrewError) -> ManagerAdapterError {
    let detail = err.to_string();
    let kind = match err {
        BrewError::Interrupted(_) => ManagerAdapterErrorKind::Interrupted,
        BrewError::Json(_) | BrewError::Domain(_) => ManagerAdapterErrorKind::Parse,
        BrewError::UnsupportedCommandIntent(_) => ManagerAdapterErrorKind::CommandConstruction,
        BrewError::Infra(_)
        | BrewError::MissingReleaseMetadata(_)
        | BrewError::ReleaseLookup(_) => ManagerAdapterErrorKind::Infra,
    };
    ManagerAdapterError::Manager {
        manager_id: MANAGER_ID.to_owned(),
        kind,
        detail,
    }
}
