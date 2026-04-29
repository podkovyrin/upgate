use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use pep440_rs::Version;

use crate::config::is_pinned;
use crate::managers::shared::emit_manager_level_error;
use crate::managers::shared::versioning::policy::{RecommendedOutcome, VersionPolicy};
#[allow(clippy::wildcard_imports)]
use crate::managers::*;
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};

const UV_MAX_PARALLEL_CHECKS: usize = 2;

#[derive(Clone)]
struct UvTool {
    name: String,
    current: String,
    python_path: String,
}

pub struct UvPlugin;

impl ManagerPlugin for UvPlugin {
    fn id(&self) -> &'static str {
        "uv"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn supports_version_policy(&self, policy: VersionPolicy) -> bool {
        policy == VersionPolicy::Disabled
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub static PLUGIN: UvPlugin = UvPlugin;

#[derive(Debug, serde::Deserialize)]
struct PypiRoot {
    #[serde(default)]
    releases: BTreeMap<String, Vec<PypiReleaseFile>>,
}

#[derive(Debug, serde::Deserialize)]
struct PypiReleaseFile {
    upload_time_iso_8601: Option<String>,
    upload_time: Option<String>,
}

type UvPlanItem = ResolvedPlanItem<UvResolvedTarget>;

struct RawUvPlanItem {
    tool: UvTool,
    target: Result<String, String>,
}

struct UvPlanParams<'a> {
    now_unix_secs: u64,
    min_age: Duration,
    min_age_raw: &'a str,
    max_parallel_checks: usize,
    outdated_latest: &'a BTreeMap<String, String>,
    pypi_client: Option<&'a reqwest::blocking::Client>,
    suppress_update_outcomes: bool,
    pinned: &'a BTreeSet<String>,
}

struct UvResolutionContext<'a> {
    pypi_client: Option<&'a reqwest::blocking::Client>,
    pypi_cache: &'a mut HashMap<String, Vec<Pep440Timestamp>>,
    now_unix_secs: u64,
    min_age: Duration,
}

impl UvResolutionContext<'_> {
    fn age_secs(&mut self, package: &str, version: &str) -> Option<u64> {
        resolve_pypi_age_secs(
            self.pypi_client,
            self.pypi_cache,
            package,
            version,
            self.now_unix_secs,
        )
    }
}

#[derive(Clone)]
struct UvResolvedTarget {
    recommendation: RecommendedOutcome,
    latest_version: Option<String>,
    latest_age_secs: Option<u64>,
    blocked_by_age_count: usize,
}

impl ResolvedPlanTarget for UvResolvedTarget {
    fn recommendation(&self) -> &RecommendedOutcome {
        &self.recommendation
    }

    fn latest_version(&self) -> Option<&str> {
        self.latest_version.as_deref()
    }

    fn latest_age_secs(&self) -> Option<u64> {
        self.latest_age_secs
    }

    fn blocked_by_age_count(&self) -> usize {
        self.blocked_by_age_count
    }
}

fn run(ctx: &ManagerCtx) -> Result<()> {
    run_manager_pipeline(ctx, scan, run_plan_apply)
}

fn run_plan_apply(ctx: &ManagerCtx) -> Result<()> {
    let min_age_raw = ctx.policy.min_release_age.cli_arg().to_string();
    let apply_min_age_raw = min_age_raw.clone();

    run_plan_apply_framework(
        ctx,
        PLUGIN.id(),
        PlanApplyFrameworkPolicy::SOFT_FETCH_STRICT_RESOLVE,
        || {
            let tool_dir = uv_tool_dir().context("failed to locate uv tool directory")?;
            uv_installed_tools(&tool_dir).context("failed to discover installed uv tools")
        },
        Vec::is_empty,
        move |installed, runtime| {
            let outdated_latest = match uv_outdated_latest_map() {
                Ok(map) => map,
                Err(err) => {
                    emit_manager_level_error(
                        PLUGIN.id(),
                        format!("failed to query latest uv tool versions: {err}"),
                    );
                    BTreeMap::new()
                }
            };

            let pypi_client = match crate::util::http::default_blocking_client() {
                Ok(client) => Some(client),
                Err(err) => {
                    emit_manager_level_error(
                        PLUGIN.id(),
                        format!("failed to initialize metadata HTTP client: {err}"),
                    );
                    None
                }
            };

            resolve_uv_plan(
                installed,
                &UvPlanParams {
                    now_unix_secs: runtime.now_unix_secs,
                    min_age: runtime.min_age,
                    min_age_raw: &min_age_raw,
                    max_parallel_checks: runtime.max_parallel_checks,
                    outdated_latest: &outdated_latest,
                    pypi_client: pypi_client.as_ref(),
                    suppress_update_outcomes: runtime.suppress_update_outcomes,
                    pinned: runtime.pinned,
                },
            )
            .context("planning execution failed")
        },
        |_installed, plan, runtime| {
            Ok(collect_apply_candidates_from_resolved_plan(
                PLUGIN.id(),
                plan,
                runtime.min_age,
                runtime.suppress_update_outcomes,
                runtime.pinned,
                false,
            ))
        },
        |ctx, _installed, candidates| {
            run_per_item_apply_candidate_flow(ctx, PLUGIN.id(), candidates, move |selected| {
                apply_uv_updates(&apply_min_age_raw, selected);
            })
        },
    )
}

