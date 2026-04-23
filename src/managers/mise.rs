use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::managers::shared::versioning::policy::VersionPolicy;
#[allow(clippy::wildcard_imports)]
use crate::managers::*;
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::{now_unix_secs, parse_rfc3339_unix};

const MISE_MAX_PARALLEL_CHECKS: usize = 4;

pub struct MisePlugin;

impl ManagerPlugin for MisePlugin {
    fn id(&self) -> &'static str {
        "mise"
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

pub static PLUGIN: MisePlugin = MisePlugin;

type NpmTimeMap = BTreeMap<String, String>;
type MisePlanItem = ResolvedPlanItem<VersionPolicyResolution>;

#[derive(Debug, Deserialize)]
struct MiseLsByToolEntry {
    version: Option<String>,
}

type MiseLsJson = BTreeMap<String, Vec<MiseLsByToolEntry>>;

#[derive(Debug, Deserialize)]
struct MiseLsRemoteVersion {
    version: String,
    #[serde(default)]
    created_at: Option<String>,
}

fn run(ctx: &ManagerCtx) -> Result<()> {
    run_manager_pipeline(ctx, scan, run_plan_apply)
}

fn run_plan_apply(ctx: &ManagerCtx) -> Result<()> {
    run_plan_apply_framework(
        ctx,
        PLUGIN.id(),
        PlanApplyFrameworkPolicy::SOFT_FETCH_STRICT_RESOLVE,
        || mise_installed_versions().context("failed to read installed mise tools"),
        BTreeMap::is_empty,
        |installed, runtime| {
            resolve_mise_plan(
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
            run_per_item_apply_flow(ctx, PLUGIN.id(), upgradable, apply_mise_updates)
        },
    )
}

fn resolve_mise_plan(
    installed: &BTreeMap<String, String>,
    now_unix_secs: u64,
    min_age: Duration,
    max_parallel_checks: usize,
    version_policy: VersionPolicy,
) -> Result<Vec<MisePlanItem>> {
    let jobs: Vec<(String, String)> = installed
        .iter()
        .map(|(tool, current)| (tool.clone(), current.clone()))
        .collect();

    let threads = effective_parallelism(max_parallel_checks, MISE_MAX_PARALLEL_CHECKS);
    run_indexed_parallel(jobs, threads, PLUGIN.id(), |(tool, current)| {
        let resolved = mise_resolve_target_with_min_age(
            &tool,
            &current,
            now_unix_secs,
            min_age,
            version_policy,
        )
        .map_err(|err| err.to_string());

        MisePlanItem::new(tool, current, resolved)
    })
}

fn mise_resolve_target_with_min_age(
    tool: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
    version_policy: VersionPolicy,
) -> Result<VersionPolicyResolution> {
    let releases = mise_semver_releases(tool)?;

    let resolved =
        resolve_semver_with_min_age(current, &releases, now_unix_secs, min_age, version_policy)
            .with_context(|| format!("failed to resolve eligible semver target for {tool}"))?;

    Ok(resolved)
}

fn mise_semver_releases(tool: &str) -> Result<Vec<SemverTimestamp>> {
    if tool.starts_with("npm:") {
        return npm_semver_time_releases_for_mise(tool);
    }

    // TODO(option-b): replace this fallback with explicit per-ecosystem metadata
    // providers (cargo, dotnet, pypi, etc.) routed by tool prefix/plugin.
    // Keep ls-remote as a lowest-priority safety net.
    mise_ls_remote_semver_releases(tool)
}

fn npm_semver_time_releases_for_mise(tool: &str) -> Result<Vec<SemverTimestamp>> {
    let pkg = tool
        .strip_prefix("npm:")
        .with_context(|| format!("invalid mise npm tool name: {tool}"))?;

    let timestamps_by_version: NpmTimeMap =
        run_cmd("npm", ["view", pkg, "time", "--json"], CmdStatus::Success)
            .output()?
            .json()?;

    if timestamps_by_version.is_empty() {
        bail!("npm view time JSON is empty for {pkg}");
    }

    parse_semver_time_releases(PLUGIN.id(), pkg, &timestamps_by_version)
}

fn mise_ls_remote_semver_releases(tool: &str) -> Result<Vec<SemverTimestamp>> {
    let raw: serde_json::Value = run_cmd("mise", ["ls-remote", "--json", tool], CmdStatus::Success)
        .output()?
        .json()?;

    parse_mise_ls_remote_semver_releases(tool, raw)
}

fn parse_mise_ls_remote_semver_releases(
    tool: &str,
    raw: serde_json::Value,
) -> Result<Vec<SemverTimestamp>> {
    if let Ok(entries) = serde_json::from_value::<Vec<MiseLsRemoteVersion>>(raw.clone()) {
        let mut releases = Vec::new();
        for entry in entries {
            let Some(created_at) = entry.created_at else {
                continue;
            };

            let published_unix = parse_rfc3339_unix(&created_at).with_context(|| {
                format!(
                    "invalid mise ls-remote timestamp for {tool}@{}: {created_at}",
                    entry.version
                )
            })?;

            releases.push(SemverTimestamp {
                version: entry.version,
                published_unix,
            });
        }
        return Ok(releases);
    }

    if serde_json::from_value::<Vec<String>>(raw).is_ok() {
        // TODO(option-b): when ls-remote JSON lacks created_at metadata, route this
        // tool through an ecosystem provider that can return publish timestamps.
        return Ok(Vec::new());
    }

    bail!("failed to parse mise ls-remote JSON for {tool}")
}

fn apply_mise_updates(upgradable: Vec<crate::managers::PlannedUpdate>) {
    for item in upgradable {
        let name = item.name;
        let current = item.current;
        let target = item.target;
        let spec = format!("{name}@{target}");

        if let Err(err) = run_cmd("mise", ["upgrade", &spec], CmdStatus::Success)
            .mutating()
            .output()
        {
            emit_apply_error(PLUGIN.id(), name, current, target, err);
        }
    }
}

fn npm_version_age_secs(tool: &str, version: &str, now_unix_secs: u64) -> Result<Option<u64>> {
    let releases = npm_semver_time_releases_for_mise(tool)?;
    Ok(release_age_secs_for_version(
        &releases,
        version,
        now_unix_secs,
    ))
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
                npm_version_age_secs(&tool, &version, now_unix_secs)
                    .ok()
                    .flatten()
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
    fn parses_ls_remote_entries_with_created_at() {
        let raw: serde_json::Value = serde_json::from_str(
            r#"
[
  {"version":"1.0.0","created_at":"2020-01-01T00:00:00Z"},
  {"version":"1.1.0","created_at":"2021-01-01T00:00:00Z"}
]
"#,
        )
        .expect("valid json");

        let parsed = parse_mise_ls_remote_semver_releases("node", raw).expect("should parse");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].version, "1.0.0");
        assert_eq!(parsed[1].version, "1.1.0");
    }

    #[test]
    fn ls_remote_string_only_json_is_supported_as_empty_release_timeline() {
        let raw: serde_json::Value =
            serde_json::from_str(r#"["1.0.0","1.1.0"]"#).expect("valid json");

        let parsed = parse_mise_ls_remote_semver_releases("node", raw).expect("should parse");
        assert!(parsed.is_empty());
    }
}
