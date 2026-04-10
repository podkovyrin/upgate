use crate::Cli;
use crate::manager::Manager;
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::durationparse::parse_duration;
use crate::util::process::run_command_checked_stdout;
use crate::util::timefmt::human_age;
use crate::util::timeparse::parse_rfc3339_unix;
use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const MISE_DELAY: &str = "7d";
const MISE_NPM_AGE_MAX_PARALLEL_CHECKS: usize = 4;

struct MiseLatestAgeResult {
    age_secs: Result<u64, String>,
}

pub(crate) fn run(cli: &Cli) -> Result<()> {
    let planned = mise_upgrade_dry_run_with_before(MISE_DELAY)?;
    let plan_pairs = build_plan_pairs(&planned);
    let latest_map = match mise_outdated_latest_map() {
        Ok(map) => map,
        Err(err) => {
            let outcome = ItemOutcome::error(
                Manager::Mise,
                "*",
                "*",
                "*",
                Manager::Mise.as_str(),
                REASON_COMMAND_FAILED,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
            BTreeMap::new()
        }
    };

    let min_age = parse_duration(MISE_DELAY)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX_EPOCH")?
        .as_secs();

    let mut age_jobs: Vec<(usize, String, String)> = Vec::new();
    for (idx, item) in plan_pairs.iter().enumerate() {
        if let Some(latest) = latest_map.get(&item.tool)
            && latest != &item.to_version
            && item.tool.starts_with("npm:")
        {
            age_jobs.push((idx, item.tool.clone(), latest.clone()));
        }
    }

    let effective_parallelism = cli
        .max_parallel_checks
        .clamp(1, MISE_NPM_AGE_MAX_PARALLEL_CHECKS);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(effective_parallelism)
        .build()
        .context("failed to build mise npm-age planning thread pool")?;

    let age_results_indexed: Vec<(usize, MiseLatestAgeResult)> = pool.install(|| {
        age_jobs
            .into_par_iter()
            .map(|(idx, tool, latest)| {
                let age_secs =
                    npm_latest_age_secs(&tool, &latest, now).map_err(|err| err.to_string());
                (idx, MiseLatestAgeResult { age_secs })
            })
            .collect()
    });

    let mut age_by_index: BTreeMap<usize, MiseLatestAgeResult> = BTreeMap::new();
    for (idx, age_result) in age_results_indexed {
        age_by_index.insert(idx, age_result);
    }

    for (idx, item) in plan_pairs.iter().enumerate() {
        if let Some(latest) = latest_map.get(&item.tool)
            && latest != &item.to_version
        {
            let age_secs = if item.tool.starts_with("npm:") {
                if let Some(age_result) = age_by_index.remove(&idx) {
                    match age_result.age_secs {
                        Ok(age_secs) => age_secs,
                        Err(err) => {
                            let outcome = ItemOutcome::error(
                                Manager::Mise,
                                item.tool.clone(),
                                item.from_version.clone(),
                                item.to_version.clone(),
                                Manager::Mise.as_str(),
                                REASON_COMMAND_FAILED,
                                err,
                            );
                            emit_text_outcome(&outcome);
                            continue;
                        }
                    }
                } else {
                    0
                }
            } else {
                0
            };

            let outcome = ItemOutcome::update_with_delayed_latest(
                Manager::Mise,
                item.tool.clone(),
                item.from_version.clone(),
                item.to_version.clone(),
                Manager::Mise.as_str(),
                latest.clone(),
                human_age(age_secs),
                human_age(min_age.as_secs()),
            );
            emit_text_outcome(&outcome);
            continue;
        }

        let outcome = ItemOutcome::update(
            Manager::Mise,
            item.tool.clone(),
            item.from_version.clone(),
            item.to_version.clone(),
            Manager::Mise.as_str(),
        );
        emit_text_outcome(&outcome);
    }

    if cli.dry_run {
        return Ok(());
    }

    if let Err(err) = run_mise(&["upgrade", "--before", MISE_DELAY]) {
        let outcome = ItemOutcome::error(
            Manager::Mise,
            "*",
            "*",
            "*",
            Manager::Mise.as_str(),
            REASON_COMMAND_FAILED,
            err.to_string(),
        );
        emit_text_outcome(&outcome);
    }
    Ok(())
}

struct MisePlanItem {
    tool: String,
    from_version: String,
    to_version: String,
}

fn build_plan_pairs(lines: &[String]) -> Vec<MisePlanItem> {
    let mut old_versions: BTreeMap<String, String> = BTreeMap::new();
    let mut result = Vec::new();

    for line in lines {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("Would uninstall ") {
            if let Some((tool, from_ver)) = split_tool_and_version(rest) {
                old_versions.insert(tool.to_string(), from_ver.to_string());
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("Would install ")
            && let Some((tool, to_ver)) = split_tool_and_version(rest)
        {
            let from = old_versions
                .get(tool)
                .cloned()
                .unwrap_or_else(|| "?".to_string());
            result.push(MisePlanItem {
                tool: tool.to_string(),
                from_version: from,
                to_version: to_ver.to_string(),
            });
        }
    }

    result
}

fn split_tool_and_version(input: &str) -> Option<(&str, &str)> {
    let idx = input.rfind('@')?;
    let (tool, ver) = input.split_at(idx);
    Some((tool, ver.strip_prefix('@')?))
}

fn mise_upgrade_dry_run_with_before(before: &str) -> Result<Vec<String>> {
    let output = Command::new("mise")
        .args(["upgrade", "--dry-run", "--before", before])
        .output()
        .with_context(|| {
            format!(
                "failed to run {} upgrade --dry-run --before {before}",
                Manager::Mise.as_str()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} upgrade --dry-run --before {before} failed: {}",
            Manager::Mise.as_str(),
            stderr.trim()
        );
    }

    let stdout = String::from_utf8(output.stdout).context("mise dry-run output not UTF-8")?;
    Ok(stdout.lines().map(str::to_string).collect())
}

#[derive(Debug, serde::Deserialize)]
struct MiseOutdatedItem {
    latest: String,
}

fn mise_outdated_latest_map() -> Result<BTreeMap<String, String>> {
    let output = Command::new("mise")
        .args(["outdated", "--json"])
        .output()
        .with_context(|| format!("failed to run {} outdated --json", Manager::Mise.as_str()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} outdated --json failed: {}",
            Manager::Mise.as_str(),
            stderr.trim()
        );
    }

    let stdout = String::from_utf8(output.stdout).context("mise outdated output not UTF-8")?;
    let parsed: BTreeMap<String, MiseOutdatedItem> =
        serde_json::from_str(&stdout).context("failed to parse mise outdated JSON")?;

    Ok(parsed.into_iter().map(|(k, v)| (k, v.latest)).collect())
}

fn npm_latest_age_secs(tool: &str, latest: &str, now_unix_secs: u64) -> Result<u64> {
    let pkg = tool.trim_start_matches("npm:");
    let spec = format!("{pkg}@{latest}");
    let output = Command::new("npm")
        .args(["view", &spec, "time", "--json"])
        .output()
        .with_context(|| {
            format!(
                "failed to run {} view {spec} time --json",
                Manager::Npm.as_str()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} view {spec} time --json failed: {}",
            Manager::Npm.as_str(),
            stderr.trim()
        );
    }

    let stdout = String::from_utf8(output.stdout).context("npm view output not UTF-8")?;
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .with_context(|| format!("failed to parse npm view JSON for {spec}"))?;

    let ts_raw = val
        .get(latest)
        .and_then(serde_json::Value::as_str)
        .with_context(|| format!("npm view time missing timestamp for {spec}"))?;

    let ts = parse_rfc3339_unix(ts_raw)
        .with_context(|| format!("invalid RFC3339 timestamp for {spec}: {ts_raw}"))?;

    Ok(now_unix_secs.saturating_sub(ts))
}

fn run_mise(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("mise");
    command.args(args);
    run_command_checked_stdout(command)
}
