use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{emit_manager_level_error, emit_scan_current};
use crate::outcome::{
    ItemOutcome, REASON_COMMAND_FAILED, REASON_MISSING_METADATA, REASON_PINNED, emit_text_outcome,
};
use crate::ui::output_theme;
use crate::util::process::RunCmd;
use crate::util::time::now_unix_secs;
use crate::util::timefmt::human_age;
use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

const BREW_MAX_PARALLEL_CHECKS_MIN: usize = 1;
const BREW_API_FALLBACK_MAX_PARALLEL_CHECKS: usize = 4;

pub(crate) struct BrewPlugin;

impl ManagerPlugin for BrewPlugin {
    fn id(&self) -> &'static str {
        "brew"
    }

    fn default_min_release_age(&self) -> &'static str {
        "12h"
    }

    fn supports_no_update(&self) -> bool {
        true
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub(crate) static PLUGIN: BrewPlugin = BrewPlugin;

#[derive(Debug, Deserialize)]
struct OutdatedRoot {
    formulae: Vec<OutdatedFormula>,
    casks: Vec<OutdatedCask>,
}

#[derive(Debug, Deserialize)]
struct OutdatedFormula {
    name: String,
    installed_versions: Vec<String>,
    current_version: String,
    pinned: bool,
    pinned_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OutdatedCask {
    name: String,
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
    #[serde(default)]
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
            Self::Single(v) => Some(v.as_str()),
            Self::Multiple(v) => v.last().map(String::as_str),
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

#[derive(Debug, Clone)]
struct TapMeta {
    path: String,
    remote: Option<String>,
    branch: Option<String>,
}

#[derive(Clone, Copy)]
enum DataSource {
    Git,
    Api,
    None,
}

impl DataSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Api => "api",
            Self::None => "n/a",
        }
    }
}

enum PlanAction {
    Upgrade {
        source: DataSource,
    },
    Delayed {
        age: String,
        required: String,
        source: DataSource,
    },
    Skipped {
        reason: String,
        source: DataSource,
    },
}

struct PlanItem {
    name: String,
    installed: String,
    target: String,
    action: PlanAction,
    is_formula: bool,
}

struct PackageJob {
    name: String,
    installed: String,
    target: String,
    is_formula: bool,
    initial_skip_reason: Option<String>,
    tap_and_source: Option<(Option<String>, Option<String>)>,
}

struct ScanItem {
    name: String,
    version: String,
    tap_and_source: Option<(Option<String>, Option<String>)>,
}

#[derive(Clone)]
struct ApiJob {
    index: usize,
    name: String,
    installed: String,
    target: String,
    is_formula: bool,
    remote: String,
    branch: Option<String>,
    source_path: String,
    local_err: String,
}