fn resolve_uv_plan(installed: &[UvTool], params: &UvPlanParams<'_>) -> Result<Vec<UvPlanItem>> {
    let jobs = installed.to_vec();

    let threads = effective_parallelism(params.max_parallel_checks, UV_MAX_PARALLEL_CHECKS);
    let min_age_raw = params.min_age_raw;
    let raw_plan = run_indexed_parallel(jobs, threads, PLUGIN.id(), move |tool| {
        let target =
            uv_resolve_target_with_exclude_newer(&tool, min_age_raw).map_err(|err| err.to_string());

        RawUvPlanItem { tool, target }
    })?;

    let mut pypi_cache: HashMap<String, Vec<Pep440Timestamp>> = HashMap::new();

    Ok(raw_plan
        .into_iter()
        .map(|item| {
            let RawUvPlanItem { tool, target } = item;
            let resolved =
                if !params.suppress_update_outcomes && is_pinned(&tool.name, params.pinned) {
                    // Keep legacy uv output: pinned tools render as skipped at the
                    // installed version even though the dry-run resolver still ran.
                    target.map(|_target| UvResolvedTarget {
                        recommendation: RecommendedOutcome::CurrentNoNewer,
                        latest_version: None,
                        latest_age_secs: None,
                        blocked_by_age_count: 0,
                    })
                } else {
                    target.map(|target| {
                        let mut resolution_ctx = UvResolutionContext {
                            pypi_client: params.pypi_client,
                            pypi_cache: &mut pypi_cache,
                            now_unix_secs: params.now_unix_secs,
                            min_age: params.min_age,
                        };
                        uv_resolution_from_exclude_newer_target(
                            &mut resolution_ctx,
                            &tool.name,
                            &tool.current,
                            &target,
                            params.outdated_latest.get(&tool.name).map(String::as_str),
                        )
                    })
                };

            UvPlanItem::new(tool.name, tool.current, resolved)
        })
        .collect())
}

fn uv_resolution_from_exclude_newer_target(
    ctx: &mut UvResolutionContext<'_>,
    package: &str,
    current: &str,
    target: &str,
    latest: Option<&str>,
) -> UvResolvedTarget {
    if pep440_compare(target, current) == Some(Ordering::Less) {
        let latest_age_secs = latest.and_then(|latest| ctx.age_secs(package, latest));

        return UvResolvedTarget {
            recommendation: RecommendedOutcome::DelayedByAge,
            latest_version: latest.map(str::to_string),
            latest_age_secs,
            blocked_by_age_count: usize::from(latest_age_secs.is_some()),
        };
    }

    if target == current {
        if let Some(age_secs) = ctx.age_secs(package, current)
            && age_secs < ctx.min_age.as_secs()
        {
            let latest_age_secs =
                latest.map(|latest| ctx.age_secs(package, latest).unwrap_or(age_secs));

            return UvResolvedTarget {
                recommendation: RecommendedOutcome::DelayedByAge,
                latest_version: latest.map(str::to_string),
                latest_age_secs,
                blocked_by_age_count: 1,
            };
        }

        return UvResolvedTarget {
            recommendation: RecommendedOutcome::CurrentNoNewer,
            latest_version: None,
            latest_age_secs: None,
            blocked_by_age_count: 0,
        };
    }

    let delayed_latest = latest
        .filter(|latest| pep440_compare(latest, target) == Some(Ordering::Greater))
        .and_then(|latest| {
            let age_secs = ctx.age_secs(package, latest)?;
            Some((latest.to_string(), age_secs))
        });

    UvResolvedTarget {
        recommendation: RecommendedOutcome::Update {
            target_version: target.to_string(),
        },
        latest_version: delayed_latest
            .as_ref()
            .map(|(version, _age)| version.clone()),
        latest_age_secs: delayed_latest.as_ref().map(|(_version, age)| *age),
        blocked_by_age_count: usize::from(delayed_latest.is_some()),
    }
}

