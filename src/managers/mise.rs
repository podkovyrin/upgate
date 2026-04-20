use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

#[allow(clippy::wildcard_imports)]
use crate::managers::*;
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::{now_unix_secs, parse_rfc3339_unix};

const MISE_NPM_AGE_MAX_PARALLEL_CHECKS: usize = 4;

pub struct MisePlugin;

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

pub static PLUGIN: MisePlugin = MisePlugin;

#[derive(Clone)]
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
    run_manager_pipeline(ctx, scan, run_plan_apply)
}

fn run_plan_apply(ctx: &ManagerCtx) -> Result<()> {
    let min_age_raw = ctx.policy.min_release_age.cli_arg().to_string();

    run_plan_apply_framework(
        ctx,
        PLUGIN.id(),
        PlanApplyFrameworkPolicy::SOFT_FETCH_STRICT_RESOLVE,
        || collect_mise_plan_inputs(&min_age_raw),
        |(plan_pairs, _latest_map)| plan_pairs.is_empty(),
        |(plan_pairs, latest_map), runtime| {
            let (age_by_index, npm_age_annotations_enabled) = resolve_mise_age_annotations(
                plan_pairs,
                latest_map,
                runtime.now_unix_secs,
                runtime.max_parallel_checks,
            );

            Ok(MiseResolved {
                plan_pairs: plan_pairs.clone(),
                latest_map: latest_map.clone(),
                age_by_index,
                npm_age_annotations_enabled,
            })
        },
        |_discovered, resolved, runtime| {
            let mut age_by_index = resolved.age_by_index;
            Ok(collect_upgradable_from_plan(
                resolved.plan_pairs.into_iter().enumerate().collect(),
                |(idx, item)| {
                    let decision = mise_plan_decision(
                        idx,
                        &item.tool,
                        item.to_version,
                        &resolved.latest_map,
                        &mut age_by_index,
                        resolved.npm_age_annotations_enabled,
                        runtime.min_age,
                    );

                    (
                        PlanMeta {
                            manager: PLUGIN.id(),
                            name: item.tool,
                            current: item.from_version,
                        },
                        decision,
                    )
                },
                runtime.suppress_update_outcomes,
                runtime.pinned,
            ))
        },
        |ctx, _discovered, upgradable| {
            run_selective_or_global_apply_flow(
                ctx,
                PLUGIN.id(),
                upgradable,
                |selected| apply_mise_selected_updates(&min_age_raw, selected),
                || apply_mise_updates(&min_age_raw),
            )
        },
    )
}

fn apply_mise_updates(min_age_raw: &str) -> Result<()> {
    run_cmd(
        "mise",
        ["upgrade", "--before", min_age_raw],
        CmdStatus::Success,
    )
    .mutating()
    .output()?;

    Ok(())
}

fn apply_mise_selected_updates(min_age_raw: &str, upgradable: Vec<crate::managers::PlannedUpdate>) {
    for item in upgradable {
        let name = item.name;
        let current = item.current;
        let target = item.target;

        let args = [
            "upgrade".to_string(),
            "--before".to_string(),
            min_age_raw.to_string(),
            name.clone(),
        ];

        if let Err(err) = run_cmd("mise", &args, CmdStatus::Success)
            .mutating()
            .output()
        {
            emit_apply_error(PLUGIN.id(), name, current, target, err);
        }
    }
}

fn collect_mise_plan_inputs(before: &str) -> Result<(Vec<MisePlanItem>, BTreeMap<String, String>)> {
    let planned = mise_upgrade_dry_run_with_before(before)
        .with_context(|| "failed to build mise upgrade plan")?;

    let plan_pairs = build_plan_pairs(&planned);
    let latest_map = soft_fail_or(
        mise_outdated_latest_map(),
        BTreeMap::new,
        PLUGIN.id(),
        "failed to fetch latest version map",
    );

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
                emit_manager_level_error_with(
                    PLUGIN.id(),
                    "npm delayed-latest age enrichment is unavailable",
                    err,
                );
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
    tool: &str,
    to_version: String,
    latest_map: &BTreeMap<String, String>,
    age_by_index: &mut BTreeMap<usize, MiseLatestAgeResult>,
    npm_age_annotations_enabled: bool,
    min_age: Duration,
) -> PlanDecision {
    let Some(latest) = latest_map.get(tool) else {
        return PlanDecision::Update {
            target: to_version,
            delayed_latest: None,
        };
    };

    if latest == &to_version {
        return PlanDecision::Update {
            target: to_version,
            delayed_latest: None,
        };
    }

    let delayed_latest = if tool.starts_with("npm:") && npm_age_annotations_enabled {
        match age_by_index.remove(&idx) {
            Some(age_result) => match age_result.age_secs {
                Ok(age_secs) => Some(DelayedLatest::new(latest.clone(), age_secs, min_age)),
                Err(err) => return PlanDecision::Error(err),
            },
            None => None,
        }
    } else {
        None
    };

    PlanDecision::Update {
        target: to_version,
        delayed_latest,
    }
}

