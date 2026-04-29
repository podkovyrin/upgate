use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::managers::shared::versioning::policy::VersionPolicy;
#[allow(clippy::wildcard_imports)]
use crate::managers::*;
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};

const PNPM_MAX_PARALLEL_CHECKS: usize = 6;

pub struct PnpmPlugin;

impl ManagerPlugin for PnpmPlugin {
    fn id(&self) -> &'static str {
        "pnpm"
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

pub static PLUGIN: PnpmPlugin = PnpmPlugin;

#[derive(Debug, Deserialize)]
struct PnpmListItem {
    #[serde(default)]
    dependencies: BTreeMap<String, PnpmDependency>,
}

#[derive(Debug, Deserialize)]
struct PnpmDependency {
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PnpmOutdatedMapEntry {
    current: Option<String>,
}

type PnpmTimeMap = BTreeMap<String, String>;

type PnpmPlanItem = ResolvedPlanItem<VersionPolicyResolution>;

fn run(ctx: &ManagerCtx) -> Result<()> {
    run_manager_pipeline(ctx, scan, run_plan_apply)
}

fn run_plan_apply(ctx: &ManagerCtx) -> Result<()> {
    run_plan_apply_framework(
        ctx,
        PLUGIN.id(),
        PlanApplyFrameworkPolicy::SOFT_FETCH_STRICT_RESOLVE,
        || pnpm_plan_seed(ctx.policy.version_policy),
        BTreeMap::is_empty,
        |plan_seed, runtime| {
            resolve_pnpm_plan(
                plan_seed,
                runtime.now_unix_secs,
                runtime.min_age,
                runtime.max_parallel_checks,
                ctx.policy.version_policy,
            )
            .context("planning execution failed")
        },
        |_plan_seed, plan, runtime| {
            Ok(collect_apply_candidates_from_resolved_plan(
                PLUGIN.id(),
                plan,
                runtime.min_age,
                runtime.suppress_update_outcomes,
                runtime.pinned,
                true,
            ))
        },
        |ctx, _plan_seed, candidates| {
            run_per_item_apply_candidate_flow(ctx, PLUGIN.id(), candidates, apply_pnpm_updates)
        },
    )
}

fn pnpm_plan_seed(version_policy: VersionPolicy) -> Result<BTreeMap<String, String>> {
    if version_policy == VersionPolicy::Disabled {
        return pnpm_outdated_global().context("failed to query outdated pnpm packages");
    }

    pnpm_installed_global().context("failed to query installed pnpm packages")
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let Some(installed) = soft_fail(
        pnpm_installed_global(),
        PLUGIN.id(),
        "failed to query installed pnpm packages",
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
        pnpm_release_age_secs,
    );

    Ok(())
}

fn resolve_pnpm_plan(
    plan_seed: &BTreeMap<String, String>,
    now_unix_secs: u64,
    min_age: Duration,
    max_parallel_checks: usize,
    version_policy: VersionPolicy,
) -> Result<Vec<PnpmPlanItem>> {
    let jobs = plan_seed
        .iter()
        .map(|(name, current)| (name.clone(), current.clone()))
        .collect();

    let threads = effective_parallelism(max_parallel_checks, PNPM_MAX_PARALLEL_CHECKS);
    run_indexed_parallel(jobs, threads, PLUGIN.id(), |(name, current)| {
        let resolved = pnpm_resolve_target_with_min_age(
            &name,
            &current,
            now_unix_secs,
            min_age,
            version_policy,
        )
        .map_err(|err| err.to_string());

        PnpmPlanItem::new(name, current, resolved)
    })
}

fn apply_pnpm_updates(upgradable: Vec<crate::managers::PlannedUpdate>) {
    for item in upgradable {
        let name = item.name;
        let current = item.current;
        let version = item.target;
        let spec = format!("{name}@{version}");
        if let Err(err) = run_cmd("pnpm", ["add", "-g", &spec], CmdStatus::Success)
            .mutating()
            .output()
        {
            emit_apply_error(PLUGIN.id(), name, current, version, err);
        }
    }
}

fn pnpm_installed_global() -> Result<BTreeMap<String, String>> {
    let items: Vec<PnpmListItem> = run_cmd(
        "pnpm",
        ["list", "-g", "--depth", "0", "--json"],
        CmdStatus::Success,
    )
    .output()?
    .json()?;

    let mut out = BTreeMap::new();
    for item in items {
        for (name, dep) in item.dependencies {
            if let Some(version) = dep.version {
                out.insert(name, version);
            }
        }
    }

    Ok(out)
}

fn pnpm_outdated_global() -> Result<BTreeMap<String, String>> {
    let output = run_cmd(
        "pnpm",
        ["outdated", "-g", "--json"],
        CmdStatus::IgnoreStatus,
    )
    .output()?;

    let stdout = output.stdout()?;
    let stderr = output.stderr().unwrap_or_default();

    if is_no_importer_manifest_error(stdout) || is_no_importer_manifest_error(stderr) {
        return Ok(BTreeMap::new());
    }

    // Similar to npm, pnpm can return non-zero when outdated packages exist.
    if !output.success() && output.code() != Some(1) {
        let err_text = crate::util::text::read_non_empty(stderr, stdout);
        bail!("pnpm outdated -g --json failed: {err_text}");
    }

    if crate::util::text::is_blank(stdout) {
        return Ok(BTreeMap::new());
    }

    parse_pnpm_outdated_json(stdout)
}

fn is_no_importer_manifest_error(text: &str) -> bool {
    text.contains("ERR_PNPM_NO_IMPORTER_MANIFEST_FOUND")
}

fn parse_pnpm_outdated_json(stdout: &str) -> Result<BTreeMap<String, String>> {
    let entries: BTreeMap<String, PnpmOutdatedMapEntry> =
        serde_json::from_str(stdout).context("failed to parse pnpm outdated JSON")?;

    let mut out = BTreeMap::new();
    for (name, entry) in entries {
        let Some(current) = entry.current else {
            continue;
        };

        out.insert(name, current);
    }

    Ok(out)
}

fn pnpm_resolve_target_with_min_age(
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
    version_policy: VersionPolicy,
) -> Result<VersionPolicyResolution> {
    let timestamps_by_version: PnpmTimeMap =
        run_cmd("pnpm", ["view", name, "time", "--json"], CmdStatus::Success)
            .output()?
            .json()?;
    let releases = pnpm_semver_time_releases(name, &timestamps_by_version)?;

    let resolved =
        resolve_semver_with_min_age(current, &releases, now_unix_secs, min_age, version_policy)
            .with_context(|| format!("failed to resolve eligible semver target for {name}"))?;

    Ok(resolved)
}

fn pnpm_release_age_secs(name: &str, version: &str, now_unix_secs: u64) -> Result<Option<u64>> {
    let timestamps_by_version: PnpmTimeMap =
        run_cmd("pnpm", ["view", name, "time", "--json"], CmdStatus::Success)
            .output()?
            .json()?;
    let releases = pnpm_semver_time_releases(name, &timestamps_by_version)?;
    Ok(release_age_secs_for_version(
        &releases,
        version,
        now_unix_secs,
    ))
}

fn pnpm_semver_time_releases(
    name: &str,
    timestamps_by_version: &PnpmTimeMap,
) -> Result<Vec<SemverTimestamp>> {
    if timestamps_by_version.is_empty() {
        anyhow::bail!("pnpm view time JSON is empty for {name}");
    }

    parse_semver_time_releases(PLUGIN.id(), name, timestamps_by_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_outdated_object_shape() {
        let raw = r#"{
          "foo": { "current": "1.0.0" },
          "bar": { "current": "2.0.0" }
        }"#;

        let parsed = parse_pnpm_outdated_json(raw).expect("should parse");
        assert_eq!(parsed.get("foo").map(String::as_str), Some("1.0.0"));
        assert_eq!(parsed.get("bar").map(String::as_str), Some("2.0.0"));
    }

    #[test]
    fn parse_outdated_ignores_entries_without_current() {
        let raw = r#"{
          "foo": { "current": "1.0.0" },
          "bar": { "latest": "2.0.0" }
        }"#;

        let parsed = parse_pnpm_outdated_json(raw).expect("should parse");
        assert_eq!(parsed.get("foo").map(String::as_str), Some("1.0.0"));
        assert!(!parsed.contains_key("bar"));
    }

    #[test]
    fn no_importer_manifest_detection() {
        let stderr = "ERR_PNPM_NO_IMPORTER_MANIFEST_FOUND: no package.json";
        assert!(is_no_importer_manifest_error(stderr));
    }

    #[test]
    fn no_importer_manifest_detection_with_pnpm_styled_text() {
        let stdout = " ERR_PNPM_NO_IMPORTER_MANIFEST_FOUND  No package.json found";
        assert!(is_no_importer_manifest_error(stdout));
    }
}
