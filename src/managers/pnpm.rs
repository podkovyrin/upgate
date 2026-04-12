use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, PlanDecision, PlanMeta, SemverAgeResolution, SemverTimestamp,
    emit_manager_level_error, emit_plan_and_collect_upgradable, emit_version_scan_outcomes,
    parse_semver_time_releases, release_age_secs_for_version, resolve_semver_with_min_age,
    verbose_now_unix_secs,
};
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::RunCmd;
use crate::util::time::now_unix_secs;
use crate::util::timefmt::human_age;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

const PNPM_MAX_PARALLEL_CHECKS: usize = 6;

pub(crate) struct PnpmPlugin;

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

pub(crate) static PLUGIN: PnpmPlugin = PnpmPlugin;

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

    apply_pnpm_updates(upgradable);

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
    run_indexed_parallel(
        jobs,
        threads,
        "failed to build pnpm planning thread pool",
        "internal error: missing pnpm plan slot",
        |(name, current)| {
            let resolved =
                pnpm_resolve_target_with_min_age(&name, &current, now_unix_secs, min_age)
                    .map_err(|err| err.to_string());

            PnpmPlanItem {
                name,
                current,
                resolved,
            }
        },
    )
}

fn apply_pnpm_updates(upgradable: Vec<(String, String, String)>) {
    for (name, current, version) in upgradable {
        let spec = format!("{name}@{version}");
        if let Err(err) = RunCmd::Success.run("pnpm", ["add", "-g", &spec]) {
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

fn pnpm_installed_global() -> Result<BTreeMap<String, String>> {
    let items: Vec<PnpmListItem> =
        RunCmd::Success.json("pnpm", ["list", "-g", "--depth", "0", "--json"])?;

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
    let output = RunCmd::IgnoreStatus
        .run("pnpm", ["outdated", "-g", "--json"])
        .with_context(|| "failed to run pnpm outdated -g --json")?;

    let stdout = String::from_utf8(output.stdout).context("pnpm outdated output not UTF-8")?;
    let stderr = String::from_utf8_lossy(&output.stderr);

    if is_no_importer_manifest_error(&stdout) || is_no_importer_manifest_error(&stderr) {
        return Ok(BTreeMap::new());
    }

    // Similar to npm, pnpm can return non-zero when outdated packages exist.
    if !output.status.success() && output.status.code() != Some(1) {
        let err_text = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        bail!("pnpm outdated -g --json failed: {err_text}");
    }

    if stdout.trim().is_empty() {
        return Ok(BTreeMap::new());
    }

    parse_pnpm_outdated_json(&stdout)
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

impl PnpmResolvedTarget {
    fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest> {
        DelayedLatest::from_latest(
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
        RunCmd::Success.json("pnpm", ["view", name, "time", "--json"])?;
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
        RunCmd::Success.json("pnpm", ["view", name, "time", "--json"])?;
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
        assert!(parsed.get("bar").is_none());
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
