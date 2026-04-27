use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;

use crate::config::is_pinned;
use crate::managers::shared::plan::VersionPolicyMeta;
use crate::managers::shared::versioning::policy::{
    ReleaseClass, VersionPolicy, evaluate_version_policy,
};
use crate::managers::shared::versioning::{
    DEV_LABELS, RC_LABELS, leading_alpha_prefix, matches_any_label, select_less_stable_class,
};
#[allow(clippy::wildcard_imports)]
use crate::managers::*;
use crate::outcome::{ItemOutcome, emit_text_outcome};
use crate::ui::output_theme;
use crate::util::http::{HTTP_TIMEOUT_SECS, HTTP_USER_AGENT};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::{human_age, now_unix_secs};

const BREW_MAX_PARALLEL_CHECKS_MIN: usize = 1;
const BREW_API_FALLBACK_MAX_PARALLEL_CHECKS: usize = 4;

pub struct BrewPlugin;

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

    fn supports_version_policy(&self, _policy: VersionPolicy) -> bool {
        true
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub static PLUGIN: BrewPlugin = BrewPlugin;

#[derive(Debug, Clone, Deserialize)]
struct OutdatedRoot {
    formulae: Vec<OutdatedFormula>,
    casks: Vec<OutdatedCask>,
}

#[derive(Debug, Clone, Deserialize)]
struct OutdatedFormula {
    name: String,
    installed_versions: Vec<String>,
    current_version: String,
    pinned: bool,
    _pinned_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Clone)]
enum PlanAction {
    Upgrade,
    Delayed { age: String, required: String },
    CurrentBlockedByPolicy,
    Skipped { reason: String },
}

