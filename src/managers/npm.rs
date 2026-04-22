use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::managers::shared::versioning::policy::VersionPolicy;
#[allow(clippy::wildcard_imports)]
use crate::managers::*;
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};

const NPM_MAX_PARALLEL_CHECKS: usize = 6;

pub struct NpmPlugin;

impl ManagerPlugin for NpmPlugin {
    fn id(&self) -> &'static str {
        "npm"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn supports_version_policy(&self, _policy: VersionPolicy) -> bool {
        true
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub static PLUGIN: NpmPlugin = NpmPlugin;

#[derive(Debug, Deserialize)]
struct OutdatedEntry {
    current: String,
}

#[derive(Debug, Deserialize)]
struct NpmLsJson {
    #[serde(default)]
    dependencies: BTreeMap<String, NpmLsDependency>,
}

#[derive(Debug, Deserialize)]
struct NpmLsDependency {
    version: Option<String>,
}

type NpmTimeMap = BTreeMap<String, String>;

type NpmPlanItem = ResolvedPlanItem<AgeResolvedTarget>;

fn run(ctx: &ManagerCtx) -> Result<()> {
    run_manager_pipeline(ctx, scan, run_plan_apply)
}

fn run_plan_apply(ctx: &ManagerCtx) -> Result<()> {
    run_plan_apply_framework(
        ctx,
        PLUGIN.id(),
        PlanApplyFrameworkPolicy::SOFT_FETCH_STRICT_RESOLVE,
        || npm_outdated_global().context("failed to query outdated npm packages"),
        BTreeMap::is_empty,
        |outdated, runtime| {
            resolve_npm_plan(
                outdated,
                runtime.now_unix_secs,
                runtime.min_age,
                runtime.max_parallel_checks,
                ctx.policy.version_policy,
            )
            .context("planning execution failed")
        },
        |_outdated, plan, runtime| {
            Ok(collect_upgradable_from_resolved_plan(
                PLUGIN.id(),
                plan,
                runtime.min_age,
                runtime.suppress_update_outcomes,
                runtime.pinned,
            ))
        },
        |ctx, _outdated, upgradable| {
            let min_age_days = ctx.policy.min_release_age.whole_days();
            run_selective_or_global_apply_flow(
                ctx,
                PLUGIN.id(),
                upgradable,
                |selected| apply_npm_selected_updates(min_age_days, selected),
                || apply_npm_updates(min_age_days),
            )
        },
    )
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let Some(installed) = soft_fail(
        npm_installed_global(),
        PLUGIN.id(),
        "failed to query installed npm packages",
    ) else {
        return Ok(());
    };

    if installed.is_empty() {
        return Ok(());
    }

    let now = verbose_now_unix_secs()?;

    emit_version_scan_outcomes(
        PLUGIN.id(),
        installed,
        now,
        ctx.scan_old_age_threshold,
        npm_release_age_secs,
    );

    Ok(())
}

fn resolve_npm_plan(
    outdated: &BTreeMap<String, OutdatedEntry>,
    now_unix_secs: u64,
    min_age: Duration,
    max_parallel_checks: usize,
    version_policy: VersionPolicy,
) -> Result<Vec<NpmPlanItem>> {
    let jobs: Vec<(String, String)> = outdated
        .iter()
        .map(|(name, entry)| (name.clone(), entry.current.clone()))
        .collect();

    let threads = effective_parallelism(max_parallel_checks, NPM_MAX_PARALLEL_CHECKS);
    run_indexed_parallel(jobs, threads, PLUGIN.id(), |(name, current)| {
        let resolved = npm_resolve_target_with_min_age(
            &name,
            &current,
            now_unix_secs,
            min_age,
            version_policy,
        )
        .map_err(|err| err.to_string());

        NpmPlanItem::new(name, current, resolved)
    })
}

fn apply_npm_updates(min_age_days: u64) -> Result<()> {
    let min_age_days = min_age_days.to_string();
    run_cmd(
        "npm",
        ["-g", "update", "--min-release-age", &min_age_days],
        CmdStatus::Success,
    )
    .mutating()
    .output()?;

    Ok(())
}

fn apply_npm_selected_updates(min_age_days: u64, upgradable: Vec<crate::managers::PlannedUpdate>) {
    let min_age_days = min_age_days.to_string();

    for item in upgradable {
        let name = item.name;
        let current = item.current;
        let target = item.target;

        let args = [
            "-g".to_string(),
            "update".to_string(),
            name.clone(),
            "--min-release-age".to_string(),
            min_age_days.clone(),
        ];

        if let Err(err) = run_cmd("npm", &args, CmdStatus::Success)
            .mutating()
            .output()
        {
            emit_apply_error(PLUGIN.id(), name, current, target, err);
        }
    }
}

fn npm_installed_global() -> Result<BTreeMap<String, String>> {
    let parsed: NpmLsJson = run_cmd(
        "npm",
        ["ls", "-g", "--depth=0", "--json"],
        CmdStatus::Success,
    )
    .output()?
    .json()?;

    let mut out = BTreeMap::new();
    for (name, dep) in parsed.dependencies {
        if let Some(version) = dep.version {
            out.insert(name, version);
        }
    }

    Ok(out)
}

fn npm_outdated_global() -> Result<BTreeMap<String, OutdatedEntry>> {
    run_cmd("npm", ["outdated", "-g", "--json"], CmdStatus::Allow(&[1]))
        .output()?
        .json()
}

fn npm_resolve_target_with_min_age(
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
    version_policy: VersionPolicy,
) -> Result<AgeResolvedTarget> {
    let timestamps_by_version: NpmTimeMap =
        run_cmd("npm", ["view", name, "time", "--json"], CmdStatus::Success)
            .output()?
            .json()?;

    let releases = npm_semver_time_releases(name, &timestamps_by_version)?;

    let resolved =
        resolve_semver_with_min_age(current, &releases, now_unix_secs, min_age, version_policy)
            .with_context(|| format!("failed to resolve eligible semver target for {name}"))?;

    Ok(resolved.into())
}

fn npm_release_age_secs(name: &str, version: &str, now_unix_secs: u64) -> Result<Option<u64>> {
    let timestamps_by_version: NpmTimeMap =
        run_cmd("npm", ["view", name, "time", "--json"], CmdStatus::Success)
            .output()?
            .json()?;

    let releases = npm_semver_time_releases(name, &timestamps_by_version)?;
    Ok(release_age_secs_for_version(
        &releases,
        version,
        now_unix_secs,
    ))
}

fn npm_semver_time_releases(
    name: &str,
    timestamps_by_version: &NpmTimeMap,
) -> Result<Vec<SemverTimestamp>> {
    if timestamps_by_version.is_empty() {
        anyhow::bail!("npm view time JSON is empty for {name}");
    }

    parse_semver_time_releases(PLUGIN.id(), name, timestamps_by_version)
}
