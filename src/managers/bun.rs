use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::managers::shared::versioning::policy::VersionPolicy;
#[allow(clippy::wildcard_imports)]
use crate::managers::*;
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};

const BUN_MAX_PARALLEL_CHECKS: usize = 6;

pub struct BunPlugin;

impl ManagerPlugin for BunPlugin {
    fn id(&self) -> &'static str {
        "bun"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn probe_command(&self) -> Option<String> {
        Some(bun_executable())
    }

    fn supports_version_policy(&self, _policy: VersionPolicy) -> bool {
        true
    }

    crate::impl_manager_pipeline!();
}

pub static PLUGIN: BunPlugin = BunPlugin;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BunPmLsJson {
    Root(BunPmLsRoot),
    Roots(Vec<BunPmLsRoot>),
}

#[derive(Debug, Deserialize)]
struct BunPmLsRoot {
    #[serde(default)]
    dependencies: BTreeMap<String, BunPmDependency>,
}

#[derive(Debug, Deserialize)]
struct BunPmDependency {
    version: Option<String>,
}

type BunTimeMap = BTreeMap<String, String>;

type BunPlanItem = ResolvedPlanItem<VersionPolicyResolution>;

fn apply(ctx: &ManagerCtx) -> Result<()> {
    run_planned_apply(ctx, plan_apply(ctx)?, apply_planned_updates)
}

fn plan_apply(ctx: &ManagerCtx) -> Result<Option<PlannedApply<String>>> {
    let bun = bun_executable();
    let fetch_bun = bun.clone();
    let resolve_bun = bun.clone();

    run_plan_apply_framework(
        ctx,
        PLUGIN.id(),
        PlanApplyFrameworkPolicy::SOFT_FETCH_STRICT_RESOLVE,
        || bun_installed_global(&fetch_bun).context("failed to query global Bun packages"),
        BTreeMap::is_empty,
        |installed, runtime| {
            let Some(global_cwd) = soft_fail(
                bun_global_cwd(),
                PLUGIN.id(),
                "failed to resolve Bun global directory",
            ) else {
                return Ok(Vec::new());
            };

            resolve_bun_plan(
                &resolve_bun,
                global_cwd.as_str(),
                installed,
                runtime.now_unix_secs,
                runtime.min_age,
                runtime.max_parallel_checks,
                ctx.policy.version_policy,
            )
            .context("planning execution failed")
        },
        move |_installed, plan, runtime| {
            let candidates = collect_apply_candidates_from_resolved_plan(
                PLUGIN.id(),
                plan,
                runtime.min_age,
                runtime.suppress_update_outcomes,
                runtime.pinned,
                true,
            );
            Ok(PlannedApplyPayload::new(bun, candidates))
        },
    )
}

fn interactive_apply(
    ctx: &ManagerCtx,
) -> Result<Option<crate::interactive::apply::InteractiveApplyPlan>> {
    Ok(plan_interactive_apply_from_planned(
        plan_apply(ctx)?,
        apply_planned_updates,
    ))
}

fn apply_planned_updates(
    ctx: &ManagerCtx,
    bun: String,
    selection: crate::interactive::apply::ApplySelection,
) {
    let min_age = ctx.policy.min_release_age.duration();
    apply_selective_or_global_selection(
        ctx,
        PLUGIN.id(),
        selection,
        |selected| apply_bun_selected_updates(&bun, min_age, selected),
        || apply_bun_updates(&bun, min_age),
    );
    drop(bun);
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let bun = bun_executable();

    let Some(installed) = soft_fail(
        bun_installed_global(&bun),
        PLUGIN.id(),
        "failed to query global Bun packages",
    ) else {
        return Ok(());
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
    installed: &BTreeMap<String, String>,
    now_unix_secs: u64,
    min_age: Duration,
    max_parallel_checks: usize,
    version_policy: VersionPolicy,
) -> Result<Vec<BunPlanItem>> {
    let jobs: Vec<(String, String)> = installed
        .iter()
        .map(|(name, current)| (name.clone(), current.clone()))
        .collect();

    let threads = effective_parallelism(max_parallel_checks, BUN_MAX_PARALLEL_CHECKS);
    let bun_path = bun.to_string();
    let global_cwd_path = global_cwd.to_string();
    run_indexed_parallel(jobs, threads, PLUGIN.id(), move |(name, current)| {
        let resolved = bun_resolve_target_with_min_age(
            &bun_path,
            &global_cwd_path,
            &name,
            &current,
            now_unix_secs,
            min_age,
            version_policy,
        )
        .map_err(|err| err.to_string());

        BunPlanItem::new(name, current, resolved)
    })
}

fn apply_bun_updates(bun: &str, min_age: Duration) -> Result<()> {
    let min_age_secs = min_age.as_secs().to_string();
    run_cmd(
        bun,
        ["update", "-g", "--minimum-release-age", &min_age_secs],
        CmdStatus::Success,
    )
    .mutating()
    .output()?;

    Ok(())
}

fn apply_bun_selected_updates(
    bun: &str,
    min_age: Duration,
    upgradable: Vec<crate::managers::PlannedUpdate>,
) {
    let min_age_secs = min_age.as_secs().to_string();

    for item in upgradable {
        let name = item.name;
        let current = item.current;
        let target = item.target;
        let args = bun_selected_update_args(
            &name,
            &target,
            &min_age_secs,
            item.gate_bypass.min_release_age,
        );

        if let Err(err) = run_cmd(bun, &args, CmdStatus::Success).mutating().output() {
            emit_apply_error(PLUGIN.id(), name, current, target, err);
        }
    }
}

fn bun_selected_update_args(
    name: &str,
    target: &str,
    min_age_secs: &str,
    bypass_min_release_age: bool,
) -> Vec<String> {
    let mut args = vec![
        "update".to_string(),
        "-g".to_string(),
        format!("{name}@{target}"),
    ];
    if !bypass_min_release_age {
        args.push("--minimum-release-age".to_string());
        args.push(min_age_secs.to_string());
    }
    args
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

        emit_scan_current(PLUGIN.id(), name, current, age_secs, old_threshold);
    }
}