#[derive(Clone)]
struct PlanItem {
    name: String,
    installed: String,
    target: String,
    action: PlanAction,
    is_formula: bool,
    version_policy: Option<VersionPolicyMeta>,
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

struct BrewCollected {
    plan: Vec<PlanItem>,
    upgradable: Vec<PlannedUpdate>,
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
    version_policy: Option<VersionPolicyMeta>,
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
    run_manager_pipeline(ctx, scan, run_plan_apply)
}

fn run_plan_apply(ctx: &ManagerCtx) -> Result<()> {
    maybe_refresh_brew_metadata(ctx.policy.no_update);

    run_plan_apply_framework(
        ctx,
        PLUGIN.id(),
        PlanApplyFrameworkPolicy::SOFT_FETCH_STRICT_RESOLVE,
        || {
            run_cmd("brew", ["outdated", "--json=v2"], CmdStatus::Success)
                .output()
                .and_then(|output| output.json())
                .context("failed to read brew outdated state")
        },
        |outdated: &OutdatedRoot| outdated.formulae.is_empty() && outdated.casks.is_empty(),
        |outdated, runtime| {
            let tap_meta = soft_fail_or(
                brew_tap_meta(),
                HashMap::new,
                PLUGIN.id(),
                "failed to read brew tap metadata",
            );

            let github_client = soft_fail(
                github_client(),
                PLUGIN.id(),
                "failed to initialize remote lookup client",
            );

            let jobs = build_brew_plan_jobs(outdated.clone());
            resolve_brew_plan(
                jobs,
                &tap_meta,
                github_client.as_ref(),
                runtime.min_age,
                ctx.policy.version_policy,
                runtime.now_unix_secs,
                runtime.max_parallel_checks,
            )
            .context("planning execution failed")
        },
        |_outdated, plan, _runtime| {
            for item in &plan {
                if ctx.is_interactive_apply() && matches!(item.action, PlanAction::Upgrade) {
                    continue;
                }

                let outcome = if matches!(item.action, PlanAction::Upgrade)
                    && is_pinned(&item.name, &ctx.policy.pinned)
                {
                    ItemOutcome::skipped_pinned(
                        PLUGIN.id(),
                        item.name.clone(),
                        item.installed.clone(),
                        item.target.clone(),
                    )
                } else {
                    item_to_outcome(item)
                };
                emit_text_outcome(&outcome);
            }

            let upgradable: Vec<PlannedUpdate> = plan
                .iter()
                .enumerate()
                .filter_map(|(idx, item)| match item.action {
                    PlanAction::Upgrade => Some(PlannedUpdate {
                        manager: PLUGIN.id(),
                        name: item.name.clone(),
                        current: item.installed.clone(),
                        target: item.target.clone(),
                        delayed_latest: None,
                        version_policy: item.version_policy.clone(),
                        apply_spec_base: Some(idx.to_string()),
                    }),
                    PlanAction::Delayed { .. }
                    | PlanAction::CurrentBlockedByPolicy
                    | PlanAction::Skipped { .. } => None,
                })
                .collect();

            Ok(BrewCollected { plan, upgradable })
        },
        |ctx, _outdated, collected| {
            run_per_item_apply_flow(ctx, PLUGIN.id(), collected.upgradable, move |selected| {
                apply_brew_selected_plan(collected.plan, selected);
            })
        },
    )
}

fn apply_brew_selected_plan(plan: Vec<PlanItem>, selected: Vec<PlannedUpdate>) {
    let selected_indices: BTreeSet<usize> = selected
        .into_iter()
        .filter_map(|item| item.apply_spec_base)
        .filter_map(|raw| raw.parse::<usize>().ok())
        .collect();

    let filtered_plan: Vec<PlanItem> = plan
        .into_iter()
        .enumerate()
        .map(|(idx, mut item)| {
            if matches!(item.action, PlanAction::Upgrade) && !selected_indices.contains(&idx) {
                item.action = PlanAction::Skipped {
                    reason: "pinned".to_string(),
                };
            }
            item
        })
        .collect();

    apply_brew_plan(&filtered_plan);
}

fn maybe_refresh_brew_metadata(no_update: bool) {
    if !no_update
        && let Err(err) = run_cmd("brew", ["update", "--quiet"], CmdStatus::Success)
            .mutating()
            .output()
    {
        emit_manager_level_error_with(PLUGIN.id(), "brew metadata refresh failed", err);
    }
}

fn build_brew_plan_jobs(outdated: OutdatedRoot) -> Vec<PackageJob> {
    let formula_names: Vec<String> = outdated.formulae.iter().map(|f| f.name.clone()).collect();
    let cask_names: Vec<String> = outdated.casks.iter().map(|c| c.name.clone()).collect();

    let info = soft_fail_or(
        brew_info_for_names(&formula_names, &cask_names),
        || InfoRoot {
            formulae: Vec::new(),
            casks: Vec::new(),
        },
        PLUGIN.id(),
        "failed to read brew package metadata",
    );

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
            Some("pinned".to_string())
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
    version_policy: VersionPolicy,
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
            .map(|(index, job)| {
                phase_one_local_check(index, job, min_age, version_policy, now_unix_secs, tap_meta)
            })
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
                            Ok(ts) => action_from_commit_age(min_age, now_unix_secs, ts),
                            Err(remote_err) => PlanAction::Skipped {
                                reason: format!(
                                    "failed age check: local git failed ({}); remote lookup failed ({})",
                                    job.local_err, remote_err
                                ),
                            },
                        }
                    } else {
                        PlanAction::Skipped {
                            reason: format!(
                                "failed age check: local git failed ({}) and remote lookup is unavailable",
                                job.local_err
                            ),
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
                            version_policy: job.version_policy,
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
            PlanAction::Upgrade => Some(i.name.clone()),
            PlanAction::Delayed { .. }
            | PlanAction::CurrentBlockedByPolicy
            | PlanAction::Skipped { .. } => None,
        })
        .collect();

    let casks_to_upgrade: Vec<String> = plan
        .iter()
        .filter(|i| !i.is_formula)
        .filter_map(|i| match i.action {
            PlanAction::Upgrade => Some(i.name.clone()),
            PlanAction::Delayed { .. }
            | PlanAction::CurrentBlockedByPolicy
            | PlanAction::Skipped { .. } => None,
        })
        .collect();

    if !formula_to_upgrade.is_empty() {
        let mut args = vec!["upgrade".to_string(), "--formula".to_string()];
        args.extend(formula_to_upgrade);
        if let Err(err) = run_cmd("brew", &args, CmdStatus::Success)
            .mutating()
            .output()
        {
            emit_manager_level_error_with(
                PLUGIN.id(),
                "failed to apply brew formula upgrades",
                err,
            );
        }
    }

    if !casks_to_upgrade.is_empty() {
        let mut args = vec!["upgrade".to_string(), "--cask".to_string()];
        args.extend(casks_to_upgrade);
        if let Err(err) = run_cmd("brew", &args, CmdStatus::Success)
            .mutating()
            .output()
        {
            emit_manager_level_error_with(PLUGIN.id(), "failed to apply brew cask upgrades", err);
        }
    }
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let Some(scan_items) = soft_fail(
        collect_brew_scan_items(),
        PLUGIN.id(),
        "failed to collect brew scan items",
    ) else {
        return Ok(());
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
            .map_or_else(|| "unknown".to_string(), |item| item.version.clone());

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
            .map_or_else(|| "unknown".to_string(), ToString::to_string);

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
        emit_scan_current(PLUGIN.id(), item.name, item.version, age, old_threshold);
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
    version_policy: VersionPolicy,
    now_unix_secs: u64,
    tap_meta: &HashMap<String, TapMeta>,
) -> PhaseOneResult {
    if let Some(reason) = job.initial_skip_reason {
        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped { reason },
            is_formula: job.is_formula,
            version_policy: None,
        });
    }

    let (policy_action, version_policy_meta) =
        policy_gate_for_brew(&job.installed, &job.target, version_policy);

    if let Some(action) = policy_action {
        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action,
            is_formula: job.is_formula,
            version_policy: version_policy_meta,
        });
    }

    let Some((tap, source_path)) = job.tap_and_source.as_ref() else {
        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped {
                reason: "unable to resolve package metadata from brew info".to_string(),
            },
            is_formula: job.is_formula,
            version_policy: version_policy_meta,
        });
    };

    let Some(tap) = tap.as_deref() else {
        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped {
                reason: "missing tap".to_string(),
            },
            is_formula: job.is_formula,
            version_policy: version_policy_meta,
        });
    };

    let Some(source_path) = source_path.as_deref() else {
        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped {
                reason: "missing ruby_source_path".to_string(),
            },
            is_formula: job.is_formula,
            version_policy: version_policy_meta,
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
                version_policy: version_policy_meta,
            });
        }

        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped {
                reason: format!("tap '{tap}' is not installed locally"),
            },
            is_formula: job.is_formula,
            version_policy: version_policy_meta,
        });
    };

    match git_last_commit_unix_seconds(&tap_meta.path, tap_meta.branch.as_deref(), source_path) {
        Ok(ts) => PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: action_from_commit_age(min_age, now_unix_secs, ts),
            is_formula: job.is_formula,
            version_policy: version_policy_meta,
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
                    version_policy: version_policy_meta,
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
                    },
                    is_formula: job.is_formula,
                    version_policy: version_policy_meta,
                })
            }
        }
    }
}