enum PhaseOneResult {
    Final(PlanItem),
    NeedsApi(ApiJob),
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

fn run(ctx: &ManagerCtx) -> Result<()> {
    if ctx.is_scan() {
        return scan(ctx);
    }

    let min_age = ctx.policy.min_release_age.duration();

    maybe_refresh_brew_metadata(ctx.policy.no_update);

    let outdated: OutdatedRoot = match RunCmd::Success.json("brew", &["outdated", "--json=v2"]) {
        Ok(outdated) => outdated,
        Err(err) => {
            emit_manager_error(format!("failed to read brew outdated state: {err}"));
            return Ok(());
        }
    };

    if outdated.formulae.is_empty() && outdated.casks.is_empty() {
        return Ok(());
    }

    let jobs = build_brew_plan_jobs(outdated);

    let tap_meta = match brew_tap_meta() {
        Ok(tap_meta) => tap_meta,
        Err(err) => {
            emit_manager_error(format!("failed to read brew tap metadata: {err}"));
            HashMap::new()
        }
    };

    let now = now_unix_secs()?;

    let github_client = match github_client() {
        Ok(client) => Some(client),
        Err(err) => {
            emit_manager_error(format!("failed to initialize remote lookup client: {err}"));
            None
        }
    };

    let plan = resolve_brew_plan(
        jobs,
        &tap_meta,
        github_client.as_ref(),
        min_age,
        now,
        ctx.max_parallel_checks,
    )?;

    for item in &plan {
        let outcome = item_to_outcome(item);
        emit_text_outcome(&outcome);
    }

    if ctx.is_dry_run() {
        return Ok(());
    }

    apply_brew_plan(&plan);
    Ok(())
}

fn maybe_refresh_brew_metadata(no_update: bool) {
    if !no_update && let Err(err) = RunCmd::Success.run("brew", ["update", "--quiet"]) {
        emit_manager_error(format!("brew metadata refresh failed: {err}"));
    }
}

fn build_brew_plan_jobs(outdated: OutdatedRoot) -> Vec<PackageJob> {
    let formula_names: Vec<String> = outdated.formulae.iter().map(|f| f.name.clone()).collect();
    let cask_names: Vec<String> = outdated.casks.iter().map(|c| c.name.clone()).collect();

    let info = match brew_info_for_names(&formula_names, &cask_names) {
        Ok(info) => info,
        Err(err) => {
            emit_manager_error(format!("failed to read brew package metadata: {err}"));
            InfoRoot {
                formulae: Vec::new(),
                casks: Vec::new(),
            }
        }
    };

    let mut formula_info_by_name: HashMap<String, FormulaInfo> = HashMap::new();
    for formula in info.formulae {
        formula_info_by_name.insert(formula.full_name.clone(), formula);
    }

    let mut cask_info_by_name: HashMap<String, CaskInfo> = HashMap::new();
    for cask in info.casks {
        cask_info_by_name.insert(cask.token.clone(), cask);
    }

    let mut jobs = Vec::with_capacity(outdated.formulae.len() + outdated.casks.len());

    for item in outdated.formulae {
        let installed = item
            .installed_versions
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let initial_skip_reason = if item.pinned {
            Some(match item.pinned_version {
                Some(v) => format!("pinned at {v}"),
                None => "pinned".to_string(),
            })
        } else {
            None
        };

        let tap_and_source = formula_info_by_name
            .get(&item.name)
            .map(|f| (f.tap.clone(), f.ruby_source_path.clone()));

        jobs.push(PackageJob {
            name: item.name,
            installed,
            target: item.current_version,
            is_formula: true,
            initial_skip_reason,
            tap_and_source,
        });
    }

    for item in outdated.casks {
        let installed = item
            .installed_versions
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());

        let tap_and_source = cask_info_by_name
            .get(&item.name)
            .map(|c| (c.tap.clone(), c.ruby_source_path.clone()));

        jobs.push(PackageJob {
            name: item.name,
            installed,
            target: item.current_version,
            is_formula: false,
            initial_skip_reason: None,
            tap_and_source,
        });
    }

    jobs
}

