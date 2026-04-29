use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use semver::Version;

use crate::config::is_pinned;
use crate::managers::shared::versioning::policy::{
    GateBypass, OrderedCandidate, VersionPolicy, classify_semver_release, evaluate_candidates,
};
#[allow(clippy::wildcard_imports)]
use crate::managers::*;
use crate::outcome::{ItemOutcome, ReasonCode, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::parse_rfc3339_unix;

const GO_MAX_PARALLEL_CHECKS: usize = 4;

pub struct GoPlugin;

impl ManagerPlugin for GoPlugin {
    fn id(&self) -> &'static str {
        "go"
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

pub static PLUGIN: GoPlugin = GoPlugin;

#[derive(Debug, Clone)]
struct GoManagedTool {
    binary_name: String,
    install_path: String,
    module_path: String,
    current_version: String,
}

#[derive(Debug, Clone)]
enum GoDiscoveredTool {
    Managed(GoManagedTool),
    Skipped { name: String, reason: String },
}

struct GoPlanItem {
    tool: GoManagedTool,
    resolved: Result<VersionPolicyResolution, String>,
}

fn run(ctx: &ManagerCtx) -> Result<()> {
    run_manager_pipeline(ctx, scan, run_plan_apply)
}

fn run_plan_apply(ctx: &ManagerCtx) -> Result<()> {
    run_plan_apply_framework(
        ctx,
        PLUGIN.id(),
        PlanApplyFrameworkPolicy::SOFT_FETCH_STRICT_RESOLVE,
        || go_discover_global_tools().context("failed to discover global Go tools"),
        Vec::is_empty,
        |discovered, runtime| {
            resolve_go_plan(
                discovered,
                runtime.now_unix_secs,
                runtime.min_age,
                ctx.policy.version_policy,
                runtime.max_parallel_checks,
            )
            .context("planning execution failed")
        },
        |discovered, plan, runtime| {
            collect_go_plan_and_upgradable(
                discovered,
                plan,
                runtime.min_age,
                runtime.suppress_update_outcomes,
                runtime.pinned,
            )
        },
        |ctx, _discovered, candidates| {
            run_per_item_apply_candidate_flow(ctx, PLUGIN.id(), candidates, apply_go_updates)
        },
    )
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let Some(discovered) = soft_fail(
        go_discover_global_tools(),
        PLUGIN.id(),
        "failed to discover global Go tools",
    ) else {
        return Ok(());
    };

    if discovered.is_empty() {
        return Ok(());
    }

    let now = verbose_now_unix_secs()?;

    emit_go_scan_outcomes(discovered, now, ctx.scan_old_age_threshold);
    Ok(())
}

fn resolve_go_plan(
    discovered: &[GoDiscoveredTool],
    now_unix_secs: u64,
    min_age: Duration,
    version_policy: VersionPolicy,
    max_parallel_checks: usize,
) -> Result<Vec<GoPlanItem>> {
    let managed_jobs: Vec<GoManagedTool> = discovered
        .iter()
        .filter_map(|item| match item {
            GoDiscoveredTool::Managed(tool) => Some(tool.clone()),
            GoDiscoveredTool::Skipped { .. } => None,
        })
        .collect();

    let threads = effective_parallelism(max_parallel_checks, GO_MAX_PARALLEL_CHECKS);
    run_indexed_parallel(managed_jobs, threads, PLUGIN.id(), |tool| {
        let resolved = go_resolve_target_with_min_age(
            &tool.module_path,
            &tool.current_version,
            now_unix_secs,
            min_age,
            version_policy,
        )
        .map_err(|err| err.to_string());

        GoPlanItem { tool, resolved }
    })
}

fn collect_go_plan_and_upgradable(
    discovered: &[GoDiscoveredTool],
    plan: Vec<GoPlanItem>,
    min_age: Duration,
    suppress_update_outcomes: bool,
    pinned: &BTreeSet<String>,
) -> Result<Vec<ApplyCandidate>> {
    emit_go_discovery_skip_outcomes(discovered);

    let managed_count = discovered
        .iter()
        .filter(|item| matches!(item, GoDiscoveredTool::Managed(_)))
        .count();
    if managed_count != plan.len() {
        bail!(
            "internal error: mismatched go plan items: expected {managed_count}, got {}",
            plan.len()
        );
    }

    let mut install_path_by_name = std::collections::BTreeMap::new();
    let mut resolved_plan = Vec::with_capacity(plan.len());
    for planned in plan {
        let GoPlanItem { tool, resolved } = planned;
        if install_path_by_name
            .insert(tool.binary_name.clone(), tool.install_path)
            .is_some()
        {
            bail!("internal error: duplicate go tool '{}'", tool.binary_name);
        }

        resolved_plan.push(ResolvedPlanItem::new(
            tool.binary_name,
            tool.current_version,
            resolved,
        ));
    }

    let mut candidates = collect_apply_candidates_from_resolved_plan(
        PLUGIN.id(),
        resolved_plan,
        min_age,
        suppress_update_outcomes,
        pinned,
        true,
    );

    for candidate in &mut candidates {
        let install_path = install_path_by_name.get(&candidate.update().name).cloned();
        candidate.update_tree_mut(|item| {
            item.apply_spec_base = install_path.clone();

            if suppress_update_outcomes && is_pinned(&item.name, pinned) {
                item.delayed_latest = None;
            }
        });
    }

    Ok(candidates)
}

fn emit_go_discovery_skip_outcomes(discovered: &[GoDiscoveredTool]) {
    for item in discovered {
        let GoDiscoveredTool::Skipped { name, reason } = item else {
            continue;
        };

        let outcome = ItemOutcome::skipped(
            PLUGIN.id(),
            name.clone(),
            "*",
            "*",
            ReasonCode::MissingMetadata,
            reason.clone(),
        );
        emit_text_outcome(&outcome);
    }
}

fn apply_go_updates(upgradable: Vec<PlannedUpdate>) {
    for item in upgradable {
        let binary_name = item.name;
        let current = item.current;
        let target = item.target;
        let install_path = item.apply_spec_base.unwrap_or_else(|| binary_name.clone());
        let spec = format!("{install_path}@{target}");
        if let Err(err) = run_cmd("go", ["install", &spec], CmdStatus::Success)
            .mutating()
            .output()
        {
            emit_apply_error(PLUGIN.id(), binary_name, current, target, err);
        }
    }
}

fn emit_go_scan_outcomes(
    discovered: Vec<GoDiscoveredTool>,
    now_unix_secs: Option<u64>,
    old_threshold: Duration,
) {
    for item in discovered {
        match item {
            GoDiscoveredTool::Skipped { name, reason } => {
                let outcome = ItemOutcome::skipped(
                    PLUGIN.id(),
                    name,
                    "*",
                    "*",
                    ReasonCode::MissingMetadata,
                    reason,
                );
                emit_text_outcome(&outcome);
            }
            GoDiscoveredTool::Managed(tool) => {
                let age_secs = if let Some(now_unix_secs) = now_unix_secs {
                    go_module_version_release_unix(&tool.module_path, &tool.current_version)
                        .ok()
                        .flatten()
                        .map(|released| now_unix_secs.saturating_sub(released))
                } else {
                    None
                };

                emit_scan_current(
                    PLUGIN.id(),
                    tool.binary_name,
                    tool.current_version,
                    age_secs,
                    old_threshold,
                );
            }
        }
    }
}

fn go_discover_global_tools() -> Result<Vec<GoDiscoveredTool>> {
    let bin_dir = go_bin_dir()?;
    if !bin_dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&bin_dir)
        .with_context(|| format!("failed to read Go bin dir {}", bin_dir.display()))?
    {
        let entry = entry.context("failed to read Go bin directory entry")?;
        let path = entry.path();

        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };

        if !metadata.is_file() {
            continue;
        }

        let Some(name) = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };

        let output = run_cmd(
            "go",
            [
                std::ffi::OsStr::new("version"),
                std::ffi::OsStr::new("-m"),
                path.as_os_str(),
            ],
            CmdStatus::IgnoreStatus,
        )
        .output()?;

        if !output.success() {
            entries.push(GoDiscoveredTool::Skipped {
                name,
                reason: "missing go build metadata".to_string(),
            });
            continue;
        }

        let stdout = output.stdout()?;

        let Some(info) = parse_go_version_m_output(stdout) else {
            entries.push(GoDiscoveredTool::Skipped {
                name,
                reason: "missing go module/version metadata".to_string(),
            });
            continue;
        };

        if parse_go_semver(&info.version).is_none() {
            entries.push(GoDiscoveredTool::Skipped {
                name,
                reason: format!("unsupported Go module version '{}'", info.version),
            });
            continue;
        }

        entries.push(GoDiscoveredTool::Managed(GoManagedTool {
            binary_name: name,
            install_path: info.install_path,
            module_path: info.module_path,
            current_version: info.version,
        }));
    }

    entries.sort_by(|a, b| {
        let a_name = match a {
            GoDiscoveredTool::Managed(tool) => tool.binary_name.as_str(),
            GoDiscoveredTool::Skipped { name, .. } => name.as_str(),
        };
        let b_name = match b {
            GoDiscoveredTool::Managed(tool) => tool.binary_name.as_str(),
            GoDiscoveredTool::Skipped { name, .. } => name.as_str(),
        };
        a_name.cmp(b_name)
    });

    Ok(entries)
}

