use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, PlanMeta, ResolvedPlanTarget, SemverAgeResolution, SemverTimestamp,
    emit_manager_level_error, emit_plan_and_collect_upgradable, emit_version_scan_outcomes,
    parse_semver_time_releases, plan_decision_from_resolution, release_age_secs_for_version,
    resolve_semver_with_min_age, run_per_item_apply_flow, verbose_now_unix_secs,
};
use crate::outcome::{ItemOutcome, ReasonCode, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::now_unix_secs;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

const PNPM_MAX_PARALLEL_CHECKS: usize = 6;

pub struct PnpmPlugin;

impl ManagerPlugin for PnpmPlugin {
    fn id(&self) -> &'static str {
        "pnpm"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub static PLUGIN: PnpmPlugin = PnpmPlugin;

#[derive(Debug, Deserialize)]
struct OutdatedEntry {
    current: String,
}

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

struct PnpmPlanItem {
    name: String,
    current: String,
    resolved: Result<PnpmResolvedTarget, String>,
}

fn run(ctx: &ManagerCtx) -> Result<()> {
    if ctx.is_scan() {
        return scan(ctx);
    }

    let min_age = ctx.policy.min_release_age.duration();

    let outdated = match pnpm_outdated_global() {
        Ok(outdated) => outdated,
        Err(err) => {
            emit_pnpm_manager_error(format!("failed to query outdated pnpm packages: {err}"));
            return Ok(());
        }
    };

    if outdated.is_empty() {
        return Ok(());
    }

    let now = now_unix_secs()?;

    let plan = resolve_pnpm_plan(&outdated, now, min_age, ctx.max_parallel_checks)?;

    let upgradable = emit_plan_and_collect_upgradable(
        plan,
        |item| {
            let PnpmPlanItem {
                name,
                current,
                resolved,
            } = item;

            let decision = plan_decision_from_resolution(&current, resolved, min_age);

            (
                PlanMeta {
                    manager: PLUGIN.id(),
                    source: PLUGIN.id(),
                    name,
                    current,
                },
                decision,
            )
        },
        ctx.is_interactive_apply(),
        Some(&ctx.policy.pinned),
    );

    run_per_item_apply_flow(ctx, PLUGIN.id(), upgradable, apply_pnpm_updates)?;

    Ok(())
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let installed = match pnpm_installed_global() {
        Ok(installed) => installed,
        Err(err) => {
            emit_pnpm_manager_error(format!("failed to query installed pnpm packages: {err}"));
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
        pnpm_release_age_secs,
    );

    Ok(())
}

fn resolve_pnpm_plan(
    outdated: &BTreeMap<String, OutdatedEntry>,
    now_unix_secs: u64,
    min_age: Duration,
    max_parallel_checks: usize,
) -> Result<Vec<PnpmPlanItem>> {
    let jobs: Vec<(String, String)> = outdated
        .iter()
        .map(|(name, entry)| (name.clone(), entry.current.clone()))
        .collect();

    let threads = effective_parallelism(max_parallel_checks, PNPM_MAX_PARALLEL_CHECKS);
    run_indexed_parallel(jobs, threads, PLUGIN.id(), |(name, current)| {
        let resolved = pnpm_resolve_target_with_min_age(&name, &current, now_unix_secs, min_age)
            .map_err(|err| err.to_string());

        PnpmPlanItem {
            name,
            current,
            resolved,
        }
    })
}

fn apply_pnpm_updates(upgradable: Vec<crate::managers::common::PlannedUpdate>) {
    for item in upgradable {
        let name = item.name;
        let current = item.current;
        let version = item.target;
        let spec = format!("{name}@{version}");
        if let Err(err) = run_cmd("pnpm", ["add", "-g", &spec], CmdStatus::Success)
            .mutating()
            .output()
        {
            let outcome = ItemOutcome::error(
                PLUGIN.id(),
                name,
                current,
                version,
                PLUGIN.id(),
                ReasonCode::CommandFailed,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
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

fn pnpm_outdated_global() -> Result<BTreeMap<String, OutdatedEntry>> {
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
        let err_text = if stderr.is_empty() { stdout } else { stderr };
        bail!("pnpm outdated -g --json failed: {err_text}");
    }

    if stdout.trim().is_empty() {
        return Ok(BTreeMap::new());
    }

    parse_pnpm_outdated_json(stdout)
}

fn is_no_importer_manifest_error(text: &str) -> bool {
    text.contains("ERR_PNPM_NO_IMPORTER_MANIFEST_FOUND")
}

fn parse_pnpm_outdated_json(stdout: &str) -> Result<BTreeMap<String, OutdatedEntry>> {
    let entries: BTreeMap<String, PnpmOutdatedMapEntry> =
        serde_json::from_str(stdout).context("failed to parse pnpm outdated JSON")?;

    let mut out = BTreeMap::new();
    for (name, entry) in entries {
        let Some(current) = entry.current else {
            continue;
        };

        out.insert(name, OutdatedEntry { current });
    }

    Ok(out)
}

struct PnpmResolvedTarget {
    selected_version: Option<String>,
    latest_version: Option<String>,
    latest_age_secs: Option<u64>,
}

impl ResolvedPlanTarget for PnpmResolvedTarget {
    fn selected_version(&self) -> Option<&str> {
        self.selected_version.as_deref()
    }

    fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest> {
        DelayedLatest::from_too_fresh_latest(
            self.selected_version.as_deref(),
            self.latest_version.as_deref(),
            self.latest_age_secs,
            min_age,
        )
    }
}

fn pnpm_resolve_target_with_min_age(
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<PnpmResolvedTarget> {
    let timestamps_by_version: PnpmTimeMap =
        run_cmd("pnpm", ["view", name, "time", "--json"], CmdStatus::Success)
            .output()?
            .json()?;
    let releases = pnpm_semver_time_releases(name, &timestamps_by_version)?;

    let SemverAgeResolution {
        selected_version,
        latest_version,
        latest_age_secs,
    } = resolve_semver_with_min_age(current, &releases, now_unix_secs, min_age)
        .with_context(|| format!("failed to resolve eligible semver target for {name}"))?;

    Ok(PnpmResolvedTarget {
        selected_version,
        latest_version,
        latest_age_secs,
    })
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

fn emit_pnpm_manager_error(detail: impl AsRef<str>) {
    emit_manager_level_error(PLUGIN.id(), PLUGIN.id(), detail);
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
        assert_eq!(parsed.get("foo").map(|e| e.current.as_str()), Some("1.0.0"));
        assert_eq!(parsed.get("bar").map(|e| e.current.as_str()), Some("2.0.0"));
    }

    #[test]
    fn parse_outdated_ignores_entries_without_current() {
        let raw = r#"{
          "foo": { "current": "1.0.0" },
          "bar": { "latest": "2.0.0" }
        }"#;

        let parsed = parse_pnpm_outdated_json(raw).expect("should parse");
        assert_eq!(parsed.get("foo").map(|e| e.current.as_str()), Some("1.0.0"));
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