fn resolve_brew_plan(
    jobs: Vec<PackageJob>,
    tap_meta: &HashMap<String, TapMeta>,
    github_client: Option<&Client>,
    min_age: Duration,
    now_unix_secs: u64,
    max_parallel_checks: usize,
) -> Result<Vec<PlanItem>> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(max_parallel_checks.max(BREW_MAX_PARALLEL_CHECKS_MIN))
        .build()
        .context("failed to build rayon thread pool")?;

    let phase_one_results: Vec<PhaseOneResult> = pool.install(|| {
        jobs.into_par_iter()
            .enumerate()
            .map(|(index, job)| phase_one_local_check(index, job, min_age, now_unix_secs, tap_meta))
            .collect()
    });

    let mut plan_slots: Vec<Option<PlanItem>> =
        (0..phase_one_results.len()).map(|_| None).collect();
    let mut api_jobs = Vec::new();

    for (index, result) in phase_one_results.into_iter().enumerate() {
        match result {
            PhaseOneResult::Final(item) => plan_slots[index] = Some(item),
            PhaseOneResult::NeedsApi(job) => api_jobs.push(job),
        }
    }

    if !api_jobs.is_empty() {
        let api_parallelism = max_parallel_checks.clamp(
            BREW_MAX_PARALLEL_CHECKS_MIN,
            BREW_API_FALLBACK_MAX_PARALLEL_CHECKS,
        );
        let api_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(api_parallelism)
            .build()
            .context("failed to build API fallback thread pool")?;

        let api_results: Vec<(usize, PlanItem)> = api_pool.install(|| {
            api_jobs
                .into_par_iter()
                .map(|job| {
                    let action = if let Some(client) = github_client {
                        match github_last_commit_unix_seconds(
                            client,
                            &job.remote,
                            job.branch.as_deref(),
                            &job.source_path,
                        ) {
                            Ok(ts) => {
                                action_from_commit_age(min_age, now_unix_secs, ts, DataSource::Api)
                            }
                            Err(remote_err) => PlanAction::Skipped {
                                reason: format!(
                                    "failed age check: local git failed ({}); remote lookup failed ({})",
                                    job.local_err, remote_err
                                ),
                                source: DataSource::Api,
                            },
                        }
                    } else {
                        PlanAction::Skipped {
                            reason: format!(
                                "failed age check: local git failed ({}) and remote lookup is unavailable",
                                job.local_err
                            ),
                            source: DataSource::None,
                        }
                    };

                    (
                        job.index,
                        PlanItem {
                            name: job.name,
                            installed: job.installed,
                            target: job.target,
                            action,
                            is_formula: job.is_formula,
                        },
                    )
                })
                .collect()
        });

        for (index, item) in api_results {
            plan_slots[index] = Some(item);
        }
    }

    plan_slots
        .into_iter()
        .map(|item| item.context("internal error: missing plan slot"))
        .collect()
}

fn apply_brew_plan(plan: &[PlanItem]) {
    let formula_to_upgrade: Vec<String> = plan
        .iter()
        .filter(|i| i.is_formula)
        .filter_map(|i| match i.action {
            PlanAction::Upgrade { .. } => Some(i.name.clone()),
            PlanAction::Delayed { .. } | PlanAction::Skipped { .. } => None,
        })
        .collect();

    let casks_to_upgrade: Vec<String> = plan
        .iter()
        .filter(|i| !i.is_formula)
        .filter_map(|i| match i.action {
            PlanAction::Upgrade { .. } => Some(i.name.clone()),
            PlanAction::Delayed { .. } | PlanAction::Skipped { .. } => None,
        })
        .collect();

    if !formula_to_upgrade.is_empty() {
        let mut args = vec!["upgrade".to_string(), "--formula".to_string()];
        args.extend(formula_to_upgrade);
        if let Err(err) = RunCmd::Success.run("brew", &args) {
            emit_manager_error(format!("failed to apply brew formula upgrades: {err}"));
        }
    }

    if !casks_to_upgrade.is_empty() {
        let mut args = vec!["upgrade".to_string(), "--cask".to_string()];
        args.extend(casks_to_upgrade);
        if let Err(err) = RunCmd::Success.run("brew", &args) {
            emit_manager_error(format!("failed to apply brew cask upgrades: {err}"));
        }
    }
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let scan_items = match collect_brew_scan_items() {
        Ok(items) => items,
        Err(err) => {
            emit_manager_error(err.to_string());
            return Ok(());
        }
    };

    if !output_theme().verbose {
        emit_brew_scan_items(scan_items, None, ctx.scan_old_age_threshold);
        return Ok(());
    }

    let Some(age_slots) = resolve_brew_scan_age_slots(&scan_items, ctx.max_parallel_checks)? else {
        emit_brew_scan_items(scan_items, None, ctx.scan_old_age_threshold);
        return Ok(());
    };

    emit_brew_scan_items(scan_items, Some(&age_slots), ctx.scan_old_age_threshold);
    Ok(())
}

