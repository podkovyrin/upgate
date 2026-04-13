use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, PlanDecision, PlanMeta, emit_manager_level_error,
    emit_plan_and_collect_upgradable, emit_scan_current,
};
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::now_unix_secs;
use crate::util::timefmt::human_age;
use crate::util::timeparse::parse_rfc3339_unix;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

const MISE_NPM_AGE_MAX_PARALLEL_CHECKS: usize = 4;

pub(crate) struct MisePlugin;

impl ManagerPlugin for MisePlugin {
    fn id(&self) -> &'static str {
        "mise"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub(crate) static PLUGIN: MisePlugin = MisePlugin;

struct MiseLatestAgeResult {
    age_secs: Result<u64, String>,
}

type NpmTimeMap = BTreeMap<String, String>;

#[derive(Debug, Deserialize)]
struct MiseLsByToolEntry {
    version: Option<String>,
}

type MiseLsJson = BTreeMap<String, Vec<MiseLsByToolEntry>>;

fn run(ctx: &ManagerCtx) -> Result<()> {
    if ctx.is_scan() {
        return scan(ctx);
    }

    let min_age_raw = ctx.policy.min_release_age.cli_arg();
    let min_age = ctx.policy.min_release_age.duration();

    let (plan_pairs, latest_map) = match collect_mise_plan_inputs(min_age_raw) {
        Ok(values) => values,
        Err(err) => {
            emit_mise_manager_error(err.to_string());
            return Ok(());
        }
    };

    let now = now_unix_secs()?;
    let (mut age_by_index, npm_age_annotations_enabled) =
        resolve_mise_age_annotations(&plan_pairs, &latest_map, now, ctx.max_parallel_checks);

    let _upgradable = emit_plan_and_collect_upgradable(
        plan_pairs.into_iter().enumerate().collect(),
        |(_idx, item)| PlanMeta {
            manager: PLUGIN.id(),
            source: PLUGIN.id(),
            name: item.tool.clone(),
            current: item.from_version.clone(),
        },
        |(idx, item)| {
            mise_plan_decision(
                *idx,
                item,
                &latest_map,
                &mut age_by_index,
                npm_age_annotations_enabled,
                min_age,
            )
        },
    );

    if ctx.is_dry_run() {
        return Ok(());
    }

    if let Err(err) = run_cmd(
        "mise",
        ["upgrade", "--before", min_age_raw],
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
    Ok(())
}

fn collect_mise_plan_inputs(before: &str) -> Result<(Vec<MisePlanItem>, BTreeMap<String, String>)> {
    let planned = mise_upgrade_dry_run_with_before(before)
        .with_context(|| "failed to build mise upgrade plan")?;

    let plan_pairs = build_plan_pairs(&planned);
    let latest_map = match mise_outdated_latest_map() {
        Ok(map) => map,
        Err(err) => {
            emit_mise_manager_error(format!("failed to fetch latest version map: {err}"));
            BTreeMap::new()
        }
    };

    Ok((plan_pairs, latest_map))
}

fn resolve_mise_age_annotations(
    plan_pairs: &[MisePlanItem],
    latest_map: &BTreeMap<String, String>,
    now_unix_secs: u64,
    max_parallel_checks: usize,
) -> (BTreeMap<usize, MiseLatestAgeResult>, bool) {
    let mut age_jobs: Vec<(usize, String, String)> = Vec::new();
    for (idx, item) in plan_pairs.iter().enumerate() {
        if let Some(latest) = latest_map.get(&item.tool)
            && latest != &item.to_version
            && item.tool.starts_with("npm:")
        {
            age_jobs.push((idx, item.tool.clone(), latest.clone()));
        }
    }

    let threads = effective_parallelism(max_parallel_checks, MISE_NPM_AGE_MAX_PARALLEL_CHECKS);
    let age_results_indexed: Vec<(usize, MiseLatestAgeResult)> =
        match run_indexed_parallel(age_jobs, threads, PLUGIN.id(), |(idx, tool, latest)| {
            let age_secs =
                npm_latest_age_secs(&tool, &latest, now_unix_secs).map_err(|err| err.to_string());
            (idx, MiseLatestAgeResult { age_secs })
        }) {
            Ok(results) => results,
            Err(err) => {
                emit_mise_manager_error(format!(
                    "npm delayed-latest age enrichment is unavailable: {err}"
                ));
                return (BTreeMap::new(), false);
            }
        };

    let mut age_by_index: BTreeMap<usize, MiseLatestAgeResult> = BTreeMap::new();
    for (idx, age_result) in age_results_indexed {
        age_by_index.insert(idx, age_result);
    }

    (age_by_index, true)
}

fn mise_plan_decision(
    idx: usize,
    item: &MisePlanItem,
    latest_map: &BTreeMap<String, String>,
    age_by_index: &mut BTreeMap<usize, MiseLatestAgeResult>,
    npm_age_annotations_enabled: bool,
    min_age: Duration,
) -> PlanDecision {
    let Some(latest) = latest_map.get(&item.tool) else {
        return PlanDecision::Update {
            target: item.to_version.clone(),
            delayed_latest: None,
        };
    };

    if latest == &item.to_version {
        return PlanDecision::Update {
            target: item.to_version.clone(),
            delayed_latest: None,
        };
    }

    let age_secs = if item.tool.starts_with("npm:") {
        if npm_age_annotations_enabled {
            match age_by_index.remove(&idx) {
                Some(age_result) => match age_result.age_secs {
                    Ok(age_secs) => age_secs,
                    Err(err) => return PlanDecision::Error(err),
                },
                None => 0,
            }
        } else {
            0
        }
    } else {
        0
    };

    PlanDecision::Update {
        target: item.to_version.clone(),
        delayed_latest: Some(DelayedLatest {
            latest_version: latest.clone(),
            latest_age: human_age(age_secs),
            required_age: human_age(min_age.as_secs()),
        }),
    }
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
    let output = run_cmd(
        "mise",
        ["upgrade", "--dry-run", "--before", before],
        CmdStatus::Success,
    )
    .output()?;
    let text = output.stdout()?;
    Ok(text.lines().map(str::to_string).collect())
}

#[derive(Debug, serde::Deserialize)]
struct MiseOutdatedItem {
    latest: String,
}

fn mise_outdated_latest_map() -> Result<BTreeMap<String, String>> {
    let parsed: BTreeMap<String, MiseOutdatedItem> =
        run_cmd("mise", ["outdated", "--json"], CmdStatus::Success)
            .output()?
            .json()?;

    Ok(parsed.into_iter().map(|(k, v)| (k, v.latest)).collect())
}

fn npm_latest_age_secs(tool: &str, latest: &str, now_unix_secs: u64) -> Result<u64> {
    let pkg = tool.trim_start_matches("npm:");
    let spec = format!("{pkg}@{latest}");
    let timestamps_by_version: NpmTimeMap =
        run_cmd("npm", ["view", &spec, "time", "--json"], CmdStatus::Success)
            .output()?
            .json()?;

    let ts_raw = timestamps_by_version
        .get(latest)
        .with_context(|| format!("npm view time missing timestamp for {spec}"))?;

    let ts = parse_rfc3339_unix(ts_raw)
        .with_context(|| format!("invalid RFC3339 timestamp for {spec}: {ts_raw}"))?;

    Ok(now_unix_secs.saturating_sub(ts))
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let installed = match mise_installed_versions() {
        Ok(installed) => installed,
        Err(err) => {
            emit_mise_manager_error(format!("failed to read installed mise tools: {err}"));
            return Ok(());
        }
    };

    if installed.is_empty() {
        return Ok(());
    }

    let now = if crate::ui::output_theme().verbose {
        Some(now_unix_secs()?)
    } else {
        None
    };

    emit_mise_scan_outcomes(installed, now, ctx.scan_old_age_threshold);
    Ok(())
}

fn emit_mise_scan_outcomes(
    installed: BTreeMap<String, String>,
    now_unix_secs: Option<u64>,
    old_threshold: Duration,
) {
    for (tool, version) in installed {
        let age_secs = if let Some(now_unix_secs) = now_unix_secs {
            if tool.starts_with("npm:") {
                npm_latest_age_secs(&tool, &version, now_unix_secs).ok()
            } else {
                None
            }
        } else {
            None
        };

        emit_scan_current(
            PLUGIN.id(),
            PLUGIN.id(),
            tool,
            version,
            age_secs,
            old_threshold,
        );
    }
}

fn mise_installed_versions() -> Result<BTreeMap<String, String>> {
    let parsed: MiseLsJson = run_cmd("mise", ["ls", "--json"], CmdStatus::Success)
        .output()?
        .json()?;

    let mut out = BTreeMap::new();
    for (tool, entries) in parsed {
        for entry in entries {
            let Some(version) = entry.version else {
                continue;
            };

            out.insert(tool.clone(), version);
        }
    }

    Ok(out)
}

fn emit_mise_manager_error(detail: impl AsRef<str>) {
    emit_manager_level_error(PLUGIN.id(), PLUGIN.id(), detail);
}