fn apply_uv_updates(min_age_raw: &str, upgradable: Vec<crate::managers::PlannedUpdate>) {
    for item in upgradable {
        let tool = item.name;
        let current = item.current;
        let target = item.target;
        let args = uv_tool_install_args(&tool, min_age_raw, item.gate_bypass.min_release_age);

        if let Err(err) = run_cmd("uv", &args, CmdStatus::Success).mutating().output() {
            emit_apply_error(PLUGIN.id(), tool, current, target, err);
        }
    }
}

fn uv_tool_install_args(
    tool: &str,
    min_age_raw: &str,
    bypass_min_release_age: bool,
) -> Vec<String> {
    let mut args = vec![
        "tool".to_string(),
        "install".to_string(),
        "--upgrade".to_string(),
    ];
    if !bypass_min_release_age {
        args.push("--exclude-newer".to_string());
        args.push(min_age_raw.to_string());
    }
    args.push(tool.to_string());
    args
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let Some(tool_dir) = soft_fail(
        uv_tool_dir(),
        PLUGIN.id(),
        "failed to locate uv tool directory",
    ) else {
        return Ok(());
    };

    let Some(installed) = soft_fail(
        uv_installed_tools(&tool_dir),
        PLUGIN.id(),
        "failed to discover installed uv tools",
    ) else {
        return Ok(());
    };
    if installed.is_empty() {
        return Ok(());
    }

    let now = verbose_now_unix_secs()?;

    let mut pypi_cache: HashMap<String, Vec<Pep440Timestamp>> = HashMap::new();
    let pypi_client = if now.is_some() {
        crate::util::http::default_blocking_client().ok()
    } else {
        None
    };

    for tool in installed {
        let age_secs = if let (Some(client), Some(now_unix_secs)) = (pypi_client.as_ref(), now) {
            pypi_release_age_secs(
                client,
                &mut pypi_cache,
                &tool.name,
                &tool.current,
                now_unix_secs,
            )
            .ok()
            .flatten()
        } else {
            None
        };

        emit_scan_current(
            PLUGIN.id(),
            tool.name,
            tool.current,
            age_secs,
            ctx.scan_old_age_threshold,
        );
    }

    Ok(())
}

fn uv_tool_dir() -> Result<String> {
    let output = run_cmd("uv", ["tool", "dir"], CmdStatus::Success).output()?;
    let path = output.stdout()?;
    if path.is_empty() {
        bail!("uv tool dir returned an empty path");
    }
    Ok(path.to_string())
}

fn uv_installed_tools(tool_dir: &str) -> Result<Vec<UvTool>> {
    let output = run_cmd(
        "uv",
        ["tool", "list", "--show-version-specifiers"],
        CmdStatus::Success,
    )
    .output()?;
    let text = output.stdout()?;

    let mut out = Vec::new();
    for line in text.lines() {
        if let Some((name, current)) = parse_installed_tool_line(line) {
            out.push(UvTool {
                python_path: uv_tool_python_path(tool_dir, &name),
                name,
                current,
            });
        }
    }

    if !out.is_empty() {
        return Ok(out);
    }

    // Fallback: in some uv versions/outputs, list formatting can change.
    // Read tool receipts directly to discover installed tools robustly.
    uv_installed_tools_from_receipts(tool_dir)
}

fn uv_installed_tools_from_receipts(tool_dir: &str) -> Result<Vec<UvTool>> {
    let root = Path::new(tool_dir);
    let read_dir = std::fs::read_dir(root)
        .with_context(|| format!("failed to read uv tool directory: {tool_dir}"))?;

    let mut out = Vec::new();

    for entry in read_dir {
        let entry = entry.context("failed to read uv tool directory entry")?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Some(name_os) = path.file_name() else {
            continue;
        };
        let Some(tool_name) = name_os.to_str() else {
            continue;
        };

        let receipt = path.join("uv-receipt.toml");
        if !receipt.exists() {
            continue;
        }

        let python_path = uv_tool_python_path(tool_dir, tool_name);
        let current = uv_python_package_version(&python_path, tool_name)?;

        out.push(UvTool {
            name: tool_name.to_string(),
            current,
            python_path,
        });
    }

    Ok(out)
}