fn collect_brew_scan_items() -> Result<Vec<ScanItem>> {
    let info = brew_info_installed().with_context(|| "failed to query installed brew packages")?;

    let mut scan_items = Vec::with_capacity(info.formulae.len() + info.casks.len());

    for formula in info.formulae {
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
            .map(|item| item.version.clone())
            .unwrap_or_else(|| "unknown".to_string());

        scan_items.push(ScanItem {
            name: formula.full_name,
            version,
            tap_and_source: Some((formula.tap, formula.ruby_source_path)),
        });
    }

    for cask in info.casks {
        let version = cask
            .installed
            .as_ref()
            .and_then(CaskInstalledVersions::latest)
            .map(ToString::to_string)
            .unwrap_or_else(|| "unknown".to_string());

        scan_items.push(ScanItem {
            name: cask.token,
            version,
            tap_and_source: Some((cask.tap, cask.ruby_source_path)),
        });
    }

    Ok(scan_items)
}

fn resolve_brew_scan_age_slots(
    scan_items: &[ScanItem],
    max_parallel_checks: usize,
) -> Result<Option<Vec<Option<u64>>>> {
    let tap_meta = brew_tap_meta().unwrap_or_default();
    let github_client = github_client().ok();
    let now = now_unix_secs()?;

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(max_parallel_checks.max(BREW_MAX_PARALLEL_CHECKS_MIN))
        .build()
        .context("failed to build brew scan thread pool")?;

    let tap_meta_ref = &tap_meta;
    let github_client_ref = github_client.as_ref();

    let age_indexed: Vec<(usize, Option<u64>)> = pool.install(|| {
        scan_items
            .par_iter()
            .enumerate()
            .map(|(idx, item)| {
                let tap_and_source = item
                    .tap_and_source
                    .as_ref()
                    .map(|(tap, source_path)| (tap.as_deref(), source_path.as_deref()));

                let age = brew_scan_age_secs(tap_and_source, tap_meta_ref, github_client_ref, now);
                (idx, age)
            })
            .collect()
    });

    let mut age_slots: Vec<Option<u64>> = (0..scan_items.len()).map(|_| None).collect();
    for (idx, age) in age_indexed {
        age_slots[idx] = age;
    }

    Ok(Some(age_slots))
}

fn emit_brew_scan_items(
    scan_items: Vec<ScanItem>,
    age_slots: Option<&[Option<u64>]>,
    old_threshold: Duration,
) {
    for (idx, item) in scan_items.into_iter().enumerate() {
        let age = age_slots.and_then(|slots| slots.get(idx).copied().flatten());
        emit_scan_current(
            PLUGIN.id(),
            PLUGIN.id(),
            item.name,
            item.version,
            age,
            old_threshold,
        );
    }
}

fn brew_scan_age_secs(
    tap_and_source: Option<(Option<&str>, Option<&str>)>,
    tap_meta: &HashMap<String, TapMeta>,
    github_client: Option<&Client>,
    now_unix_secs: u64,
) -> Option<u64> {
    let (tap, source_path) = tap_and_source?;
    let tap = tap?;
    let source_path = source_path?;

    let (remote, branch, local_ts) = if let Some(meta) = tap_meta.get(tap) {
        let local_ts =
            git_last_commit_unix_seconds(&meta.path, meta.branch.as_deref(), source_path).ok();
        (
            meta.remote
                .clone()
                .or_else(|| resolve_api_fallback_remote_branch(tap, Some(meta)).map(|(r, _)| r)),
            meta.branch.clone().or_else(|| {
                resolve_api_fallback_remote_branch(tap, Some(meta)).and_then(|(_, b)| b)
            }),
            local_ts,
        )
    } else {
        let (remote, branch) = resolve_api_fallback_remote_branch(tap, None)?;
        (Some(remote), branch, None)
    };

    let commit_ts = local_ts.or_else(|| {
        let remote = remote.as_deref()?;
        let client = github_client?;
        github_last_commit_unix_seconds(client, remote, branch.as_deref(), source_path).ok()
    })?;

    Some(now_unix_secs.saturating_sub(commit_ts))
}