fn bun_installed_global(bun: &str) -> Result<BTreeMap<String, String>> {
    let output = run_cmd(bun, ["pm", "ls", "-g", "--json"], CmdStatus::IgnoreStatus).output()?;
    let stdout = output.stdout()?;
    let stderr = output.stderr().unwrap_or_default();

    if is_missing_global_manifest(stdout) || is_missing_global_manifest(stderr) {
        return Ok(BTreeMap::new());
    }

    if !output.success() {
        let err_text = crate::util::text::read_non_empty(stderr, stdout);
        bail!("bun pm ls -g --json failed: {err_text}");
    }

    parse_bun_pm_ls_json(stdout)
}

fn parse_bun_pm_ls_json(stdout: &str) -> Result<BTreeMap<String, String>> {
    if crate::util::text::is_blank(stdout) {
        return Ok(BTreeMap::new());
    }

    let parsed: BunPmLsJson =
        serde_json::from_str(stdout).context("failed to parse bun pm ls JSON")?;

    let mut out = BTreeMap::new();
    let roots: Vec<BunPmLsRoot> = match parsed {
        BunPmLsJson::Root(root) => vec![root],
        BunPmLsJson::Roots(roots) => roots,
    };

    for root in roots {
        for (name, dep) in root.dependencies {
            if let Some(version) = dep.version {
                out.insert(name, version);
            }
        }
    }

    Ok(out)
}

fn is_missing_global_manifest(text: &str) -> bool {
    text.contains("missing package.json")
        || text.contains("MissingPackageJSON")
        || text.contains("No package.json was found for directory")
        || text.contains("missing lockfile, nothing outdated")
        || text.contains("Lockfile not found")
}

fn bun_resolve_target_with_min_age(
    bun: &str,
    global_cwd: &str,
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
    version_policy: VersionPolicy,
) -> Result<VersionPolicyResolution> {
    let timestamps_by_version: BunTimeMap = run_cmd(
        bun,
        ["pm", "view", name, "time", "--json", "--cwd", global_cwd],
        CmdStatus::IgnoreStatus,
    )
    .output()?
    .json()?;

    let releases = bun_semver_time_releases(name, &timestamps_by_version)?;

    let resolved =
        resolve_semver_with_min_age(current, &releases, now_unix_secs, min_age, version_policy)
            .with_context(|| format!("failed to resolve eligible semver target for {name}"))?;

    Ok(resolved)
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
    let output = run_cmd("mise", ["which", "bun"], CmdStatus::Success)
        .output()
        .ok()?;
    let path = output.stdout().ok()?;
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
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
        CmdStatus::IgnoreStatus,
    )
    .output()?;

    if !output.success() {
        let stderr = output.stderr().unwrap_or_default();
        bail!("bun pm view {name} time --json failed: {stderr}");
    }

    let stdout = output.stdout()?;
    let timestamps_by_version: BunTimeMap = serde_json::from_str(stdout)
        .with_context(|| format!("failed to parse bun pm view JSON for {name}"))?;

    let releases = bun_semver_time_releases(name, &timestamps_by_version)?;
    Ok(release_age_secs_for_version(
        &releases,
        version,
        now_unix_secs,
    ))
}

fn bun_semver_time_releases(
    name: &str,
    timestamps_by_version: &BunTimeMap,
) -> Result<Vec<SemverTimestamp>> {
    if timestamps_by_version.is_empty() {
        anyhow::bail!("bun pm view time JSON is empty for {name}");
    }

    parse_semver_time_releases(PLUGIN.id(), name, timestamps_by_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pm_ls_root_object_shape() {
        let raw = r#"{
          "dependencies": {
            "npm": { "version": "11.12.0" },
            "typescript": { "version": "5.9.3" }
          }
        }"#;

        let parsed = parse_bun_pm_ls_json(raw).expect("should parse");
        assert_eq!(parsed.get("npm").map(String::as_str), Some("11.12.0"));
        assert_eq!(parsed.get("typescript").map(String::as_str), Some("5.9.3"));
    }

    #[test]
    fn parse_pm_ls_array_shape() {
        let raw = r#"[
          {
            "dependencies": {
              "npm": { "version": "11.12.0" }
            }
          },
          {
            "dependencies": {
              "typescript": { "version": "5.9.3" }
            }
          }
        ]"#;

        let parsed = parse_bun_pm_ls_json(raw).expect("should parse");
        assert_eq!(parsed.get("npm").map(String::as_str), Some("11.12.0"));
        assert_eq!(parsed.get("typescript").map(String::as_str), Some("5.9.3"));
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

    #[test]
    fn selected_update_args_keep_min_age_by_default() {
        assert_eq!(
            bun_selected_update_args("typescript", "5.9.3", "604800", false),
            vec![
                "update".to_string(),
                "-g".to_string(),
                "typescript@5.9.3".to_string(),
                "--minimum-release-age".to_string(),
                "604800".to_string(),
            ]
        );
    }

    #[test]
    fn selected_update_args_omit_min_age_when_bypassed() {
        assert_eq!(
            bun_selected_update_args("typescript", "5.9.3", "604800", true),
            vec![
                "update".to_string(),
                "-g".to_string(),
                "typescript@5.9.3".to_string(),
            ]
        );
    }
}