fn uv_python_package_version(python_path: &str, package_name: &str) -> Result<String> {
    let script = r"import importlib.metadata as m
import sys
name = sys.argv[1]
print(m.version(name))
";

    let output = run_cmd(
        python_path,
        ["-c", script, package_name],
        CmdStatus::Success,
    )
    .output()?;
    let version = output.stdout()?;
    if version.is_empty() {
        bail!("python returned empty version for uv tool '{package_name}'");
    }

    Ok(version.to_string())
}

fn uv_outdated_latest_map() -> Result<BTreeMap<String, String>> {
    let output = run_cmd("uv", ["tool", "list", "--outdated"], CmdStatus::Success).output()?;
    let text = output.stdout()?;

    let mut out = BTreeMap::new();
    for line in text.lines() {
        if let Some((name, _current, latest)) = parse_outdated_tool_line(line)
            && let Some(latest) = latest
        {
            out.insert(name, latest);
        }
    }

    Ok(out)
}

fn parse_installed_tool_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return None;
    }

    let mut parts = trimmed.split_whitespace();
    let name = parts.next()?.to_string();
    let current_token = parts.next()?;
    let current = crate::util::text::strip_v_prefix(current_token).to_string();

    Some((name, current))
}

fn parse_outdated_tool_line(line: &str) -> Option<(String, String, Option<String>)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return None;
    }

    let mut parts = trimmed.split_whitespace();
    let name = parts.next()?.to_string();
    let current_token = parts.next()?;
    let current = crate::util::text::strip_v_prefix(current_token).to_string();

    let latest = bracket_value(trimmed, "latest: ");
    Some((name, current, latest))
}

fn bracket_value(line: &str, marker: &str) -> Option<String> {
    let token = format!("[{marker}");
    let start = line.find(&token)?;
    let after = &line[start + token.len()..];
    let end = after.find(']')?;
    Some(after[..end].to_string())
}

fn uv_resolve_target_with_exclude_newer(tool: &UvTool, min_age_raw: &str) -> Result<String> {
    let requirement = if Version::from_str(&tool.current).is_ok() {
        format!("{}>={}", tool.name, tool.current)
    } else {
        tool.name.clone()
    };

    let args = vec![
        "pip".to_string(),
        "install".to_string(),
        "--dry-run".to_string(),
        "-p".to_string(),
        tool.python_path.clone(),
        "--upgrade".to_string(),
        "--exclude-newer".to_string(),
        min_age_raw.to_string(),
        requirement,
    ];

    let output = run_cmd("uv", &args, CmdStatus::Success).output()?;

    // `uv pip install --dry-run` writes the plan to stderr in non-interactive mode.
    // Parse both streams to be robust across uv versions.
    let stdout = output.stdout()?;
    let stderr = output.stderr()?;
    let mut combined = String::with_capacity(stdout.len() + 1 + stderr.len());
    combined.push_str(stdout);
    if !combined.ends_with('\n') {
        combined.push('\n');
    }
    combined.push_str(stderr);

    let target = uv_parse_install_target_for_package(&combined, &tool.name)
        .unwrap_or_else(|| tool.current.clone());
    Ok(target)
}

fn uv_parse_install_target_for_package(text: &str, package_name: &str) -> Option<String> {
    let package_norm = normalize_package_name(package_name);

    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("+ ") else {
            continue;
        };

        let Some((name, version)) = rest.split_once("==") else {
            continue;
        };

        if normalize_package_name(name) == package_norm {
            return Some(version.trim().to_string());
        }
    }

    None
}

fn normalize_package_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

fn pep440_compare(lhs: &str, rhs: &str) -> Option<Ordering> {
    let lhs = Version::from_str(lhs).ok()?;
    let rhs = Version::from_str(rhs).ok()?;
    Some(lhs.cmp(&rhs))
}

fn resolve_pypi_age_secs(
    maybe_client: Option<&reqwest::blocking::Client>,
    cache: &mut HashMap<String, Vec<Pep440Timestamp>>,
    package: &str,
    version: &str,
    now_unix_secs: u64,
) -> Option<u64> {
    let client = maybe_client?;

    pypi_release_age_secs(client, cache, package, version, now_unix_secs).unwrap_or_default()
}

fn pypi_release_age_secs(
    pypi_client: &reqwest::blocking::Client,
    cache: &mut HashMap<String, Vec<Pep440Timestamp>>,
    package: &str,
    version: &str,
    now_unix_secs: u64,
) -> Result<Option<u64>> {
    let releases = pypi_release_timeline(pypi_client, cache, package)?;
    Ok(release_age_secs_for_pep440_version(
        releases,
        version,
        now_unix_secs,
    ))
}