fn item_to_outcome(item: &PlanItem) -> ItemOutcome {
    let mut outcome = match &item.action {
        PlanAction::Upgrade => ItemOutcome::update(
            PLUGIN.id(),
            item.name.clone(),
            item.installed.clone(),
            item.target.clone(),
        ),
        PlanAction::Delayed { age, required } => ItemOutcome::delayed_too_fresh(
            PLUGIN.id(),
            item.name.clone(),
            item.installed.clone(),
            item.target.clone(),
            age.clone(),
            required.clone(),
        ),
        PlanAction::CurrentBlockedByPolicy => {
            ItemOutcome::current(PLUGIN.id(), item.name.clone(), item.installed.clone())
        }
        PlanAction::Skipped { reason } => {
            if reason.contains("failed age check") {
                return ItemOutcome::resolver_error(
                    PLUGIN.id(),
                    item.name.clone(),
                    item.installed.clone(),
                    item.target.clone(),
                    reason.clone(),
                );
            }

            if reason.starts_with("pinned") {
                ItemOutcome::skipped_pinned(
                    PLUGIN.id(),
                    item.name.clone(),
                    item.installed.clone(),
                    item.target.clone(),
                )
            } else {
                ItemOutcome::skipped_missing_metadata(
                    PLUGIN.id(),
                    item.name.clone(),
                    item.installed.clone(),
                    item.target.clone(),
                    reason.clone(),
                )
            }
        }
    };

    if let Some(policy) = &item.version_policy {
        policy.apply_to_outcome(&mut outcome);
    }

    outcome
}