#[derive(Debug, serde::Deserialize)]
struct GoEnvJson {
    #[serde(rename = "GOPATH")]
    gopath: Option<String>,
}

fn go_bin_dir() -> Result<PathBuf> {
    if let Ok(gobin) = std::env::var("GOBIN") {
        let trimmed = gobin.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    if let Ok(gopath) = std::env::var("GOPATH") {
        let trimmed = gopath.trim();
        if !trimmed.is_empty()
            && let Some(first) = first_path_entry(trimmed)
        {
            return Ok(first.join("bin"));
        }
    }

    if let Ok(parsed) = run_cmd("go", ["env", "-json", "GOPATH"], CmdStatus::Success)
        .output()
        .and_then(|output| output.json::<GoEnvJson>())
        && let Some(gopath) = parsed.gopath
    {
        let trimmed = gopath.trim();
        if !trimmed.is_empty()
            && let Some(first) = first_path_entry(trimmed)
        {
            return Ok(first.join("bin"));
        }
    }

    let home = std::env::var("HOME").context("HOME env var is not set")?;
    Ok(PathBuf::from(home.trim()).join("go").join("bin"))
}

fn first_path_entry(raw: &str) -> Option<PathBuf> {
    std::env::split_paths(raw).next()
}

struct GoBuildInfo {
    install_path: String,
    module_path: String,
    version: String,
}

fn parse_go_version_m_output(text: &str) -> Option<GoBuildInfo> {
    let mut install_path = None::<String>;
    let mut module_path = None::<String>;
    let mut version = None::<String>;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        match parts[0] {
            "path" => {
                if parts.len() >= 2 {
                    install_path = Some(parts[1].to_string());
                }
            }
            "mod" => {
                if parts.len() >= 3 {
                    module_path = Some(parts[1].to_string());
                    version = Some(parts[2].to_string());
                }
            }
            _ => {}
        }
    }

    let install_path = install_path?;
    let module_path = module_path?;
    let version = version?;

    if version == "(devel)" || version == "devel" {
        return None;
    }

    Some(GoBuildInfo {
        install_path,
        module_path,
        version,
    })
}