fn pypi_release_timeline<'a>(
    pypi_client: &reqwest::blocking::Client,
    cache: &'a mut HashMap<String, Vec<Pep440Timestamp>>,
    package: &str,
) -> Result<&'a [Pep440Timestamp]> {
    if !cache.contains_key(package) {
        let url = format!("https://pypi.org/pypi/{package}/json");
        let body = pypi_client
            .get(&url)
            .send()
            .with_context(|| format!("failed to GET {url}"))?
            .error_for_status()
            .with_context(|| format!("PyPI returned error for {package}"))?
            .text()
            .with_context(|| format!("failed to read PyPI response body for {package}"))?;

        let root: PypiRoot = serde_json::from_str(&body)
            .with_context(|| format!("failed to parse PyPI JSON for {package}"))?;
        let releases = pypi_pep440_releases(package, &root)?;
        cache.insert(package.to_string(), releases);
    }

    Ok(cache.get(package).map_or(&[], Vec::as_slice))
}

fn pypi_pep440_releases(package: &str, root: &PypiRoot) -> Result<Vec<Pep440Timestamp>> {
    parse_pep440_release_timestamps(
        package,
        &root.releases,
        |file| file.upload_time_iso_8601.as_deref(),
        |file| file.upload_time.as_deref(),
    )
}

fn uv_tool_python_path(tool_dir: &str, tool_name: &str) -> String {
    let unix = PathBuf::from(tool_dir)
        .join(tool_name)
        .join("bin")
        .join("python");
    if unix.exists() {
        return unix.to_string_lossy().to_string();
    }

    // Default to unix path when not found so command errors remain actionable.
    Path::new(tool_dir)
        .join(tool_name)
        .join("bin")
        .join("python")
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_installed_line_with_required() {
        let line = "ruff v0.1.0 [required: ==0.1.0] [CPython 3.14.3]";
        let parsed = parse_installed_tool_line(line).expect("line should parse");
        assert_eq!(parsed.0, "ruff");
        assert_eq!(parsed.1, "0.1.0");
    }

    #[test]
    fn parse_outdated_line() {
        let line = "httpie v3.2.1 [latest: 3.2.4]";
        let parsed = parse_outdated_tool_line(line).expect("line should parse");
        assert_eq!(parsed.0, "httpie");
        assert_eq!(parsed.1, "3.2.1");
        assert_eq!(parsed.2.as_deref(), Some("3.2.4"));
    }

    #[test]
    fn parse_target_from_dry_run_plan() {
        let plan = "Would install 3 packages\n + idna==3.11\n + httpie==3.2.4\n";
        let target = uv_parse_install_target_for_package(plan, "httpie");
        assert_eq!(target.as_deref(), Some("3.2.4"));
    }

    #[test]
    fn parse_target_ignores_non_matching_plus_lines() {
        let plan = " + idna==3.11\n + httpie==3.2.4\n";
        let target = uv_parse_install_target_for_package(plan, "ruff");
        assert!(target.is_none());
    }

    #[test]
    fn normalize_package_name_collapses_separators() {
        assert_eq!(normalize_package_name("My_Pkg.Name"), "my-pkg-name");
    }

    #[test]
    fn tool_install_args_keep_exclude_newer_by_default() {
        assert_eq!(
            uv_tool_install_args("ruff", "7d", false),
            vec![
                "tool".to_string(),
                "install".to_string(),
                "--upgrade".to_string(),
                "--exclude-newer".to_string(),
                "7d".to_string(),
                "ruff".to_string(),
            ]
        );
    }

    #[test]
    fn tool_install_args_omit_exclude_newer_when_bypassed() {
        assert_eq!(
            uv_tool_install_args("ruff", "7d", true),
            vec![
                "tool".to_string(),
                "install".to_string(),
                "--upgrade".to_string(),
                "ruff".to_string(),
            ]
        );
    }

    #[test]
    fn delayed_latest_not_emitted_when_latest_is_older_than_target() {
        let mut outdated_latest = BTreeMap::new();
        outdated_latest.insert("ruff".to_string(), "0.15.8".to_string());

        let target = "0.15.9";
        let delayed_latest = outdated_latest
            .get("ruff")
            .filter(|latest| pep440_compare(latest, target) == Some(Ordering::Greater));

        assert!(delayed_latest.is_none());
    }
}
