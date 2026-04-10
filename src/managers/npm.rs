use crate::Cli;
use crate::manager::Manager;
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

const NPM_MIN_AGE_DAYS: u64 = 7;
const NPM_MAX_PARALLEL_CHECKS: usize = 6;

#[derive(Debug, Deserialize)]
struct OutdatedEntry {
    current: String,
}

struct NpmPlanItem {
    name: String,
    current: String,
    resolved: Result<Option<NpmResolvedTarget>, String>,
}

pub(crate) fn run(cli: &Cli) -> Result<()> {
    let min_age = Duration::from_secs(NPM_MIN_AGE_DAYS * 24 * 60 * 60);

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

    let threads = effective_parallelism(cli.max_parallel_checks, NPM_MAX_PARALLEL_CHECKS);
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
            manager: Manager::Npm,
            source: Manager::Npm.as_str(),
            name: item.name.clone(),
            current: item.current.clone(),
        },
        |item| {
            let target = match &item.resolved {
                Ok(Some(target)) => target,
                Ok(None) => {
                    return PlanDecision::DelayedNoEligible {
                        required_age: format!("{NPM_MIN_AGE_DAYS}d"),
                    };
                }
                Err(err) => return PlanDecision::Error(err.clone()),
            };

            if target.version == item.current {
                return PlanDecision::NoChange;
            }

            let delayed_latest = if let (Some(age_secs), Some(skipped_ver)) = (
                target.skipped_latest_age_secs,
                target.skipped_latest_version.as_deref(),
            ) {
                Some(DelayedLatest {
                    latest_version: skipped_ver.to_string(),
                    latest_age: human_age(age_secs),
                    required_age: human_age(min_age.as_secs()),
                })
            } else {
                None
            };

            PlanDecision::Update {
                target: target.version.clone(),
                delayed_latest,
            }
        },
    );

    if cli.dry_run {
        return Ok(());
    }

    if let Err(err) = run_npm(&["-g", "update", "--min-release-age", "7"]) {
        let outcome = ItemOutcome::error(
            Manager::Npm,
            "*",
            "*",
            "*",
            Manager::Npm.as_str(),
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
        .with_context(|| format!("failed to run {} outdated -g --json", Manager::Npm.as_str()))?;

    // npm outdated returns exit code 1 when outdated packages exist.
    if !output.status.success() && output.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} outdated -g --json failed: {}",
            Manager::Npm.as_str(),
            stderr.trim()
        );
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
    version: String,
    skipped_latest_age_secs: Option<u64>,
    skipped_latest_version: Option<String>,
}

fn npm_resolve_target_with_min_age(
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<Option<NpmResolvedTarget>> {
    let output = Command::new("npm")
        .args(["view", name, "time", "--json"])
        .output()
        .with_context(|| {
            format!(
                "failed to run {} view {name} time --json",
                Manager::Npm.as_str()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} view {name} time --json failed: {}",
            Manager::Npm.as_str(),
            stderr.trim()
        );
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

        let version = match Version::parse(ver_str) {
            Ok(v) => v,
            Err(_) => continue,
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

    let Some((eligible_ver, eligible_str, _eligible_ts)) = eligible else {
        return Ok(None);
    };

    let (skipped_latest_age_secs, skipped_latest_version) =
        if let Some((latest_ver, latest_str, latest_ts)) = newest_any {
            if latest_ver > eligible_ver {
                (
                    Some(now_unix_secs.saturating_sub(latest_ts)),
                    Some(latest_str),
                )
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

    let _ = eligible_str;
    Ok(Some(NpmResolvedTarget {
        version: eligible_ver.to_string(),
        skipped_latest_age_secs,
        skipped_latest_version,
    }))
}

fn run_npm(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("npm");
    command.args(args);
    run_command_checked_stdout(command)
}