#[allow(clippy::too_many_lines)]
fn phase_one_local_check(
    index: usize,
    job: PackageJob,
    min_age: Duration,
    now_unix_secs: u64,
    tap_meta: &HashMap<String, TapMeta>,
) -> PhaseOneResult {
    if let Some(reason) = job.initial_skip_reason {
        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped {
                reason,
                source: DataSource::None,
            },
            is_formula: job.is_formula,
        });
    }

    let Some((tap, source_path)) = job.tap_and_source.as_ref() else {
        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped {
                reason: "unable to resolve package metadata from brew info".to_string(),
                source: DataSource::None,
            },
            is_formula: job.is_formula,
        });
    };

    let Some(tap) = tap.as_deref() else {
        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped {
                reason: "missing tap".to_string(),
                source: DataSource::None,
            },
            is_formula: job.is_formula,
        });
    };

    let Some(source_path) = source_path.as_deref() else {
        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped {
                reason: "missing ruby_source_path".to_string(),
                source: DataSource::None,
            },
            is_formula: job.is_formula,
        });
    };

    let Some(tap_meta) = tap_meta.get(tap) else {
        if let Some((remote, branch)) = resolve_api_fallback_remote_branch(tap, None) {
            return PhaseOneResult::NeedsApi(ApiJob {
                index,
                name: job.name,
                installed: job.installed,
                target: job.target,
                is_formula: job.is_formula,
                remote,
                branch,
                source_path: source_path.to_string(),
                local_err: format!("tap '{tap}' is not installed locally"),
            });
        }

        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped {
                reason: format!("tap '{tap}' is not installed locally"),
                source: DataSource::None,
            },
            is_formula: job.is_formula,
        });
    };

    match git_last_commit_unix_seconds(&tap_meta.path, tap_meta.branch.as_deref(), source_path) {
        Ok(ts) => PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: action_from_commit_age(min_age, now_unix_secs, ts, DataSource::Git),
            is_formula: job.is_formula,
        }),
        Err(local_err) => {
            if let Some((remote, branch)) = resolve_api_fallback_remote_branch(tap, Some(tap_meta))
            {
                PhaseOneResult::NeedsApi(ApiJob {
                    index,
                    name: job.name,
                    installed: job.installed,
                    target: job.target,
                    is_formula: job.is_formula,
                    remote,
                    branch,
                    source_path: source_path.to_string(),
                    local_err: local_err.to_string(),
                })
            } else {
                PhaseOneResult::Final(PlanItem {
                    name: job.name,
                    installed: job.installed,
                    target: job.target,
                    action: PlanAction::Skipped {
                        reason: format!(
                            "failed age check: local git failed ({local_err}) and no remote fallback available"
                        ),
                        source: DataSource::None,
                    },
                    is_formula: job.is_formula,
                })
            }
        }
    }
}

fn item_to_outcome(item: &PlanItem) -> ItemOutcome {
    match &item.action {
        PlanAction::Upgrade { source } => ItemOutcome::update(
            PLUGIN.id(),
            item.name.clone(),
            item.installed.clone(),
            item.target.clone(),
            source.as_str(),
        ),
        PlanAction::Delayed {
            age,
            required,
            source,
        } => ItemOutcome::delayed_too_fresh(
            PLUGIN.id(),
            item.name.clone(),
            item.installed.clone(),
            item.target.clone(),
            source.as_str(),
            age.clone(),
            required.clone(),
        ),
        PlanAction::Skipped { reason, source } => {
            if reason.contains("failed age check") {
                return ItemOutcome::error(
                    PLUGIN.id(),
                    item.name.clone(),
                    item.installed.clone(),
                    item.target.clone(),
                    source.as_str(),
                    REASON_COMMAND_FAILED,
                    reason.clone(),
                );
            }

            let reason_code = if reason.starts_with("pinned") {
                REASON_PINNED
            } else {
                REASON_MISSING_METADATA
            };

            ItemOutcome::skipped(
                PLUGIN.id(),
                item.name.clone(),
                item.installed.clone(),
                item.target.clone(),
                source.as_str(),
                reason_code,
                reason.clone(),
            )
        }
    }
}

