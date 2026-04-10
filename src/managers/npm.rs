use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, PlanDecision, PlanMeta, emit_plan_and_collect_upgradable,
};
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::run_command_checked_stdout;
use crate::util::timefmt::human_age;
use crate::util::timeparse::parse_rfc3339_unix;
use anyhow::{Context, Result, bail};
use semver::Version;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

struct NpmPlanItem {
    name: String,
    current: String,
    resolved: Result<NpmResolvedTarget, String>,
}

fn run(ctx: &ManagerCtx) -> Result<()> {
    let min_age = ctx.policy.min_release_age.duration();

    let outdated = npm_outdated_global()?;
    if outdated.is_empty() {
        return Ok(());
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX_EPOCH")?
        .as_secs();

    let jobs: Vec<(String, String)> = outdated
        .iter()
        .map(|(name, entry)| (name.clone(), entry.current.clone()))
        .collect();

    let threads = effective_parallelism(ctx.max_parallel_checks, NPM_MAX_PARALLEL_CHECKS);
    let plan: Vec<NpmPlanItem> = run_indexed_parallel(
        jobs,
        threads,
        "failed to build npm planning thread pool",
        "internal error: missing npm plan slot",
        |(name, current)| {
            let resolved = npm_resolve_target_with_min_age(&name, &current, now, min_age)
                .map_err(|err| err.to_string());

            NpmPlanItem {
                name,
                current,
                resolved,
            }
        },
    )?;

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

    let min_age_days = ctx.policy.min_release_age.whole_days();

    if let Err(err) = run_npm(&[
        "-g",
        "update",
        "--min-release-age",
        &min_age_days.to_string(),
    ]) {
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

    Ok(())
}

fn npm_outdated_global() -> Result<BTreeMap<String, OutdatedEntry>> {
    let output = Command::new("npm")
        .args(["outdated", "-g", "--json"])
        .output()
        .with_context(|| "failed to run npm outdated -g --json")?;

    // npm outdated returns exit code 1 when outdated packages exist.
    if !output.status.success() && output.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("npm outdated -g --json failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout).context("npm outdated output not UTF-8")?;
    if stdout.trim().is_empty() {
        return Ok(BTreeMap::new());
    }

    let parsed: BTreeMap<String, OutdatedEntry> =
        serde_json::from_str(&stdout).context("failed to parse npm outdated JSON")?;

    Ok(parsed)
}

struct NpmResolvedTarget {
    selected_version: Option<String>,
    latest_version: Option<String>,
    latest_age_secs: Option<u64>,
}

impl NpmResolvedTarget {
    fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest> {
        let (Some(latest_version), Some(latest_age_secs)) =
            (self.latest_version.as_deref(), self.latest_age_secs)
        else {
            return None;
        };

        Some(DelayedLatest {
            latest_version: latest_version.to_string(),
            latest_age: human_age(latest_age_secs),
            required_age: human_age(min_age.as_secs()),
        })
    }
}

fn npm_resolve_target_with_min_age(
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<NpmResolvedTarget> {
    let output = Command::new("npm")
        .args(["view", name, "time", "--json"])
        .output()
        .with_context(|| format!("failed to run npm view {name} time --json"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("npm view {name} time --json failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout).context("npm view output not UTF-8")?;
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .with_context(|| format!("failed to parse npm view JSON for {name}"))?;

    let obj = val
        .as_object()
        .with_context(|| format!("npm view time JSON is not an object for {name}"))?;

    let current_ver = Version::parse(current)
        .with_context(|| format!("failed to parse current semver for {name}: {current}"))?;

    let mut eligible: Option<(Version, String, u64)> = None;
    let mut newest_any: Option<(Version, String, u64)> = None;

    for (ver_str, ts_val) in obj {
        if ver_str == "created" || ver_str == "modified" {
            continue;
        }

        let Some(ts_raw) = ts_val.as_str() else {
            continue;
        };

        let Ok(version) = Version::parse(ver_str) else {
            continue;
        };

        let ts = parse_rfc3339_unix(ts_raw)
            .with_context(|| format!("invalid npm timestamp for {name}@{ver_str}: {ts_raw}"))?;

        if newest_any
            .as_ref()
            .is_none_or(|(curr, _, _)| version > *curr)
        {
            newest_any = Some((version.clone(), ver_str.clone(), ts));
        }

        if version >= current_ver {
            let age_secs = now_unix_secs.saturating_sub(ts);
            if age_secs >= min_age.as_secs()
                && eligible.as_ref().is_none_or(|(curr, _, _)| version > *curr)
            {
                eligible = Some((version, ver_str.clone(), ts));
            }
        }
    }

    let selected_version = eligible.map(|(ver, _, _)| ver.to_string());
    let (latest_version, latest_age_secs) =
        if let Some((_latest_ver, latest_str, latest_ts)) = newest_any {
            (
                Some(latest_str),
                Some(now_unix_secs.saturating_sub(latest_ts)),
            )
        } else {
            (None, None)
        };

    Ok(NpmResolvedTarget {
        selected_version,
        latest_version,
        latest_age_secs,
    })
}

fn run_npm(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("npm");
    command.args(args);
    run_command_checked_stdout(command)
}
