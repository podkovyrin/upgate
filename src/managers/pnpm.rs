use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, PlanDecision, PlanMeta, SemverAgeResolution, SemverTimestamp,
    emit_manager_level_error, emit_plan_and_collect_upgradable, emit_version_scan_outcomes,
    parse_semver_time_releases, release_age_secs_for_version, resolve_semver_with_min_age,
    verbose_now_unix_secs,
};
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::run_command_checked_stdout;
use crate::util::time::now_unix_secs;
use crate::util::timefmt::human_age;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::process::Command;
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
        if let Err(err) = run_pnpm(&["add", "-g", &spec]) {
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
    let output = Command::new("pnpm")
        .args(["list", "-g", "--depth", "0", "--json"])
        .output()
        .with_context(|| "failed to run pnpm list -g --depth 0 --json")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("pnpm list -g --depth 0 --json failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout).context("pnpm list output not UTF-8")?;
    if stdout.trim().is_empty() {
        return Ok(BTreeMap::new());
    }

    let val: serde_json::Value =
        serde_json::from_str(&stdout).context("failed to parse pnpm list JSON")?;

    let mut out = BTreeMap::new();
    let Some(items) = val.as_array() else {
        return Ok(out);
    };

    for item in items {
        let deps = item
            .get("dependencies")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();

        for (name, dep) in deps {
            if let Some(version) = dep
                .get("version")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
            {
                out.insert(name, version);
            }
        }
    }

    Ok(out)
}

fn pnpm_outdated_global() -> Result<BTreeMap<String, OutdatedEntry>> {
    let output = Command::new("pnpm")
        .args(["outdated", "-g", "--json"])
        .output()
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
    let val: serde_json::Value =
        serde_json::from_str(stdout).context("failed to parse pnpm outdated JSON")?;

    let mut out = BTreeMap::new();

    if let Some(obj) = val.as_object() {
        for (name, entry_val) in obj {
            let Some(current) = entry_val
                .as_object()
                .and_then(|o| o.get("current"))
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };

            out.insert(
                name.clone(),
                OutdatedEntry {
                    current: current.to_string(),
                },
            );
        }

        return Ok(out);
    }

    if let Some(items) = val.as_array() {
        for item in items {
            let Some(obj) = item.as_object() else {
                continue;
            };

            let name = obj
                .get("name")
                .or_else(|| obj.get("packageName"))
                .or_else(|| obj.get("package"))
                .and_then(serde_json::Value::as_str);

            let current = obj.get("current").and_then(serde_json::Value::as_str);

            let (Some(name), Some(current)) = (name, current) else {
                continue;
            };

            out.insert(
                name.to_string(),
                OutdatedEntry {
                    current: current.to_string(),
                },
            );
        }

        return Ok(out);
    }

    bail!("unsupported pnpm outdated JSON shape")
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
    let output = Command::new("pnpm")
        .args(["view", name, "time", "--json"])
        .output()
        .with_context(|| format!("failed to run pnpm view {name} time --json"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("pnpm view {name} time --json failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout).context("pnpm view output not UTF-8")?;
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .with_context(|| format!("failed to parse pnpm view JSON for {name}"))?;

    let releases = pnpm_semver_time_releases(name, &val)?;

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
    let output = Command::new("pnpm")
        .args(["view", name, "time", "--json"])
        .output()
        .with_context(|| format!("failed to run pnpm view {name} time --json"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("pnpm view {name} time --json failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout).context("pnpm view output not UTF-8")?;
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .with_context(|| format!("failed to parse pnpm view JSON for {name}"))?;

    let releases = pnpm_semver_time_releases(name, &val)?;
    Ok(release_age_secs_for_version(
        &releases,
        version,
        now_unix_secs,
    ))
}

fn pnpm_semver_time_releases(name: &str, val: &serde_json::Value) -> Result<Vec<SemverTimestamp>> {
    let obj = val
        .as_object()
        .with_context(|| format!("pnpm view time JSON is not an object for {name}"))?;

    parse_semver_time_releases(PLUGIN.id(), name, obj)
}

fn run_pnpm(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("pnpm");
    command.args(args);
    run_command_checked_stdout(command)
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
    fn parse_outdated_array_shape() {
        let raw = r#"[
          { "name": "foo", "current": "1.0.0" },
          { "packageName": "bar", "current": "2.0.0" }
        ]"#;

        let parsed = parse_pnpm_outdated_json(raw).expect("should parse");
        assert_eq!(parsed.get("foo").map(|e| e.current.as_str()), Some("1.0.0"));
        assert_eq!(parsed.get("bar").map(|e| e.current.as_str()), Some("2.0.0"));
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