fn action_from_commit_age(
    min_age: Duration,
    now_unix_secs: u64,
    committed_at: u64,
    source: DataSource,
) -> PlanAction {
    let age_secs = now_unix_secs.saturating_sub(committed_at);

    if age_secs >= min_age.as_secs() {
        return PlanAction::Upgrade { source };
    }

    let age = human_age(age_secs);
    let required = human_age(min_age.as_secs());
    PlanAction::Delayed {
        age,
        required,
        source,
    }
}

fn git_last_commit_unix_seconds(
    repo_path: &str,
    branch: Option<&str>,
    source_path: &str,
) -> Result<u64> {
    // Some Homebrew taps can have an invalid local HEAD (e.g. refs/heads/.invalid),
    // so implicit HEAD-based commands fail even though origin/* and FETCH_HEAD are valid.
    // `brew update-reset` usually repairs tap refs if needed.
    let mut refs: Vec<String> = Vec::new();

    let mut push_unique = |git_ref: String| {
        if !refs.iter().any(|existing| existing == &git_ref) {
            refs.push(git_ref);
        }
    };

    if let Some(branch) = branch.filter(|b| !b.is_empty()) {
        push_unique(format!("origin/{branch}"));
    }

    push_unique("origin/HEAD".to_string());
    push_unique("FETCH_HEAD".to_string());
    push_unique("HEAD".to_string());

    let mut last_err = String::new();
    for git_ref in refs {
        match git_log_timestamp_for_ref(repo_path, source_path, &git_ref) {
            Ok(ts) => return Ok(ts),
            Err(err) => last_err = format!("{git_ref}: {err}"),
        }
    }

    bail!("git log failed for all refs ({last_err})")
}

fn git_log_timestamp_for_ref(repo_path: &str, source_path: &str, git_ref: &str) -> Result<u64> {
    let stdout = RunCmd::Success.text(
        "git",
        [
            "-C",
            repo_path,
            "log",
            "-1",
            "--format=%ct",
            git_ref,
            "--",
            source_path,
        ],
    )?;

    let ts = stdout
        .trim()
        .parse::<u64>()
        .with_context(|| format!("invalid git timestamp for {source_path}"))?;
    Ok(ts)
}

fn github_last_commit_unix_seconds(
    client: &Client,
    remote: &str,
    branch: Option<&str>,
    source_path: &str,
) -> Result<u64> {
    let (owner, repo) = parse_github_remote(remote)
        .with_context(|| format!("unsupported non-GitHub remote '{remote}'"))?;

    let mut url = reqwest::Url::parse(&format!(
        "https://api.github.com/repos/{owner}/{repo}/commits"
    ))
    .context("failed to build GitHub commits URL")?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("path", source_path);
        if let Some(branch) = branch.filter(|b| !b.is_empty()) {
            q.append_pair("sha", branch);
        }
        q.append_pair("per_page", "1");
    }

    let commits: Vec<GitHubCommitItem> = client
        .get(url)
        .send()
        .context("GitHub API request failed")?
        .error_for_status()
        .context("GitHub API returned an error status")?
        .json()
        .context("failed to parse GitHub API response")?;

    let first = commits
        .first()
        .context("GitHub API returned no commits for this file")?;

    let date = first
        .commit
        .committer
        .as_ref()
        .map(|p| p.date.as_str())
        .or_else(|| first.commit.author.as_ref().map(|p| p.date.as_str()))
        .context("GitHub commit payload missing date")?;

    let dt = chrono::DateTime::parse_from_rfc3339(date)
        .with_context(|| format!("invalid RFC3339 date from GitHub: {date}"))?;

    u64::try_from(dt.timestamp()).context("GitHub commit timestamp is negative")
}

fn parse_github_remote(remote: &str) -> Option<(String, String)> {
    let rest = if let Some(r) = remote.strip_prefix("https://github.com/") {
        r
    } else if let Some(r) = remote.strip_prefix("http://github.com/") {
        r
    } else if let Some(r) = remote.strip_prefix("git@github.com:") {
        r
    } else if let Some(r) = remote.strip_prefix("ssh://git@github.com/") {
        r
    } else {
        return None;
    };

    let cleaned = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = cleaned.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    Some((owner, repo))
}

