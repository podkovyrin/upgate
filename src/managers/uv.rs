use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, Pep440Timestamp, PlanDecision, PlanMeta, emit_manager_level_error,
    emit_plan_and_collect_upgradable, emit_scan_current, parse_pep440_release_timestamps,
    release_age_secs_for_pep440_version, verbose_now_unix_secs,
};
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::RunCmd;
use crate::util::time::now_unix_secs;
use crate::util::timefmt::human_age;
use anyhow::{Context, Result, bail};
use pep440::Version;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

const UV_MAX_PARALLEL_CHECKS: usize = 2;

struct UvTool {
    name: String,
    current: String,
    python_path: String,
}

pub(crate) struct UvPlugin;

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

pub(crate) static PLUGIN: UvPlugin = UvPlugin;

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
    let plan: Vec<UvPlanItem> = run_indexed_parallel(
        installed,
        threads,
        "failed to build uv planning thread pool",
        "internal error: missing uv plan slot",
        |tool| {
            let target = uv_resolve_target_with_exclude_newer(&tool, min_age_raw)
                .map_err(|err| err.to_string());
            UvPlanItem { tool, target }
        },
    )?;

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
        |item| PlanMeta {
            manager: PLUGIN.id(),
            source: PLUGIN.id(),
            name: item.tool.name.clone(),
            current: item.tool.current.clone(),
        },
        |item| {
            let target = match &item.target {
                Ok(target) => target,
                Err(err) => return PlanDecision::Error(err.clone()),
            };

            if pep440_compare(target, &item.tool.current) == Some(Ordering::Less) {
                let delayed_latest = outdated_latest.get(&item.tool.name).map(|latest| {
                    let latest_age = resolve_pypi_age_secs(
                        pypi_client.as_ref(),
                        &mut pypi_cache,
                        &item.tool.name,
                        latest,
                        now,
                    )
                    .unwrap_or(0);

                    DelayedLatest {
                        latest_version: latest.clone(),
                        latest_age: human_age(latest_age),
                        required_age: human_age(min_age.as_secs()),
                    }
                });

                return PlanDecision::DelayedNoEligible {
                    required_age: human_age(min_age.as_secs()),
                    delayed_latest,
                };
            }

            if target == &item.tool.current {
                if let Some(age_secs) = resolve_pypi_age_secs(
                    pypi_client.as_ref(),
                    &mut pypi_cache,
                    &item.tool.name,
                    &item.tool.current,
                    now,
                ) && age_secs < min_age.as_secs()
                {
                    let delayed_latest = outdated_latest.get(&item.tool.name).map(|latest| {
                        let latest_age = resolve_pypi_age_secs(
                            pypi_client.as_ref(),
                            &mut pypi_cache,
                            &item.tool.name,
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

                    return PlanDecision::DelayedNoEligible {
                        required_age: human_age(min_age.as_secs()),
                        delayed_latest,
                    };
                }

                return PlanDecision::NoChange;
            }

            let delayed_latest = if let Some(latest) = outdated_latest.get(&item.tool.name)
                && latest != target
            {
                let latest_age = resolve_pypi_age_secs(
                    pypi_client.as_ref(),
                    &mut pypi_cache,
                    &item.tool.name,
                    latest,
                    now,
                )
                .unwrap_or(0);

                Some(DelayedLatest {
                    latest_version: latest.clone(),
                    latest_age: human_age(latest_age),
                    required_age: human_age(min_age.as_secs()),
                })
            } else {
                None
            };

            PlanDecision::Update {
                target: target.clone(),
                delayed_latest,
            }
        },
    );

    if ctx.is_dry_run() {
        return Ok(());
    }

    if upgradable_tools.is_empty() {
        return Ok(());
    }

    for (tool, current, target) in upgradable_tools {
        let args = vec![
            "tool".to_string(),
            "install".to_string(),
            "--upgrade".to_string(),
            "--exclude-newer".to_string(),
            min_age_raw.to_string(),
            tool.clone(),
        ];

        if let Err(err) = RunCmd::Success.run("uv", &args) {
            let outcome = ItemOutcome::error(
                PLUGIN.id(),
                tool,
                current,
                target,
                PLUGIN.id(),
                REASON_COMMAND_FAILED,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
        }
    }

    Ok(())
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
    let path = RunCmd::Success.text("uv", ["tool", "dir"])?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        bail!("uv tool dir returned an empty path");
    }
    Ok(trimmed.to_string())
}

fn uv_installed_tools(tool_dir: &str) -> Result<Vec<UvTool>> {
    let text = RunCmd::Success.text("uv", ["tool", "list", "--show-version-specifiers"])?;

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

    let stdout = RunCmd::Success.text(python_path, ["-c", script, package_name])?;
    let version = stdout.trim();
    if version.is_empty() {
        bail!("python returned empty version for uv tool '{package_name}'");
    }

    Ok(version.to_string())
}

fn uv_outdated_latest_map() -> Result<BTreeMap<String, String>> {
    let text = RunCmd::Success.text("uv", ["tool", "list", "--outdated"])?;

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
    let requirement = if Version::parse(&tool.current).is_some() {
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

    let output = RunCmd::Success.run("uv", &args)?;
    let stdout = output.stdout;
    let stderr = output.stderr;

    // `uv pip install --dry-run` writes the plan to stderr in non-interactive mode.
    // Parse both streams to be robust across uv versions.
    let mut combined = String::new();
    combined
        .push_str(&String::from_utf8(stdout).context("uv pip install --dry-run stdout not UTF-8")?);
    if !combined.ends_with('\n') {
        combined.push('\n');
    }
    combined
        .push_str(&String::from_utf8(stderr).context("uv pip install --dry-run stderr not UTF-8")?);

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
    let lhs = Version::parse(lhs)?;
    let rhs = Version::parse(rhs)?;
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

    let windows = PathBuf::from(tool_dir)
        .join(tool_name)
        .join("Scripts")
        .join("python.exe");
    if windows.exists() {
        return windows.to_string_lossy().to_string();
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
}
