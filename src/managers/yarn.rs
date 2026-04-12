use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, PlanDecision, PlanMeta, SemverAgeResolution, SemverTimestamp,
    emit_manager_level_error, emit_plan_and_collect_upgradable, emit_version_scan_outcomes,
    parse_semver_time_releases, release_age_secs_for_version, resolve_semver_with_min_age,
    verbose_now_unix_secs,
};
use crate::outcome::{
    ItemOutcome, REASON_COMMAND_FAILED, REASON_MISSING_METADATA, emit_text_outcome,
};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::now_unix_secs;
use crate::util::timefmt::human_age;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

const YARN_MAX_PARALLEL_CHECKS: usize = 6;

pub(crate) struct YarnPlugin;

impl ManagerPlugin for YarnPlugin {
    fn id(&self) -> &'static str {
        "yarn"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub(crate) static PLUGIN: YarnPlugin = YarnPlugin;

#[derive(Debug)]
struct InstalledEntry {
    current: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum YarnInfoJsonLine {
    #[serde(rename = "inspect")]
    Inspect { data: BTreeMap<String, String> },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct YarnListTreeData {
    trees: Vec<YarnListTreeNode>,
}

#[derive(Debug, Deserialize)]
struct YarnListTreeNode {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum YarnGlobalListJsonLine {
    #[serde(rename = "tree")]
    Tree { data: YarnListTreeData },
    #[serde(other)]
    Other,
}

type YarnTimeMap = BTreeMap<String, String>;

struct YarnPlanItem {
    name: String,
    current: String,
    resolved: Result<YarnResolvedTarget, String>,
}

fn run(ctx: &ManagerCtx) -> Result<()> {
    if ctx.is_scan() {
        return scan(ctx);
    }

    let min_age = ctx.policy.min_release_age.duration();

    let yarn_major = match yarn_major_version() {
        Ok(v) => v,
        Err(err) => {
            emit_yarn_manager_error(format!("failed to detect Yarn major version: {err}"));
            return Ok(());
        }
    };

    if yarn_major >= 2 {
        let outcome = ItemOutcome::skipped(
            PLUGIN.id(),
            "*",
            "*",
            "*",
            PLUGIN.id(),
            REASON_MISSING_METADATA,
            "global upgrades are not supported for Yarn 2+; skipping manager",
        );
        emit_text_outcome(&outcome);
        return Ok(());
    }

    let installed = match yarn_global_installed() {
        Ok(installed) => installed,
        Err(err) => {
            emit_yarn_manager_error(format!("failed to query global Yarn packages: {err}"));
            return Ok(());
        }
    };

    if installed.is_empty() {
        return Ok(());
    }

    let now = now_unix_secs()?;

    let plan = match resolve_yarn_plan(&installed, now, min_age, ctx.max_parallel_checks) {
        Ok(plan) => plan,
        Err(err) => {
            emit_yarn_manager_error(format!("planning execution failed: {err}"));
            return Ok(());
        }
    };

    let upgradable = emit_plan_and_collect_upgradable(
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

    apply_yarn_updates(upgradable);

    Ok(())
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let yarn_major = match yarn_major_version() {
        Ok(v) => v,
        Err(err) => {
            emit_yarn_manager_error(format!("failed to detect Yarn major version: {err}"));
            return Ok(());
        }
    };

    if yarn_major >= 2 {
        let outcome = ItemOutcome::skipped(
            PLUGIN.id(),
            "*",
            "*",
            "*",
            PLUGIN.id(),
            REASON_MISSING_METADATA,
            "global upgrades are not supported for Yarn 2+; skipping manager",
        );
        emit_text_outcome(&outcome);
        return Ok(());
    }

    let installed = match yarn_global_installed() {
        Ok(installed) => installed,
        Err(err) => {
            emit_yarn_manager_error(format!("failed to query global Yarn packages: {err}"));
            return Ok(());
        }
    };

    if installed.is_empty() {
        return Ok(());
    }

    let now = verbose_now_unix_secs()?;

    let items = installed
        .into_iter()
        .map(|(name, entry)| (name, entry.current));
    emit_version_scan_outcomes(
        PLUGIN.id(),
        PLUGIN.id(),
        items,
        now,
        ctx.scan_old_age_threshold,
        yarn_release_age_secs,
    );

    Ok(())
}

fn resolve_yarn_plan(
    installed: &BTreeMap<String, InstalledEntry>,
    now_unix_secs: u64,
    min_age: Duration,
    max_parallel_checks: usize,
) -> Result<Vec<YarnPlanItem>> {
    let jobs: Vec<(String, String)> = installed
        .iter()
        .map(|(name, entry)| (name.clone(), entry.current.clone()))
        .collect();

    let threads = effective_parallelism(max_parallel_checks, YARN_MAX_PARALLEL_CHECKS);
    run_indexed_parallel(
        jobs,
        threads,
        "failed to build yarn planning thread pool",
        "internal error: missing yarn plan slot",
        |(name, current)| {
            let resolved =
                yarn_resolve_target_with_min_age(&name, &current, now_unix_secs, min_age)
                    .map_err(|err| err.to_string());

            YarnPlanItem {
                name,
                current,
                resolved,
            }
        },
    )
}

fn apply_yarn_updates(upgradable: Vec<(String, String, String)>) {
    for (name, current, version) in upgradable {
        let spec = format!("{name}@{version}");
        if let Err(err) = run_cmd("yarn", ["global", "add", &spec], CmdStatus::Success) {
            let outcome = ItemOutcome::error(
                PLUGIN.id(),
                name,
                current,
                version,
                PLUGIN.id(),
                REASON_COMMAND_FAILED,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
        }
    }
}

fn yarn_major_version() -> Result<u64> {
    let output = run_cmd("yarn", ["--version"], CmdStatus::Success)?;
    let text = output.stdout()?;

    parse_yarn_major_version(text)
        .with_context(|| format!("failed to parse yarn major version from '{}'", text.trim()))
}

fn parse_yarn_major_version(text: &str) -> Option<u64> {
    if text.is_empty() {
        return None;
    }

    let first_token = text.split_whitespace().next()?;
    let trimmed = first_token.strip_prefix('v').unwrap_or(first_token);
    let major = trimmed.split('.').next()?;
    major.parse::<u64>().ok()
}

fn yarn_global_installed() -> Result<BTreeMap<String, InstalledEntry>> {
    let output = run_cmd(
        "yarn",
        ["global", "list", "--depth=0", "--json"],
        CmdStatus::Success,
    )?;
    let text = output.stdout()?;

    Ok(parse_yarn_global_list(text))
}

fn parse_yarn_global_list(text: &str) -> BTreeMap<String, InstalledEntry> {
    let mut out = BTreeMap::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parsed: YarnGlobalListJsonLine = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let YarnGlobalListJsonLine::Tree { data } = parsed else {
            continue;
        };

        for node in data.trees {
            let Some((name, version)) = parse_yarn_package_spec(&node.name) else {
                continue;
            };

            out.insert(
                name.to_string(),
                InstalledEntry {
                    current: version.to_string(),
                },
            );
        }
    }

    out
}

fn parse_yarn_package_spec(spec: &str) -> Option<(&str, &str)> {
    let (name, version) = spec.rsplit_once('@')?;
    if name.is_empty() || version.is_empty() {
        return None;
    }

    Some((name, version))
}

struct YarnResolvedTarget {
    selected_version: Option<String>,
    latest_version: Option<String>,
    latest_age_secs: Option<u64>,
}

impl YarnResolvedTarget {
    fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest> {
        DelayedLatest::from_latest(
            self.latest_version.as_deref(),
            self.latest_age_secs,
            min_age,
        )
    }
}

fn yarn_resolve_target_with_min_age(
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<YarnResolvedTarget> {
    let output = run_cmd("yarn", ["info", name, "time", "--json"], CmdStatus::Success)?;
    let text = output.stdout()?;

    let obj = parse_yarn_inspect_object(text, "time")?;
    let releases = yarn_semver_time_releases(name, &obj)?;

    let SemverAgeResolution {
        selected_version,
        latest_version,
        latest_age_secs,
    } = resolve_semver_with_min_age(current, &releases, now_unix_secs, min_age)
        .with_context(|| format!("failed to resolve eligible semver target for {name}"))?;

    Ok(YarnResolvedTarget {
        selected_version,
        latest_version,
        latest_age_secs,
    })
}

fn parse_yarn_inspect_object(text: &str, field: &str) -> Result<YarnTimeMap> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parsed: YarnInfoJsonLine = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        match parsed {
            YarnInfoJsonLine::Inspect { data } => return Ok(data),
            YarnInfoJsonLine::Other => continue,
        }
    }

    bail!("failed to parse yarn {field} JSON payload")
}

fn yarn_release_age_secs(name: &str, version: &str, now_unix_secs: u64) -> Result<Option<u64>> {
    let output = run_cmd("yarn", ["info", name, "time", "--json"], CmdStatus::Success)?;
    let text = output.stdout()?;
    let obj = parse_yarn_inspect_object(text, "time")?;
    let releases = yarn_semver_time_releases(name, &obj)?;

    Ok(release_age_secs_for_version(
        &releases,
        version,
        now_unix_secs,
    ))
}

fn yarn_semver_time_releases(
    name: &str,
    timestamps_by_version: &YarnTimeMap,
) -> Result<Vec<SemverTimestamp>> {
    parse_semver_time_releases(PLUGIN.id(), name, timestamps_by_version)
}

fn emit_yarn_manager_error(detail: impl AsRef<str>) {
    emit_manager_level_error(PLUGIN.id(), PLUGIN.id(), detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_global_list_with_scoped_package() {
        let raw = r#"{"type":"tree","data":{"type":"list","trees":[{"name":"npm@11.12.0","children":[],"hint":null,"color":"bold","depth":0},{"name":"@scope/tool@2.3.4","children":[],"hint":null,"color":"bold","depth":0}]}}"#;

        let parsed = parse_yarn_global_list(raw);
        assert_eq!(
            parsed.get("npm").map(|e| e.current.as_str()),
            Some("11.12.0")
        );
        assert_eq!(
            parsed.get("@scope/tool").map(|e| e.current.as_str()),
            Some("2.3.4")
        );
    }

    #[test]
    fn parse_inspect_data_line() {
        let raw = "{\"type\":\"inspect\",\"data\":{\"1.0.0\":\"2025-01-01T00:00:00.000Z\"}}\n";
        let parsed = parse_yarn_inspect_object(raw, "time").expect("should parse");
        assert_eq!(
            parsed.get("1.0.0").map(String::as_str),
            Some("2025-01-01T00:00:00.000Z")
        );
    }

    #[test]
    fn parse_yarn_major_version_plain() {
        assert_eq!(parse_yarn_major_version("1.22.22\n"), Some(1));
    }

    #[test]
    fn parse_yarn_major_version_with_v_prefix() {
        assert_eq!(parse_yarn_major_version("v4.3.1\n"), Some(4));
    }
}
