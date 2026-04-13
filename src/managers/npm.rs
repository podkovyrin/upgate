use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, PlanDecision, PlanMeta, SemverAgeResolution, SemverTimestamp,
    emit_manager_level_error, emit_plan_and_collect_upgradable, emit_version_scan_outcomes,
    parse_semver_time_releases, release_age_secs_for_version, resolve_semver_with_min_age,
    verbose_now_unix_secs,
};
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::now_unix_secs;
use crate::util::timefmt::human_age;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

const NPM_MAX_PARALLEL_CHECKS: usize = 6;

pub(crate) struct NpmPlugin;

impl ManagerPlugin for NpmPlugin {
    fn id(&self) -> &'static str {
        "npm"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub(crate) static PLUGIN: NpmPlugin = NpmPlugin;

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

struct NpmPlanItem {
    name: String,
    current: String,
    resolved: Result<NpmResolvedTarget, String>,
}

fn run(ctx: &ManagerCtx) -> Result<()> {
    if ctx.is_scan() {
        return scan(ctx);
    }

    let min_age = ctx.policy.min_release_age.duration();

    let outdated = match npm_outdated_global() {
        Ok(outdated) => outdated,
        Err(err) => {
            emit_npm_manager_error(format!("failed to query outdated npm packages: {err}"));
            return Ok(());
        }
    };

    if outdated.is_empty() {
        return Ok(());
    }

    let now = now_unix_secs()?;

    let plan = resolve_npm_plan(&outdated, now, min_age, ctx.max_parallel_checks)?;

    let _upgradable = emit_plan_and_collect_upgradable(
        plan,
        |item| PlanMeta {
            manager: PLUGIN.id(),
            source: PLUGIN.id(),
            name: item.name.clone(),
            current: item.current.clone(),
        },
        |item| {
            let target = match &item.resolved {
                Ok(target) => target,
                Err(err) => return PlanDecision::Error(err.clone()),
            };

            if let Some(selected) = target.selected_version.as_deref() {
                if selected == item.current {
                    return PlanDecision::NoChange;
                }

                return PlanDecision::Update {
                    target: selected.to_string(),
                    delayed_latest: target.delayed_latest(min_age),
                };
            }

            PlanDecision::DelayedNoEligible {
                required_age: human_age(min_age.as_secs()),
                delayed_latest: target.delayed_latest(min_age),
            }
        },
    );

    if ctx.is_dry_run() {
        return Ok(());
    }

    apply_npm_updates(ctx.policy.min_release_age.whole_days());

    Ok(())
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let installed = match npm_installed_global() {
        Ok(installed) => installed,
        Err(err) => {
            emit_npm_manager_error(format!("failed to query installed npm packages: {err}"));
            return Ok(());
        }
    };

    if installed.is_empty() {
        return Ok(());
    }

    let now = verbose_now_unix_secs()?;

    emit_version_scan_outcomes(
        PLUGIN.id(),
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
) -> Result<Vec<NpmPlanItem>> {
    let jobs: Vec<(String, String)> = outdated
        .iter()
        .map(|(name, entry)| (name.clone(), entry.current.clone()))
        .collect();

    let threads = effective_parallelism(max_parallel_checks, NPM_MAX_PARALLEL_CHECKS);
    run_indexed_parallel(jobs, threads, PLUGIN.id(), |(name, current)| {
        let resolved = npm_resolve_target_with_min_age(&name, &current, now_unix_secs, min_age)
            .map_err(|err| err.to_string());

        NpmPlanItem {
            name,
            current,
            resolved,
        }
    })
}

fn apply_npm_updates(min_age_days: u64) {
    if let Err(err) = run_cmd(
        "npm",
        [
            "-g",
            "update",
            "--min-release-age",
            &min_age_days.to_string(),
        ],
        CmdStatus::Success,
    )
    .mutating()
    .output()
    {
        let outcome = ItemOutcome::error(
            PLUGIN.id(),
            "*",
            "*",
            "*",
            PLUGIN.id(),
            REASON_COMMAND_FAILED,
            err.to_string(),
        );
        emit_text_outcome(&outcome);
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

struct NpmResolvedTarget {
    selected_version: Option<String>,
    latest_version: Option<String>,
    latest_age_secs: Option<u64>,
}

impl NpmResolvedTarget {
    fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest> {
        DelayedLatest::from_latest(
            self.latest_version.as_deref(),
            self.latest_age_secs,
            min_age,
        )
    }
}

fn npm_resolve_target_with_min_age(
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<NpmResolvedTarget> {
    let timestamps_by_version: NpmTimeMap =
        run_cmd("npm", ["view", name, "time", "--json"], CmdStatus::Success)
            .output()?
            .json()?;

    let releases = npm_semver_time_releases(name, &timestamps_by_version)?;

    let SemverAgeResolution {
        selected_version,
        latest_version,
        latest_age_secs,
    } = resolve_semver_with_min_age(current, &releases, now_unix_secs, min_age)
        .with_context(|| format!("failed to resolve eligible semver target for {name}"))?;

    Ok(NpmResolvedTarget {
        selected_version,
        latest_version,
        latest_age_secs,
    })
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

fn emit_npm_manager_error(detail: impl AsRef<str>) {
    emit_manager_level_error(PLUGIN.id(), PLUGIN.id(), detail);
}
