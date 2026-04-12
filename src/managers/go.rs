use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, emit_manager_level_error, emit_scan_current, verbose_now_unix_secs,
};
use crate::outcome::{
    ItemOutcome, REASON_COMMAND_FAILED, REASON_MISSING_METADATA, emit_text_outcome,
};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::now_unix_secs;
use crate::util::timefmt::human_age;
use crate::util::timeparse::parse_rfc3339_unix;
use anyhow::{Context, Result, bail};
use semver::Version;
use std::path::PathBuf;
use std::time::Duration;

const GO_MAX_PARALLEL_CHECKS: usize = 4;

pub(crate) struct GoPlugin;

impl ManagerPlugin for GoPlugin {
    fn id(&self) -> &'static str {
        "go"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub(crate) static PLUGIN: GoPlugin = GoPlugin;

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
    resolved: Result<GoResolvedTarget, String>,
}

struct GoResolvedTarget {
    selected_version: Option<String>,
    latest_version: Option<String>,
    latest_age_secs: Option<u64>,
}

impl GoResolvedTarget {
    fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest> {
        DelayedLatest::from_too_fresh_latest(
            self.selected_version.as_deref(),
            self.latest_version.as_deref(),
            self.latest_age_secs,
            min_age,
        )
    }
}

fn run(ctx: &ManagerCtx) -> Result<()> {
    if ctx.is_scan() {
        return scan(ctx);
    }

    let min_age = ctx.policy.min_release_age.duration();

    let discovered = match go_discover_global_tools() {
        Ok(discovered) => discovered,
        Err(err) => {
            emit_go_manager_error(format!("failed to discover global Go tools: {err}"));
            return Ok(());
        }
    };

    if discovered.is_empty() {
        return Ok(());
    }

    let now = now_unix_secs()?;
    let plan = resolve_go_plan(&discovered, now, min_age, ctx.max_parallel_checks)?;
    let upgradable = emit_go_plan_and_collect_upgradable(discovered, plan, min_age)?;

    if ctx.is_dry_run() {
        return Ok(());
    }

    apply_go_updates(upgradable);
    Ok(())
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let discovered = match go_discover_global_tools() {
        Ok(discovered) => discovered,
        Err(err) => {
            emit_go_manager_error(format!("failed to discover global Go tools: {err}"));
            return Ok(());
        }
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
    run_indexed_parallel(
        managed_jobs,
        threads,
        "failed to build go planning thread pool",
        "internal error: missing go plan slot",
        |tool| {
            let resolved = go_resolve_target_with_min_age(
                &tool.module_path,
                &tool.current_version,
                now_unix_secs,
                min_age,
            )
            .map_err(|err| err.to_string());

            GoPlanItem { tool, resolved }
        },
    )
}

#[allow(clippy::too_many_lines)]
fn emit_go_plan_and_collect_upgradable(
    discovered: Vec<GoDiscoveredTool>,
    plan: Vec<GoPlanItem>,
    min_age: Duration,
) -> Result<Vec<(String, String, String, String)>> {
    let mut upgradable = Vec::new();
    let mut plan_iter = plan.into_iter();

    for item in discovered {
        match item {
            GoDiscoveredTool::Skipped { name, reason } => {
                let outcome = ItemOutcome::skipped(
                    PLUGIN.id(),
                    name,
                    "*",
                    "*",
                    PLUGIN.id(),
                    REASON_MISSING_METADATA,
                    reason,
                );
                emit_text_outcome(&outcome);
            }
            GoDiscoveredTool::Managed(_) => {
                let planned = plan_iter
                    .next()
                    .context("internal error: missing go plan entry")?;
                let tool = planned.tool;

                match planned.resolved {
                    Err(err) => {
                        let outcome = ItemOutcome::error(
                            PLUGIN.id(),
                            tool.binary_name,
                            tool.current_version.clone(),
                            tool.current_version,
                            PLUGIN.id(),
                            REASON_COMMAND_FAILED,
                            err,
                        );
                        emit_text_outcome(&outcome);
                    }
                    Ok(target) => {
                        let delayed_latest = target.delayed_latest(min_age);

                        if let Some(selected) = target.selected_version {
                            if selected == tool.current_version {
                                let outcome = ItemOutcome::skipped_no_change(
                                    PLUGIN.id(),
                                    tool.binary_name,
                                    tool.current_version,
                                    PLUGIN.id(),
                                );
                                emit_text_outcome(&outcome);
                                continue;
                            }

                            let outcome = if let Some(DelayedLatest {
                                latest_version,
                                latest_age,
                                required_age,
                            }) = delayed_latest
                            {
                                ItemOutcome::update_with_delayed_latest(
                                    PLUGIN.id(),
                                    tool.binary_name.clone(),
                                    tool.current_version.clone(),
                                    selected.clone(),
                                    PLUGIN.id(),
                                    latest_version,
                                    latest_age,
                                    required_age,
                                )
                            } else {
                                ItemOutcome::update(
                                    PLUGIN.id(),
                                    tool.binary_name.clone(),
                                    tool.current_version.clone(),
                                    selected.clone(),
                                    PLUGIN.id(),
                                )
                            };

                            emit_text_outcome(&outcome);
                            upgradable.push((
                                tool.binary_name,
                                tool.current_version,
                                selected,
                                tool.install_path,
                            ));
                        } else {
                            let outcome = if let Some(DelayedLatest {
                                latest_version,
                                latest_age,
                                required_age,
                            }) = delayed_latest
                            {
                                ItemOutcome::delayed_no_eligible_with_latest(
                                    PLUGIN.id(),
                                    tool.binary_name,
                                    tool.current_version,
                                    PLUGIN.id(),
                                    latest_version,
                                    latest_age,
                                    required_age,
                                )
                            } else {
                                ItemOutcome::delayed_no_eligible(
                                    PLUGIN.id(),
                                    tool.binary_name,
                                    tool.current_version,
                                    PLUGIN.id(),
                                    human_age(min_age.as_secs()),
                                )
                            };

                            emit_text_outcome(&outcome);
                        }
                    }
                }
            }
        }
    }

    if plan_iter.next().is_some() {
        bail!("internal error: unexpected extra go plan entries");
    }

    Ok(upgradable)
}

fn apply_go_updates(upgradable: Vec<(String, String, String, String)>) {
    for (binary_name, current, target, install_path) in upgradable {
        let spec = format!("{install_path}@{target}");
        if let Err(err) = run_cmd("go", ["install", &spec], CmdStatus::Success) {
            let outcome = ItemOutcome::error(
                PLUGIN.id(),
                binary_name,
                current,
                target,
                PLUGIN.id(),
                REASON_COMMAND_FAILED,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
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
                    PLUGIN.id(),
                    REASON_MISSING_METADATA,
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
        .with_context(|| format!("failed to run go version -m {}", path.display()))?;

        if !output.success() {
            entries.push(GoDiscoveredTool::Skipped {
                name,
                reason: "missing go build metadata".to_string(),
            });
            continue;
        }

        let stdout = output
            .stdout()
            .with_context(|| format!("go version -m output not UTF-8 for {}", path.display()))?;

        let Some(info) = parse_go_version_m_output(&stdout) else {
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
) -> Result<GoResolvedTarget> {
    let current_ver = parse_go_semver(current).with_context(|| {
        format!("failed to parse current go semver for {module_path}: {current}")
    })?;

    let versions = go_module_versions(module_path)?;

    let mut newest_any: Option<(Version, String, u64)> = None;
    let mut eligible: Option<(Version, String, u64)> = None;

    for version_raw in versions {
        let Some(version) = parse_go_semver(&version_raw) else {
            continue;
        };

        let Some(released_at_unix) = go_module_version_release_unix(module_path, &version_raw)?
        else {
            continue;
        };

        if newest_any
            .as_ref()
            .is_none_or(|(curr, _, _)| version > *curr)
        {
            newest_any = Some((version.clone(), version_raw.clone(), released_at_unix));
        }

        if version >= current_ver {
            let age_secs = now_unix_secs.saturating_sub(released_at_unix);
            if age_secs >= min_age.as_secs()
                && eligible.as_ref().is_none_or(|(curr, _, _)| version > *curr)
            {
                eligible = Some((version, version_raw, released_at_unix));
            }
        }
    }

    let selected_version = eligible.map(|(_, raw, _)| raw);
    let (latest_version, latest_age_secs) =
        if let Some((_latest, latest_raw, latest_released_at)) = newest_any {
            (
                Some(latest_raw),
                Some(now_unix_secs.saturating_sub(latest_released_at)),
            )
        } else {
            (None, None)
        };

    Ok(GoResolvedTarget {
        selected_version,
        latest_version,
        latest_age_secs,
    })
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
    )?
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
    )?
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
    Version::parse(raw.strip_prefix('v').unwrap_or(raw)).ok()
}

fn emit_go_manager_error(detail: impl AsRef<str>) {
    emit_manager_level_error(PLUGIN.id(), PLUGIN.id(), detail);
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let target = GoResolvedTarget {
            selected_version: Some("v0.1.5".to_string()),
            latest_version: Some("v0.1.5".to_string()),
            latest_age_secs: Some(10 * 24 * 60 * 60),
        };

        assert!(
            target
                .delayed_latest(Duration::from_secs(7 * 24 * 60 * 60))
                .is_none()
        );
    }

    #[test]
    fn delayed_latest_present_when_latest_is_too_fresh_and_selected_is_older() {
        let target = GoResolvedTarget {
            selected_version: Some("v0.1.4".to_string()),
            latest_version: Some("v0.1.5".to_string()),
            latest_age_secs: Some(2 * 24 * 60 * 60),
        };

        assert!(
            target
                .delayed_latest(Duration::from_secs(7 * 24 * 60 * 60))
                .is_some()
        );
    }
}