fn action_from_commit_age(min_age: Duration, now_unix_secs: u64, committed_at: u64) -> PlanAction {
    let age_secs = now_unix_secs.saturating_sub(committed_at);

    if age_secs >= min_age.as_secs() {
        return PlanAction::Upgrade;
    }

    let age = human_age(age_secs);
    let required = human_age(min_age.as_secs());
    PlanAction::Delayed { age, required }
}

fn policy_gate_for_brew(
    installed: &str,
    target: &str,
    policy: VersionPolicy,
) -> (Option<PlanAction>, Option<VersionPolicyMeta>) {
    // Homebrew exposes one chosen outdated target, not a candidate timeline we
    // can reselect from. Version policy is therefore a target-safety gate here:
    // accept Homebrew's target or block it, but do not synthesize an older one.
    let installed_class = classify_brew_release(installed);
    let target_class = classify_brew_release(target);
    let decision = evaluate_version_policy(policy, installed_class, target_class);

    let action = (!decision.allowed).then_some(PlanAction::CurrentBlockedByPolicy);
    let version_policy =
        (action.is_some() || decision.warning.is_some()).then_some(VersionPolicyMeta {
            policy,
            latest_blocked_version: action.as_ref().map(|_| target.to_string()),
            warning: decision.warning,
        });

    (action, version_policy)
}

fn classify_brew_release(raw: &str) -> ReleaseClass {
    let normalized = normalize_brew_version_for_policy(raw);
    let version = normalized.trim();

    if version.is_empty()
        || version.eq_ignore_ascii_case("latest")
        || !version.chars().any(|ch| ch.is_ascii_alphanumeric())
    {
        return ReleaseClass::Unknown;
    }

    classify_brew_prerelease(version).unwrap_or(ReleaseClass::Final)
}

fn normalize_brew_version_for_policy(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_cask_build = trimmed
        .split_once(',')
        .map_or(trimmed, |(head, _)| head.trim());

    strip_brew_revision_suffix(without_cask_build).to_string()
}

fn strip_brew_revision_suffix(raw: &str) -> &str {
    let Some((head, revision)) = raw.rsplit_once('_') else {
        return raw;
    };

    if !head.is_empty() && revision.chars().all(|ch| ch.is_ascii_digit()) {
        return head;
    }

    raw
}

fn classify_brew_prerelease(version: &str) -> Option<ReleaseClass> {
    let mut best_match = None;
    let mut token_start = None;

    for (idx, ch) in version.char_indices() {
        if ch.is_ascii_alphanumeric() {
            token_start.get_or_insert(idx);
        } else if let Some(start) = token_start.take() {
            best_match = update_brew_prerelease_match(best_match, version, start, idx);
        }
    }

    if let Some(start) = token_start {
        best_match = update_brew_prerelease_match(best_match, version, start, version.len());
    }

    best_match
}

fn update_brew_prerelease_match(
    current: Option<ReleaseClass>,
    version: &str,
    start: usize,
    end: usize,
) -> Option<ReleaseClass> {
    let token = &version[start..end];
    let Some(next) = classify_brew_prerelease_token(version, start, token) else {
        return current;
    };

    Some(select_less_stable_class(current, next))
}