fn resolve_api_fallback_remote_branch(
    tap: &str,
    tap_meta: Option<&TapMeta>,
) -> Option<(String, Option<String>)> {
    if let Some(meta) = tap_meta {
        if let Some(remote) = meta.remote.clone() {
            return Some((remote, meta.branch.clone()));
        }
    }

    match tap {
        "homebrew/core" => Some((
            "https://github.com/Homebrew/homebrew-core".to_string(),
            Some("main".to_string()),
        )),
        "homebrew/cask" => Some((
            "https://github.com/Homebrew/homebrew-cask".to_string(),
            Some("main".to_string()),
        )),
        _ => None,
    }
}

fn brew_tap_meta() -> Result<HashMap<String, TapMeta>> {
    let taps: Vec<TapInfo> =
        RunCmd::Success.json("brew", &["tap-info", "--json", "--installed"])?;
    Ok(taps
        .into_iter()
        .map(|t| {
            (
                t.name,
                TapMeta {
                    path: t.path,
                    remote: t.remote,
                    branch: t.branch,
                },
            )
        })
        .collect())
}

fn brew_info_for_names(formula_names: &[String], cask_names: &[String]) -> Result<InfoRoot> {
    if formula_names.is_empty() && cask_names.is_empty() {
        return Ok(InfoRoot {
            formulae: Vec::new(),
            casks: Vec::new(),
        });
    }

    let mut args = vec!["info".to_string(), "--json=v2".to_string()];
    args.extend(formula_names.iter().cloned());
    args.extend(cask_names.iter().cloned());

    RunCmd::Success.json("brew", &args)
}

fn brew_info_installed() -> Result<InfoRoot> {
    RunCmd::Success.json("brew", ["info", "--json=v2", "--installed"])
}

fn github_client() -> Result<Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("upnow/0.1"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );

    let token = std::env::var("HOMEBREW_GITHUB_API_TOKEN")
        .ok()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .or_else(|| std::env::var("GH_TOKEN").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if let Some(token) = token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .context("invalid GitHub token value in env")?;
        headers.insert(AUTHORIZATION, value);
    }

    Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(8))
        .build()
        .context("failed to build HTTP client")
}

fn emit_manager_error(detail: impl AsRef<str>) {
    emit_manager_level_error(PLUGIN.id(), PLUGIN.id(), detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_https_remote() {
        let parsed = parse_github_remote("https://github.com/Homebrew/homebrew-core.git")
            .expect("should parse");
        assert_eq!(parsed.0, "Homebrew");
        assert_eq!(parsed.1, "homebrew-core");
    }

    #[test]
    fn parse_github_ssh_remote() {
        let parsed =
            parse_github_remote("git@github.com:Homebrew/homebrew-cask.git").expect("should parse");
        assert_eq!(parsed.0, "Homebrew");
        assert_eq!(parsed.1, "homebrew-cask");
    }

    #[test]
    fn fallback_remote_for_homebrew_cask() {
        let (remote, branch) =
            resolve_api_fallback_remote_branch("homebrew/cask", None).expect("fallback expected");
        assert_eq!(remote, "https://github.com/Homebrew/homebrew-cask");
        assert_eq!(branch.as_deref(), Some("main"));
    }

    #[test]
    fn fallback_remote_prefers_tap_meta_remote() {
        let meta = TapMeta {
            path: "/tmp/does-not-matter".to_string(),
            remote: Some("https://github.com/acme/custom-tap".to_string()),
            branch: Some("master".to_string()),
        };

        let (remote, branch) = resolve_api_fallback_remote_branch("homebrew/cask", Some(&meta))
            .expect("fallback expected");
        assert_eq!(remote, "https://github.com/acme/custom-tap");
        assert_eq!(branch.as_deref(), Some("master"));
    }
}
