use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, Pep440Timestamp, PlanDecision, PlanMeta, emit_manager_level_error,
    emit_plan_and_collect_upgradable, emit_scan_current, parse_pep440_release_timestamps,
    release_age_secs_for_pep440_version, run_per_item_apply_flow, verbose_now_unix_secs,
};
use crate::outcome::{ItemOutcome, ReasonCode, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::human_age;
use crate::util::time::now_unix_secs;
use anyhow::{Context, Result, bail};
use pep440_rs::Version;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::str::FromStr;

const UV_MAX_PARALLEL_CHECKS: usize = 2;

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

struct UvPlanItem {
    tool: UvTool,
    target: Result<String, String>,
}

#[allow(clippy::too_many_lines)]
fn run(ctx: &ManagerCtx) -> Result<()> {
    if ctx.is_scan() {
        return scan(ctx);
    }

    let min_age_raw = ctx.policy.min_release_age.cli_arg();
    let min_age = ctx.policy.min_release_age.duration();
    let tool_dir = match uv_tool_dir() {
        Ok(tool_dir) => tool_dir,
        Err(err) => {
            emit_uv_manager_error(format!("failed to locate uv tool directory: {err}"));
            return Ok(());
        }
    };

    let installed = match uv_installed_tools(&tool_dir) {
        Ok(installed) => installed,
        Err(err) => {
            emit_uv_manager_error(format!("failed to discover installed uv tools: {err}"));
            return Ok(());
        }
    };
    if installed.is_empty() {
        return Ok(());
    }

    let outdated_latest = match uv_outdated_latest_map() {
        Ok(map) => map,
        Err(err) => {
            emit_uv_manager_error(format!("failed to query latest uv tool versions: {err}"));
            BTreeMap::new()
        }
    };

    let now = now_unix_secs()?;

    let threads = effective_parallelism(ctx.max_parallel_checks, UV_MAX_PARALLEL_CHECKS);
    let plan: Vec<UvPlanItem> = run_indexed_parallel(installed, threads, PLUGIN.id(), |tool| {
        let target =
            uv_resolve_target_with_exclude_newer(&tool, min_age_raw).map_err(|err| err.to_string());
        UvPlanItem { tool, target }
    })?;

    let mut pypi_cache: HashMap<String, Vec<Pep440Timestamp>> = HashMap::new();
    let pypi_client = match crate::util::http::default_blocking_client() {
        Ok(client) => Some(client),
        Err(err) => {
            emit_uv_manager_error(format!("failed to initialize metadata HTTP client: {err}"));
            None
        }
    };

    let upgradable_tools = emit_plan_and_collect_upgradable(
        plan,
        |item| {
            let UvPlanItem { tool, target } = item;

            let decision = match target {
                Ok(target) => {
                    if pep440_compare(&target, &tool.current) == Some(Ordering::Less) {
                        let delayed_latest = outdated_latest.get(&tool.name).and_then(|latest| {
                            let latest_age = resolve_pypi_age_secs(
                                pypi_client.as_ref(),
                                &mut pypi_cache,
                                &tool.name,
                                latest,
                                now,
                            );

                            DelayedLatest::from_too_fresh_latest(
                                None,
                                Some(latest.as_str()),
                                latest_age,
                                min_age,
                            )
                        });

                        PlanDecision::DelayedNoEligible {
                            required_age: human_age(min_age.as_secs()),
                            delayed_latest,
                        }
                    } else if target == tool.current {
                        if let Some(age_secs) = resolve_pypi_age_secs(
                            pypi_client.as_ref(),
                            &mut pypi_cache,
                            &tool.name,
                            &tool.current,
                            now,
                        ) && age_secs < min_age.as_secs()
                        {
                            let delayed_latest = outdated_latest.get(&tool.name).map(|latest| {
                                let latest_age = resolve_pypi_age_secs(
                                    pypi_client.as_ref(),
                                    &mut pypi_cache,
                                    &tool.name,
                                    latest,
                                    now,
                                )
                                .unwrap_or(age_secs);

                                DelayedLatest {
                                    latest_version: latest.clone(),
                                    latest_age: human_age(latest_age),
                                    required_age: human_age(min_age.as_secs()),
                                }
                            });

                            PlanDecision::DelayedNoEligible {
                                required_age: human_age(min_age.as_secs()),
                                delayed_latest,
                            }
                        } else {
                            PlanDecision::NoChange
                        }
                    } else {
                        let delayed_latest = if let Some(latest) = outdated_latest.get(&tool.name)
                            && pep440_compare(latest, &target) == Some(Ordering::Greater)
                        {
                            let latest_age = resolve_pypi_age_secs(
                                pypi_client.as_ref(),
                                &mut pypi_cache,
                                &tool.name,
                                latest,
                                now,
                            );

                            latest_age.and_then(|latest_age| {
                                (latest_age < min_age.as_secs()).then(|| DelayedLatest {
                                    latest_version: latest.clone(),
                                    latest_age: human_age(latest_age),
                                    required_age: human_age(min_age.as_secs()),
                                })
                            })
                        } else {
                            None
                        };

                        PlanDecision::Update {
                            target,
                            delayed_latest,
                        }
                    }
                }
                Err(err) => PlanDecision::Error(err),
            };

            (
                PlanMeta {
                    manager: PLUGIN.id(),
                    source: PLUGIN.id(),
                    name: tool.name,
                    current: tool.current,
                },
                decision,
            )
        },
        ctx.is_interactive_apply(),
        Some(&ctx.policy.pinned),
    );

    run_per_item_apply_flow(ctx, PLUGIN.id(), upgradable_tools, |selected| {
        apply_uv_updates(min_age_raw, selected);
    })?;

    Ok(())
}

fn apply_uv_updates(min_age_raw: &str, upgradable: Vec<crate::managers::common::PlannedUpdate>) {
    for item in upgradable {
        let tool = item.name;
        let current = item.current;
        let target = item.target;
        let args = vec![
            "tool".to_string(),
            "install".to_string(),
            "--upgrade".to_string(),
            "--exclude-newer".to_string(),
            min_age_raw.to_string(),
            tool.clone(),
        ];

        if let Err(err) = run_cmd("uv", &args, CmdStatus::Success).mutating().output() {
            let outcome = ItemOutcome::error(
                PLUGIN.id(),
                tool,
                current,
                target,
                PLUGIN.id(),
                ReasonCode::CommandFailed,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
        }
    }
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let tool_dir = match uv_tool_dir() {
        Ok(tool_dir) => tool_dir,
        Err(err) => {
            emit_uv_manager_error(format!("failed to locate uv tool directory: {err}"));
            return Ok(());
        }
    };

    let installed = match uv_installed_tools(&tool_dir) {
        Ok(installed) => installed,
        Err(err) => {
            emit_uv_manager_error(format!("failed to discover installed uv tools: {err}"));
            return Ok(());
        }
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
    let current = current_token
        .strip_prefix('v')
        .unwrap_or(current_token)
        .to_string();

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
    let current = current_token
        .strip_prefix('v')
        .unwrap_or(current_token)
        .to_string();

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

fn emit_uv_manager_error(detail: impl AsRef<str>) {
    emit_manager_level_error(PLUGIN.id(), PLUGIN.id(), detail);
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
    fn delayed_latest_not_emitted_when_latest_is_older_than_target() {
        let mut outdated_latest = BTreeMap::new();
        outdated_latest.insert("ruff".to_string(), "0.15.8".to_string());

        let target = "0.15.9";
        let delayed_latest = if let Some(latest) = outdated_latest.get("ruff")
            && pep440_compare(latest, target) == Some(Ordering::Greater)
        {
            Some(DelayedLatest {
                latest_version: latest.clone(),
                latest_age: human_age(2 * 24 * 60 * 60),
                required_age: human_age(7 * 24 * 60 * 60),
            })
        } else {
            None
        };

        assert!(delayed_latest.is_none());
    }
}