fn classify_brew_prerelease_token(
    version: &str,
    token_start: usize,
    token: &str,
) -> Option<ReleaseClass> {
    let normalized = token.to_ascii_lowercase();
    let marker = normalized
        .trim_start_matches(|ch: char| ch.is_ascii_digit())
        .trim();
    if marker.is_empty() {
        return None;
    }

    let label = leading_alpha_prefix(marker);
    if label.is_empty() {
        return None;
    }

    if matches_any_label(label, DEV_LABELS) || matches_any_label(label, BREW_EXTRA_DEV_LABELS) {
        return Some(ReleaseClass::Dev);
    }
    if label == "alpha" {
        return Some(ReleaseClass::Alpha);
    }
    if label == "beta" {
        return Some(ReleaseClass::Beta);
    }
    if matches_any_label(label, RC_LABELS) {
        return Some(ReleaseClass::Rc);
    }
    if matches_any_label(label, BREW_SHORT_ALPHA_LABELS)
        && has_short_brew_prerelease_context(version, token_start, token, marker)
    {
        return Some(ReleaseClass::Alpha);
    }
    if matches_any_label(label, BREW_SHORT_BETA_LABELS)
        && has_short_brew_prerelease_context(version, token_start, token, marker)
    {
        return Some(ReleaseClass::Beta);
    }

    None
}

fn has_short_brew_prerelease_context(
    version: &str,
    token_start: usize,
    token: &str,
    marker: &str,
) -> bool {
    if marker.len() < token.len() {
        return true;
    }

    let prefix = version[..token_start]
        .trim_start_matches(['v', 'V'])
        .trim_matches(|ch: char| matches!(ch, '.' | '-' | '_' | '+'));

    prefix.is_empty()
        || (prefix.chars().any(|ch| ch.is_ascii_digit())
            && prefix
                .chars()
                .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '_' | '+')))
}