fn go_resolve_target_with_min_age(
    module_path: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
    version_policy: VersionPolicy,
) -> Result<VersionPolicyResolution> {
    let current_ver = parse_go_semver(current).with_context(|| {
        format!("failed to parse current go semver for {module_path}: {current}")
    })?;
    let installed_class = classify_semver_release(current);

    let versions = go_module_versions(module_path)?;
    let mut candidates: Vec<OrderedCandidate<Version>> = Vec::new();

    for version_raw in versions {
        let Some(version) = parse_go_semver(&version_raw) else {
            continue;
        };

        let Some(released_at_unix) = go_module_version_release_unix(module_path, &version_raw)?
        else {
            continue;
        };

        candidates.push(OrderedCandidate {
            version: version_raw.clone(),
            parsed: version,
            release_class: classify_semver_release(&version_raw),
            published_unix: released_at_unix,
        });
    }

    let resolution = evaluate_candidates(
        &current_ver,
        &candidates,
        installed_class,
        version_policy,
        now_unix_secs,
        min_age,
        GateBypass::NONE,
    );

    Ok(resolution)
}

#[derive(Debug, serde::Deserialize)]
struct GoListVersionsResponse {
    #[serde(default, rename = "Versions")]
    versions: Vec<String>,
}

fn go_module_versions(module_path: &str) -> Result<Vec<String>> {
    let parsed: GoListVersionsResponse = run_cmd(
        "go",
        ["list", "-m", "-json", "-versions", module_path],
        CmdStatus::Success,
    )
    .output()?
    .json()?;

    Ok(parsed.versions)
}

