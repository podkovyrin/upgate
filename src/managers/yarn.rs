use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::managers::shared::versioning::policy::VersionPolicy;
#[allow(clippy::wildcard_imports)]
use crate::managers::*;
use crate::outcome::{ItemOutcome, ReasonCode, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};

const YARN_MAX_PARALLEL_CHECKS: usize = 6;

pub struct YarnPlugin;

impl ManagerPlugin for YarnPlugin {
    fn id(&self) -> &'static str {
        "yarn"
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

pub static PLUGIN: YarnPlugin = YarnPlugin;

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

type YarnPlanItem = ResolvedPlanItem<VersionPolicyResolution>;

fn run(ctx: &ManagerCtx) -> Result<()> {
    run_manager_pipeline(ctx, scan, run_plan_apply)
}

fn run_plan_apply(ctx: &ManagerCtx) -> Result<()> {
    let Some(yarn_major) = soft_fail(
        yarn_major_version(),
        PLUGIN.id(),
        "failed to detect Yarn major version",
    ) else {
        return Ok(());
    };

    if yarn_major >= 2 {
        let outcome = ItemOutcome::skipped(
            PLUGIN.id(),
            "*",
            "*",
            "*",
            ReasonCode::MissingMetadata,
            "global upgrades are not supported for Yarn 2+; skipping manager",
        );
        emit_text_outcome(&outcome);
        return Ok(());
    }

    run_plan_apply_framework(
        ctx,
        PLUGIN.id(),
        PlanApplyFrameworkPolicy::SOFT_FETCH_SOFT_RESOLVE,
        || yarn_global_installed().context("failed to query global Yarn packages"),
        BTreeMap::is_empty,
        |installed, runtime| {
            resolve_yarn_plan(
                installed,
                runtime.now_unix_secs,
                runtime.min_age,
                runtime.max_parallel_checks,
                ctx.policy.version_policy,
            )
            .context("planning execution failed")
        },
        |_installed, plan, runtime| {
            Ok(collect_upgradable_from_resolved_plan(
                PLUGIN.id(),
                plan,
                runtime.min_age,
                runtime.suppress_update_outcomes,
                runtime.pinned,
            ))
        },
        |ctx, _installed, upgradable| {
            run_per_item_apply_flow(ctx, PLUGIN.id(), upgradable, apply_yarn_updates)
        },
    )
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let Some(yarn_major) = soft_fail(
        yarn_major_version(),
        PLUGIN.id(),
        "failed to detect Yarn major version",
    ) else {
        return Ok(());
    };

    if yarn_major >= 2 {
        let outcome = ItemOutcome::skipped(
            PLUGIN.id(),
            "*",
            "*",
            "*",
            ReasonCode::MissingMetadata,
            "global upgrades are not supported for Yarn 2+; skipping manager",
        );
        emit_text_outcome(&outcome);
        return Ok(());
    }

    let Some(installed) = soft_fail(
        yarn_global_installed(),
        PLUGIN.id(),
        "failed to query global Yarn packages",
    ) else {
        return Ok(());
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
    version_policy: VersionPolicy,
) -> Result<Vec<YarnPlanItem>> {
    let jobs: Vec<(String, String)> = installed
        .iter()
        .map(|(name, entry)| (name.clone(), entry.current.clone()))
        .collect();

    let threads = effective_parallelism(max_parallel_checks, YARN_MAX_PARALLEL_CHECKS);
    run_indexed_parallel(jobs, threads, PLUGIN.id(), |(name, current)| {
        let resolved = yarn_resolve_target_with_min_age(
            &name,
            &current,
            now_unix_secs,
            min_age,
            version_policy,
        )
        .map_err(|err| err.to_string());

        YarnPlanItem::new(name, current, resolved)
    })
}

fn apply_yarn_updates(upgradable: Vec<crate::managers::PlannedUpdate>) {
    for item in upgradable {
        let name = item.name;
        let current = item.current;
        let version = item.target;
        let spec = format!("{name}@{version}");
        if let Err(err) = run_cmd("yarn", ["global", "add", &spec], CmdStatus::Success)
            .mutating()
            .output()
        {
            emit_apply_error(PLUGIN.id(), name, current, version, err);
        }
    }
}

fn yarn_major_version() -> Result<u64> {
    let output = run_cmd("yarn", ["--version"], CmdStatus::Success).output()?;
    let text = output.stdout()?;

    parse_yarn_major_version(text)
        .with_context(|| format!("failed to parse yarn major version from '{}'", text.trim()))
}

fn parse_yarn_major_version(text: &str) -> Option<u64> {
    if text.is_empty() {
        return None;
    }

    let first_token = text.split_whitespace().next()?;
    let trimmed = crate::util::text::strip_v_prefix(first_token);
    let major = trimmed.split('.').next()?;
    major.parse::<u64>().ok()
}

fn yarn_global_installed() -> Result<BTreeMap<String, InstalledEntry>> {
    let output = run_cmd(
        "yarn",
        ["global", "list", "--depth=0", "--json"],
        CmdStatus::Success,
    )
    .output()?;
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

fn yarn_resolve_target_with_min_age(
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
    version_policy: VersionPolicy,
) -> Result<VersionPolicyResolution> {
    let output = run_cmd("yarn", ["info", name, "time", "--json"], CmdStatus::Success).output()?;
    let text = output.stdout()?;

    let obj = parse_yarn_inspect_object(text, "time")?;
    let releases = yarn_semver_time_releases(name, &obj)?;

    let resolved =
        resolve_semver_with_min_age(current, &releases, now_unix_secs, min_age, version_policy)
            .with_context(|| format!("failed to resolve eligible semver target for {name}"))?;

    Ok(resolved)
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

        if let YarnInfoJsonLine::Inspect { data } = parsed {
            return Ok(data);
        }
    }

    bail!("failed to parse yarn {field} JSON payload")
}

fn yarn_release_age_secs(name: &str, version: &str, now_unix_secs: u64) -> Result<Option<u64>> {
    let output = run_cmd("yarn", ["info", name, "time", "--json"], CmdStatus::Success).output()?;
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