const BREW_EXTRA_DEV_LABELS: &[&str] = &["head"];
const BREW_SHORT_ALPHA_LABELS: &[&str] = &["a"];
const BREW_SHORT_BETA_LABELS: &[&str] = &["b"];

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
    let output = run_cmd(
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
        CmdStatus::Success,
    )
    .output()?;
    let stdout = output.stdout()?;

    let ts = stdout
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
    if let Some(meta) = tap_meta
        && let Some(remote) = meta.remote.clone()
    {
        return Some((remote, meta.branch.clone()));
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
    let taps: Vec<TapInfo> = run_cmd(
        "brew",
        ["tap-info", "--json", "--installed"],
        CmdStatus::Success,
    )
    .output()?
    .json()?;
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

fn brew_info_for_names<S1: AsRef<str>, S2: AsRef<str>>(
    formula_names: &[S1],
    cask_names: &[S2],
) -> Result<InfoRoot> {
    if formula_names.is_empty() && cask_names.is_empty() {
        return Ok(InfoRoot {
            formulae: Vec::new(),
            casks: Vec::new(),
        });
    }

    let mut args: Vec<&str> = vec!["info", "--json=v2"];
    args.extend(formula_names.iter().map(AsRef::as_ref));
    args.extend(cask_names.iter().map(AsRef::as_ref));

    run_cmd("brew", &args, CmdStatus::Success).output()?.json()
}

fn brew_info_installed() -> Result<InfoRoot> {
    run_cmd(
        "brew",
        ["info", "--json=v2", "--installed"],
        CmdStatus::Success,
    )
    .output()?
    .json()
}

fn github_client() -> Result<Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(HTTP_USER_AGENT));
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
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .context("failed to build HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::shared::versioning::policy::PolicyWarning;

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

    #[test]
    fn policy_gate_blocks_prerelease_under_stable() {
        let (action, policy) = policy_gate_for_brew("1.2.0", "1.3.0-beta.1", VersionPolicy::Stable);

        assert!(matches!(action, Some(PlanAction::CurrentBlockedByPolicy)));
        let policy = policy.expect("stable block should include policy metadata");
        assert_eq!(policy.policy, VersionPolicy::Stable);
        assert_eq!(
            policy.latest_blocked_version.as_deref(),
            Some("1.3.0-beta.1")
        );
        assert_eq!(policy.warning, None);
    }

    #[test]
    fn policy_gate_allows_final_under_stable() {
        let (action, policy) = policy_gate_for_brew("1.2.0", "1.3.0", VersionPolicy::Stable);
        assert!(action.is_none());
        assert!(policy.is_none());
    }

    #[test]
    fn brew_classifier_treats_non_semver_stable_versions_as_final() {
        for version in [
            "2024-01-31",
            "jdk-21.0.2+13",
            "8u402-b06",
            "1.2.3-openssl3",
            "2024.01.01,abcd",
            "1.2.3-vendor_2",
        ] {
            assert_eq!(
                classify_brew_release(version),
                ReleaseClass::Final,
                "{version}"
            );
            let (action, policy) = policy_gate_for_brew("1.0.0", version, VersionPolicy::Stable);
            assert!(action.is_none(), "{version}");
            assert!(policy.is_none(), "{version}");
        }
    }

    #[test]
    fn brew_classifier_blocks_clear_prerelease_markers_under_stable() {
        for (version, release_class) in [
            ("1.2.3-alpha.1", ReleaseClass::Alpha),
            ("1.2.3-a1", ReleaseClass::Alpha),
            ("1.2.3-beta.1", ReleaseClass::Beta),
            ("1.2.3-b1", ReleaseClass::Beta),
            ("2.0-rc1", ReleaseClass::Rc),
            ("1.0-pre", ReleaseClass::Rc),
            ("nightly", ReleaseClass::Dev),
            ("preview", ReleaseClass::Dev),
            ("1.0-dev", ReleaseClass::Dev),
            ("foo-canary", ReleaseClass::Dev),
        ] {
            assert_eq!(classify_brew_release(version), release_class, "{version}");
            let (action, policy) = policy_gate_for_brew("1.0.0", version, VersionPolicy::Stable);
            assert!(action.is_some(), "{version}");
            assert!(policy.is_some(), "{version}");
        }
    }

    #[test]
    fn brew_classifier_keeps_unknown_sentinels_unknown() {
        for version in ["", "   ", ",123", "latest", "LATEST"] {
            assert_eq!(
                classify_brew_release(version),
                ReleaseClass::Unknown,
                "{version}"
            );
        }
    }

    #[test]
    fn same_track_unknown_installed_falls_back_to_stable_and_blocks_prerelease() {
        let (action, policy) =
            policy_gate_for_brew("latest", "1.0.0-beta.1", VersionPolicy::SameTrack);

        assert!(matches!(action, Some(PlanAction::CurrentBlockedByPolicy)));
        let policy = policy.expect("same-track fallback block should include policy metadata");
        assert_eq!(policy.policy, VersionPolicy::SameTrack);
        assert_eq!(
            policy.latest_blocked_version.as_deref(),
            Some("1.0.0-beta.1")
        );
        assert_eq!(
            policy.warning,
            Some(PolicyWarning::InstalledTrackUnknownFallbackStable)
        );
    }

    #[test]
    fn same_track_unknown_installed_allows_final_with_warning() {
        let (action, policy) = policy_gate_for_brew("latest", "1.0.0", VersionPolicy::SameTrack);

        assert!(action.is_none());
        let policy = policy.expect("same-track fallback should include policy metadata");
        assert_eq!(policy.policy, VersionPolicy::SameTrack);
        assert_eq!(policy.latest_blocked_version, None);
        assert_eq!(
            policy.warning,
            Some(PolicyWarning::InstalledTrackUnknownFallbackStable)
        );
    }

    #[test]
    fn item_to_outcome_preserves_blocked_same_track_context_with_fallback_warning() {
        let (action, version_policy) =
            policy_gate_for_brew("latest", "1.0.0-beta.1", VersionPolicy::SameTrack);

        let item = PlanItem {
            name: "demo".to_string(),
            installed: "latest".to_string(),
            target: "1.0.0-beta.1".to_string(),
            action: action.expect("fallback stable should block prerelease target"),
            is_formula: true,
            version_policy,
        };

        let outcome = item_to_outcome(&item);
        let policy = outcome
            .diagnostics
            .version_policy
            .as_ref()
            .expect("version policy diagnostic should be present");
        assert_eq!(policy.policy, "same-track");
        assert_eq!(
            policy.latest_blocked_version.as_deref(),
            Some("1.0.0-beta.1")
        );
        assert_eq!(
            policy.warning.as_deref(),
            Some("same-track fell back to stable because installed track is unknown")
        );
    }

    #[test]
    fn update_outcome_preserves_same_track_allowed_warning() {
        let (_action, version_policy) =
            policy_gate_for_brew("latest", "1.0.0", VersionPolicy::SameTrack);

        let item = PlanItem {
            name: "demo".to_string(),
            installed: "latest".to_string(),
            target: "1.0.0".to_string(),
            action: PlanAction::Upgrade,
            is_formula: true,
            version_policy,
        };

        let outcome = item_to_outcome(&item);
        let policy = outcome
            .diagnostics
            .version_policy
            .as_ref()
            .expect("version policy diagnostic should be present");
        assert_eq!(policy.policy, "same-track");
        assert_eq!(policy.latest_blocked_version, None);
        assert_eq!(
            policy.warning.as_deref(),
            Some("same-track fell back to stable because installed track is unknown")
        );
    }

    #[test]
    fn delayed_outcome_preserves_same_track_allowed_warning() {
        let (_action, version_policy) =
            policy_gate_for_brew("latest", "1.0.0", VersionPolicy::SameTrack);

        let item = PlanItem {
            name: "demo".to_string(),
            installed: "latest".to_string(),
            target: "1.0.0".to_string(),
            action: PlanAction::Delayed {
                age: "1h".to_string(),
                required: "12h".to_string(),
            },
            is_formula: true,
            version_policy,
        };

        let outcome = item_to_outcome(&item);
        let policy = outcome
            .diagnostics
            .version_policy
            .as_ref()
            .expect("version policy diagnostic should be present");
        assert_eq!(policy.policy, "same-track");
        assert_eq!(policy.latest_blocked_version, None);
        assert_eq!(
            policy.warning.as_deref(),
            Some("same-track fell back to stable because installed track is unknown")
        );
    }

    #[test]
    fn policy_normalization_strips_brew_revision_suffix() {
        assert_eq!(normalize_brew_version_for_policy("1.2.3_1"), "1.2.3");
        assert_eq!(
            normalize_brew_version_for_policy("1.2.3-rc1_2"),
            "1.2.3-rc1"
        );
    }

    #[test]
    fn policy_normalization_strips_cask_build_component() {
        assert_eq!(normalize_brew_version_for_policy("1.2.3,12345"), "1.2.3");
        assert_eq!(
            normalize_brew_version_for_policy("1.2.3-beta.1,abcdef"),
            "1.2.3-beta.1"
        );
    }

    #[test]
    fn stable_policy_allows_formula_revision_suffixes() {
        let (action, policy) = policy_gate_for_brew("1.2.3_1", "1.2.4_2", VersionPolicy::Stable);
        assert!(action.is_none());
        assert!(policy.is_none());
    }

    #[test]
    fn stable_policy_blocks_cask_prerelease_after_comma_normalization() {
        let (action, policy) =
            policy_gate_for_brew("1.2.3,12345", "1.3.0-beta.1,67890", VersionPolicy::Stable);

        assert!(matches!(action, Some(PlanAction::CurrentBlockedByPolicy)));
        let policy = policy.expect("stable block should include policy metadata");
        assert_eq!(policy.policy, VersionPolicy::Stable);
        assert_eq!(
            policy.latest_blocked_version.as_deref(),
            Some("1.3.0-beta.1,67890")
        );
        assert_eq!(policy.warning, None);
    }
}