#[derive(Debug, serde::Deserialize)]
struct GoListModuleResponse {
    #[serde(rename = "Time")]
    time: Option<String>,
}

fn go_module_version_release_unix(module_path: &str, version: &str) -> Result<Option<u64>> {
    let module_spec = format!("{module_path}@{version}");
    let parsed: GoListModuleResponse = run_cmd(
        "go",
        ["list", "-m", "-json", &module_spec],
        CmdStatus::Success,
    )
    .output()?
    .json()?;

    let Some(time_raw) = parsed.time.as_deref() else {
        return Ok(None);
    };

    let unix = parse_rfc3339_unix(time_raw).with_context(|| {
        format!("invalid go module release timestamp for {module_spec}: {time_raw}")
    })?;

    Ok(Some(unix))
}

fn parse_go_semver(raw: &str) -> Option<Version> {
    Version::parse(crate::util::text::strip_v_prefix(raw)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::shared::versioning::policy::{
        CandidateEvaluation, RecommendedOutcome, ReleaseClass, delayed_candidate_for_test,
    };

    fn resolution_for_delayed_note(
        selected: &str,
        latest_policy_eligible: &str,
        latest_age_secs: u64,
    ) -> VersionPolicyResolution {
        VersionPolicyResolution {
            configured_policy: VersionPolicy::Disabled,
            recommendation: RecommendedOutcome::Update {
                target_version: selected.to_string(),
            },
            latest_overall_version: Some(latest_policy_eligible.to_string()),
            latest_overall_age_secs: Some(latest_age_secs),
            latest_policy_eligible_version: Some(latest_policy_eligible.to_string()),
            latest_policy_eligible_age_secs: Some(latest_age_secs),
            latest_age_eligible_version: None,
            has_newer_versions: true,
            blocked_by_policy_count: 0,
            blocked_by_age_count: 1,
            evaluations: Vec::new(),
        }
    }

    fn candidate_evaluation(version: &str) -> CandidateEvaluation {
        CandidateEvaluation {
            version: version.to_string(),
            release_class: ReleaseClass::Final,
            age_secs: 10 * 24 * 60 * 60,
            policy_allowed: true,
            age_allowed: true,
            effective_allowed: true,
            policy_block_reason: None,
            policy_warning: None,
        }
    }

    fn resolution_with_candidates(selected: &str, candidates: &[&str]) -> VersionPolicyResolution {
        VersionPolicyResolution {
            configured_policy: VersionPolicy::Disabled,
            recommendation: RecommendedOutcome::Update {
                target_version: selected.to_string(),
            },
            latest_overall_version: candidates.first().map(|version| (*version).to_string()),
            latest_overall_age_secs: Some(10 * 24 * 60 * 60),
            latest_policy_eligible_version: candidates
                .first()
                .map(|version| (*version).to_string()),
            latest_policy_eligible_age_secs: Some(10 * 24 * 60 * 60),
            latest_age_eligible_version: candidates.first().map(|version| (*version).to_string()),
            has_newer_versions: !candidates.is_empty(),
            blocked_by_policy_count: 0,
            blocked_by_age_count: 0,
            evaluations: candidates
                .iter()
                .map(|version| candidate_evaluation(version))
                .collect(),
        }
    }

    fn managed_tool(binary_name: &str, install_path: &str) -> GoManagedTool {
        GoManagedTool {
            binary_name: binary_name.to_string(),
            install_path: install_path.to_string(),
            module_path: install_path.to_string(),
            current_version: "v1.0.0".to_string(),
        }
    }

    fn plan_item(tool: GoManagedTool, selected: &str, candidates: &[&str]) -> GoPlanItem {
        GoPlanItem {
            tool,
            resolved: Ok(resolution_with_candidates(selected, candidates)),
        }
    }

    #[test]
    fn parse_go_version_m_with_path_and_mod() {
        let raw = r"/usr/local/bin/gopls: go1.24.1
        path    golang.org/x/tools/gopls
        mod     golang.org/x/tools/gopls      v0.17.0  h1:hash
";

        let parsed = parse_go_version_m_output(raw).expect("should parse");
        assert_eq!(parsed.install_path, "golang.org/x/tools/gopls");
        assert_eq!(parsed.module_path, "golang.org/x/tools/gopls");
        assert_eq!(parsed.version, "v0.17.0");
    }

    #[test]
    fn parse_go_version_m_requires_mod_and_version() {
        let raw = r"/usr/local/bin/tool: go1.24.1
        path    example.com/tool/cmd/tool
";

        assert!(parse_go_version_m_output(raw).is_none());
    }

    #[test]
    fn parse_go_semver_with_v_prefix() {
        let parsed = parse_go_semver("v1.2.3").expect("should parse");
        assert_eq!(parsed, Version::new(1, 2, 3));
    }

    #[test]
    fn first_path_entry_takes_first_segment() {
        let joined = std::env::join_paths([PathBuf::from("/tmp/one"), PathBuf::from("/tmp/two")])
            .expect("join paths")
            .to_string_lossy()
            .to_string();

        let first = first_path_entry(&joined).expect("first path");
        assert_eq!(first, PathBuf::from("/tmp/one"));
    }

    #[test]
    fn delayed_latest_hidden_when_latest_is_old_enough() {
        let target = resolution_for_delayed_note("v0.1.5", "v0.1.5", 10 * 24 * 60 * 60);

        assert!(
            delayed_candidate_for_test(&target, Duration::from_secs(7 * 24 * 60 * 60)).is_none()
        );
    }

    #[test]
    fn delayed_latest_present_when_latest_is_too_fresh_and_selected_is_older() {
        let target = resolution_for_delayed_note("v0.1.4", "v0.1.5", 2 * 24 * 60 * 60);

        assert!(
            delayed_candidate_for_test(&target, Duration::from_secs(7 * 24 * 60 * 60)).is_some()
        );
    }

    #[test]
    fn exact_version_candidates_keep_go_install_path() {
        let install_path = "example.com/tools/cmd/tool";
        let tool = managed_tool("tool", install_path);
        let discovered = vec![GoDiscoveredTool::Managed(tool.clone())];
        let plan = vec![plan_item(tool, "v1.1.0", &["v1.2.0", "v1.1.0"])];

        let candidates = collect_go_plan_and_upgradable(
            &discovered,
            plan,
            Duration::ZERO,
            true,
            &BTreeSet::new(),
        )
        .expect("collect go candidates");

        let selected_alternate = candidates[0]
            .versions()
            .iter()
            .position(|version| version.update().target == "v1.2.0")
            .map(|idx| candidates[0].clone_selected_update(idx))
            .expect("alternate candidate version");

        assert_eq!(
            selected_alternate.apply_spec_base.as_deref(),
            Some(install_path)
        );
    }
}