#[derive(Clone)]
struct MisePlanItem {
    tool: String,
    from_version: String,
    to_version: String,
}

struct MiseResolved {
    plan_pairs: Vec<MisePlanItem>,
    latest_map: BTreeMap<String, String>,
    age_by_index: BTreeMap<usize, MiseLatestAgeResult>,
    npm_age_annotations_enabled: bool,
}

fn build_plan_pairs<S: AsRef<str>>(lines: &[S]) -> Vec<MisePlanItem> {
    let mut old_versions: BTreeMap<String, String> = BTreeMap::new();
    let mut result = Vec::new();

    for line in lines {
        let trimmed = line.as_ref().trim();

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
    let Some(installed) = soft_fail(
        mise_installed_versions(),
        PLUGIN.id(),
        "failed to read installed mise tools",
    ) else {
        return Ok(());
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
        let age_secs = now_unix_secs.and_then(|now_unix_secs| {
            if tool.starts_with("npm:") {
                npm_latest_age_secs(&tool, &version, now_unix_secs).ok()
            } else {
                None
            }
        });

        emit_scan_current(PLUGIN.id(), tool, version, age_secs, old_threshold);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_npm_delayed_latest_annotation_is_omitted() {
        let mut age_by_index = BTreeMap::new();
        let mut latest_map = BTreeMap::new();
        latest_map.insert("node".to_string(), "22.0.0".to_string());

        let decision = mise_plan_decision(
            0,
            "node",
            "21.0.0".to_string(),
            &latest_map,
            &mut age_by_index,
            true,
            Duration::from_secs(7 * 24 * 60 * 60),
        );

        match decision {
            PlanDecision::Update {
                target,
                delayed_latest,
            } => {
                assert_eq!(target, "21.0.0");
                assert!(delayed_latest.is_none());
            }
            _ => panic!("expected update decision"),
        }
    }

    #[test]
    fn npm_delayed_latest_annotation_is_omitted_when_age_lookup_missing() {
        let mut age_by_index = BTreeMap::new();
        let mut latest_map = BTreeMap::new();
        latest_map.insert("npm:eslint".to_string(), "9.0.0".to_string());

        let decision = mise_plan_decision(
            0,
            "npm:eslint",
            "8.0.0".to_string(),
            &latest_map,
            &mut age_by_index,
            true,
            Duration::from_secs(7 * 24 * 60 * 60),
        );

        match decision {
            PlanDecision::Update {
                target,
                delayed_latest,
            } => {
                assert_eq!(target, "8.0.0");
                assert!(delayed_latest.is_none());
            }
            _ => panic!("expected update decision"),
        }
    }

    #[test]
    fn npm_delayed_latest_annotation_emits_error_on_age_lookup_failure() {
        let mut age_by_index = BTreeMap::new();
        age_by_index.insert(
            0,
            MiseLatestAgeResult {
                age_secs: Err("lookup failed".to_string()),
            },
        );
        let mut latest_map = BTreeMap::new();
        latest_map.insert("npm:eslint".to_string(), "9.0.0".to_string());

        let decision = mise_plan_decision(
            0,
            "npm:eslint",
            "8.0.0".to_string(),
            &latest_map,
            &mut age_by_index,
            true,
            Duration::from_secs(7 * 24 * 60 * 60),
        );

        match decision {
            PlanDecision::Error(err) => assert_eq!(err, "lookup failed"),
            _ => panic!("expected error decision"),
        }
    }
}
