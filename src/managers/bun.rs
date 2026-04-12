use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, PlanDecision, PlanMeta, SemverAgeResolution, SemverTimestamp,
    emit_manager_level_error, emit_plan_and_collect_upgradable, emit_scan_current,
    parse_semver_time_releases, release_age_secs_for_version, resolve_semver_with_min_age,
    verbose_now_unix_secs,
};
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{RunCheck, run_cmd};
use crate::util::time::now_unix_secs;
use crate::util::timefmt::human_age;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::time::Duration;

const BUN_MAX_PARALLEL_CHECKS: usize = 6;

pub(crate) struct BunPlugin;

impl ManagerPlugin for BunPlugin {
    fn id(&self) -> &'static str {
        "bun"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub(crate) static PLUGIN: BunPlugin = BunPlugin;

#[derive(Debug)]
struct OutdatedEntry {
    current: String,
}

struct BunPlanItem {
    name: String,
    current: String,
    resolved: Result<BunResolvedTarget, String>,
}

fn run(ctx: &ManagerCtx) -> Result<()> {
    if ctx.is_scan() {
        return scan(ctx);
    }

    let min_age = ctx.policy.min_release_age.duration();
    let bun = bun_executable();

    let outdated = match bun_outdated_global(&bun) {
        Ok(outdated) => outdated,
        Err(err) => {
            emit_bun_manager_error(format!("failed to query global Bun packages: {err}"));
            return Ok(());
        }
    };

    if outdated.is_empty() {
        return Ok(());
    }

    let now = now_unix_secs()?;

    let global_cwd = match bun_global_cwd() {
        Ok(path) => path,
        Err(err) => {
            emit_bun_manager_error(format!("failed to resolve Bun global directory: {err}"));
            return Ok(());
        }
    };

    let plan = resolve_bun_plan(
        &bun,
        &global_cwd,
        &outdated,
        now,
        min_age,
        ctx.max_parallel_checks,
    )?;

    let _upgradable = emit_plan_and_collect_upgradable(
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

    apply_bun_updates(&bun, min_age);

    Ok(())
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let bun = bun_executable();

    let installed = match bun_installed_global(&bun) {
        Ok(installed) => installed,
        Err(err) => {
            emit_bun_manager_error(format!("failed to query global Bun packages: {err}"));
            return Ok(());
        }
    };

    if installed.is_empty() {
        return Ok(());
    }

    let now = verbose_now_unix_secs()?;

    let global_cwd = if now.is_some() {
        bun_global_cwd().ok()
    } else {
        None
    };

    emit_bun_scan_outcomes(
        &bun,
        installed,
        global_cwd.as_deref(),
        now,
        ctx.scan_old_age_threshold,
    );

    Ok(())
}

fn resolve_bun_plan(
    bun: &str,
    global_cwd: &str,
    outdated: &BTreeMap<String, OutdatedEntry>,
    now_unix_secs: u64,
    min_age: Duration,
    max_parallel_checks: usize,
) -> Result<Vec<BunPlanItem>> {
    let jobs: Vec<(String, String)> = outdated
        .iter()
        .map(|(name, entry)| (name.clone(), entry.current.clone()))
        .collect();

    let threads = effective_parallelism(max_parallel_checks, BUN_MAX_PARALLEL_CHECKS);
    let bun_path = bun.to_string();
    let global_cwd_path = global_cwd.to_string();
    run_indexed_parallel(
        jobs,
        threads,
        "failed to build bun planning thread pool",
        "internal error: missing bun plan slot",
        move |(name, current)| {
            let resolved = bun_resolve_target_with_min_age(
                &bun_path,
                &global_cwd_path,
                &name,
                &current,
                now_unix_secs,
                min_age,
            )
            .map_err(|err| err.to_string());

            BunPlanItem {
                name,
                current,
                resolved,
            }
        },
    )
}

fn apply_bun_updates(bun: &str, min_age: Duration) {
    if let Err(err) = run_cmd(
        bun,
        [
            "update",
            "-g",
            "--minimum-release-age",
            &min_age.as_secs().to_string(),
        ],
        RunCheck::Success,
    ) {
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
}

fn emit_bun_scan_outcomes(
    bun: &str,
    installed: BTreeMap<String, String>,
    global_cwd: Option<&str>,
    now_unix_secs: Option<u64>,
    old_threshold: Duration,
) {
    for (name, current) in installed {
        let age_secs = if let (Some(now_unix_secs), Some(cwd)) = (now_unix_secs, global_cwd) {
            bun_release_age_secs(bun, cwd, &name, &current, now_unix_secs)
                .ok()
                .flatten()
        } else {
            None
        };

        emit_scan_current(
            PLUGIN.id(),
            PLUGIN.id(),
            name,
            current,
            age_secs,
            old_threshold,
        );
    }
}

fn bun_installed_global(bun: &str) -> Result<BTreeMap<String, String>> {
    let output = run_cmd(bun, ["pm", "ls", "-g", "--json"], RunCheck::IgnoreStatus)?;
    let stdout = String::from_utf8(output.stdout).context("bun pm ls output not UTF-8")?;
    let stderr = String::from_utf8_lossy(&output.stderr);

    if is_missing_global_manifest(&stdout) || is_missing_global_manifest(&stderr) {
        return Ok(BTreeMap::new());
    }

    if !output.status.success() {
        let err_text = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        bail!("bun pm ls -g --json failed: {err_text}");
    }

    if stdout.trim().is_empty() {
        return Ok(BTreeMap::new());
    }

    let val: serde_json::Value =
        serde_json::from_str(&stdout).context("failed to parse bun pm ls JSON")?;

    let mut out = BTreeMap::new();
    if let Some(obj) = val
        .get("dependencies")
        .and_then(serde_json::Value::as_object)
    {
        for (name, dep) in obj {
            if let Some(version) = dep
                .get("version")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
            {
                out.insert(name.clone(), version);
            }
        }
    }

    Ok(out)
}

fn bun_outdated_global(bun: &str) -> Result<BTreeMap<String, OutdatedEntry>> {
    let output = run_cmd(bun, ["outdated", "-g"], RunCheck::IgnoreStatus)?;

    let stdout = String::from_utf8(output.stdout).context("bun outdated output not UTF-8")?;
    let stderr = String::from_utf8_lossy(&output.stderr);

    if is_missing_global_manifest(&stdout) || is_missing_global_manifest(&stderr) {
        return Ok(BTreeMap::new());
    }

    if !output.status.success() {
        let err_text = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        bail!("bun outdated -g failed: {err_text}");
    }

    Ok(parse_bun_outdated_table(&stdout))
}

fn parse_bun_outdated_table(stdout: &str) -> BTreeMap<String, OutdatedEntry> {
    let mut out = BTreeMap::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }

        if is_table_separator_line(trimmed) {
            continue;
        }

        let cols: Vec<&str> = trimmed
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();

        if cols.len() < 2 {
            continue;
        }

        if cols[0].eq_ignore_ascii_case("package") {
            continue;
        }

        out.insert(
            cols[0].to_string(),
            OutdatedEntry {
                current: cols[1].to_string(),
            },
        );
    }

    out
}

fn is_table_separator_line(line: &str) -> bool {
    line.chars()
        .all(|c| c == '|' || c == '-' || c == ' ' || c == '\t')
}

fn is_missing_global_manifest(text: &str) -> bool {
    text.contains("missing package.json")
        || text.contains("MissingPackageJSON")
        || text.contains("No package.json was found for directory")
        || text.contains("missing lockfile, nothing outdated")
        || text.contains("Lockfile not found")
}

struct BunResolvedTarget {
    selected_version: Option<String>,
    latest_version: Option<String>,
    latest_age_secs: Option<u64>,
}

impl BunResolvedTarget {
    fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest> {
        DelayedLatest::from_latest(
            self.latest_version.as_deref(),
            self.latest_age_secs,
            min_age,
        )
    }
}

fn bun_resolve_target_with_min_age(
    bun: &str,
    global_cwd: &str,
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<BunResolvedTarget> {
    let output = run_cmd(
        bun,
        ["pm", "view", name, "time", "--json", "--cwd", global_cwd],
        RunCheck::IgnoreStatus,
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("bun pm view {name} time --json failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout).context("bun pm view output not UTF-8")?;
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .with_context(|| format!("failed to parse bun pm view JSON for {name}"))?;

    let releases = bun_semver_time_releases(name, &val)?;

    let SemverAgeResolution {
        selected_version,
        latest_version,
        latest_age_secs,
    } = resolve_semver_with_min_age(current, &releases, now_unix_secs, min_age)
        .with_context(|| format!("failed to resolve eligible semver target for {name}"))?;

    Ok(BunResolvedTarget {
        selected_version,
        latest_version,
        latest_age_secs,
    })
}

fn bun_global_cwd() -> Result<String> {
    if let Ok(bun_install) = std::env::var("BUN_INSTALL") {
        let bun_install = bun_install.trim();
        if !bun_install.is_empty() {
            return Ok(format!("{bun_install}/install/global"));
        }
    }

    let home = std::env::var("HOME").context("HOME env var is not set")?;
    Ok(format!("{home}/.bun/install/global"))
}

fn bun_executable() -> String {
    if let Ok(path) = std::env::var("UPNOW_BUN_BIN") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    if let Some(path) = bun_from_mise() {
        return path;
    }

    PLUGIN.id().to_string()
}

fn bun_from_mise() -> Option<String> {
    let output = run_cmd("mise", ["which", "bun"], RunCheck::Success).ok()?;

    let path = String::from_utf8(output.stdout).ok()?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn bun_release_age_secs(
    bun: &str,
    global_cwd: &str,
    name: &str,
    version: &str,
    now_unix_secs: u64,
) -> Result<Option<u64>> {
    let output = run_cmd(
        bun,
        ["pm", "view", name, "time", "--json", "--cwd", global_cwd],
        RunCheck::IgnoreStatus,
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("bun pm view {name} time --json failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout).context("bun pm view output not UTF-8")?;
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .with_context(|| format!("failed to parse bun pm view JSON for {name}"))?;

    let releases = bun_semver_time_releases(name, &val)?;
    Ok(release_age_secs_for_version(
        &releases,
        version,
        now_unix_secs,
    ))
}

fn bun_semver_time_releases(name: &str, val: &serde_json::Value) -> Result<Vec<SemverTimestamp>> {
    let obj = val
        .as_object()
        .with_context(|| format!("bun pm view time JSON is not an object for {name}"))?;

    parse_semver_time_releases(PLUGIN.id(), name, obj)
}

fn emit_bun_manager_error(detail: impl AsRef<str>) {
    emit_manager_level_error(PLUGIN.id(), PLUGIN.id(), detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_outdated_table() {
        let raw = r"bun outdated v1.3.11 (af24e281)
|----------------------------------------|
| Package  | Current | Update  | Latest  |
|----------|---------|---------|---------|
| npm      | 11.12.0 | 11.12.0 | 11.12.1 |
| typescript | 5.9.3 | 5.9.3 | 5.9.4 |
|----------------------------------------|
";

        let parsed = parse_bun_outdated_table(raw);
        assert_eq!(
            parsed.get("npm").map(|e| e.current.as_str()),
            Some("11.12.0")
        );
        assert_eq!(
            parsed.get("typescript").map(|e| e.current.as_str()),
            Some("5.9.3")
        );
    }

    #[test]
    fn parse_outdated_without_rows() {
        let raw = "bun outdated v1.3.11 (af24e281)\n";
        let parsed = parse_bun_outdated_table(raw);
        assert!(parsed.is_empty());
    }

    #[test]
    fn detect_missing_manifest_messages() {
        assert!(is_missing_global_manifest(
            "error: missing package.json, nothing outdated"
        ));
        assert!(is_missing_global_manifest(
            "error: failed to initialize bun install: MissingPackageJSON"
        ));
        assert!(is_missing_global_manifest(
            "error: No package.json was found for directory '/tmp/x'"
        ));
        assert!(is_missing_global_manifest(
            "error: missing lockfile, nothing outdated"
        ));
        assert!(is_missing_global_manifest("error: Lockfile not found"));
    }
}
