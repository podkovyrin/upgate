use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::managers::shared::versioning::policy::VersionPolicy;
#[allow(clippy::wildcard_imports)]
use crate::managers::*;
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};

const UV_MAX_PARALLEL_CHECKS: usize = 2;

#[derive(Clone)]
struct UvTool {
    name: String,
    current: String,
}

pub struct UvPlugin;

impl ManagerPlugin for UvPlugin {
    fn id(&self) -> &'static str {
        "uv"
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

type UvPlanItem = ResolvedPlanItem<AgeResolvedTarget>;

fn run(ctx: &ManagerCtx) -> Result<()> {
    run_manager_pipeline(ctx, scan, run_plan_apply)
}

fn run_plan_apply(ctx: &ManagerCtx) -> Result<()> {
    run_plan_apply_framework(
        ctx,
        PLUGIN.id(),
        PlanApplyFrameworkPolicy::SOFT_FETCH_STRICT_RESOLVE,
        || {
            let tool_dir = uv_tool_dir().context("failed to locate uv tool directory")?;
            uv_installed_tools(&tool_dir).context("failed to discover installed uv tools")
        },
        Vec::is_empty,
        |installed, runtime| {
            resolve_uv_plan(
                installed,
                runtime.now_unix_secs,
                runtime.min_age,
                runtime.max_parallel_checks,
                ctx.policy.version_policy,
            )
            .context("planning execution failed")
        },
        |_installed, plan, runtime| {
            Ok(collect_upgradable_from_resolved_plan(
                PLUGIN.id(),
                plan,
                runtime.min_age,
                runtime.suppress_update_outcomes,
                runtime.pinned,
            ))
        },
        |ctx, _installed, upgradable| {
            run_per_item_apply_flow(ctx, PLUGIN.id(), upgradable, apply_uv_updates)
        },
    )
}

fn resolve_uv_plan(
    installed: &[UvTool],
    now_unix_secs: u64,
    min_age: Duration,
    max_parallel_checks: usize,
    version_policy: VersionPolicy,
) -> Result<Vec<UvPlanItem>> {
    let pypi_client = crate::util::http::default_blocking_client()
        .context("failed to initialize metadata HTTP client")?;
    let jobs = installed.to_vec();

    let threads = effective_parallelism(max_parallel_checks, UV_MAX_PARALLEL_CHECKS);
    run_indexed_parallel(jobs, threads, PLUGIN.id(), move |tool| {
        let resolved = uv_resolve_target_with_min_age(
            &pypi_client,
            &tool.name,
            &tool.current,
            now_unix_secs,
            min_age,
            version_policy,
        )
        .map_err(|err| err.to_string());

        UvPlanItem::new(tool.name, tool.current, resolved)
    })
}

fn uv_resolve_target_with_min_age(
    pypi_client: &reqwest::blocking::Client,
    package: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
    version_policy: VersionPolicy,
) -> Result<AgeResolvedTarget> {
    let mut timeline_cache: HashMap<String, Vec<Pep440Timestamp>> = HashMap::new();
    let releases = pypi_release_timeline(pypi_client, &mut timeline_cache, package)?;

    let resolved =
        resolve_pep440_with_min_age(current, releases, now_unix_secs, min_age, version_policy)
            .with_context(|| format!("failed to resolve eligible PEP440 target for {package}"))?;

    Ok(resolved.into())
}

fn apply_uv_updates(upgradable: Vec<crate::managers::PlannedUpdate>) {
    for item in upgradable {
        let tool = item.name;
        let current = item.current;
        let target = item.target;
        let spec = format!("{tool}=={target}");

        if let Err(err) = run_cmd(
            "uv",
            ["tool", "install", "--upgrade", &spec],
            CmdStatus::Success,
        )
        .mutating()
        .output()
        {
            emit_apply_error(PLUGIN.id(), tool, current, target, err);
        }
    }
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
            out.push(UvTool { name, current });
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
        let base_url = pypi_base_url();
        let url = format!("{base_url}/pypi/{package}/json");
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

fn pypi_base_url() -> String {
    crate::util::http::env_base_url("UPNOW_UV_PYPI_BASE_URL", "https://pypi.org")
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
}
