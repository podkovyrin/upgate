use crate::Cli;
use crate::manager::Manager;
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::process::{run_command_checked, run_command_checked_stdout};
use crate::util::timefmt::human_age;
use crate::util::timeparse::parse_rfc3339_unix;
use anyhow::{Context, Result, bail};
use pep440::Version;
use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const UV_DELAY: &str = "7d";
const UV_DELAY_DAYS: u64 = 7;
const UV_MAX_PARALLEL_CHECKS: usize = 2;

struct UvTool {
    name: String,
    current: String,
    python_path: String,
}

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

pub(crate) fn run(cli: &Cli) -> Result<()> {
    let min_age = Duration::from_secs(UV_DELAY_DAYS * 24 * 60 * 60);
    let tool_dir = uv_tool_dir()?;

    let installed = uv_installed_tools(&tool_dir)?;
    if installed.is_empty() {
        return Ok(());
    }

    let outdated_latest = uv_outdated_latest_map()?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX_EPOCH")?
        .as_secs();

    let effective_parallelism = cli.max_parallel_checks.clamp(1, UV_MAX_PARALLEL_CHECKS);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(effective_parallelism)
        .build()
        .context("failed to build uv planning thread pool")?;

    let plan_indexed: Vec<(usize, UvPlanItem)> = pool.install(|| {
        installed
            .into_par_iter()
            .enumerate()
            .map(|(index, tool)| {
                let target =
                    uv_resolve_target_with_exclude_newer(&tool).map_err(|err| err.to_string());
                (index, UvPlanItem { tool, target })
            })
            .collect()
    });

    let mut plan_slots: Vec<Option<UvPlanItem>> = (0..plan_indexed.len()).map(|_| None).collect();
    for (index, item) in plan_indexed {
        plan_slots[index] = Some(item);
    }

    let plan: Vec<UvPlanItem> = plan_slots
        .into_iter()
        .map(|item| item.context("internal error: missing uv plan slot"))
        .collect::<Result<Vec<_>>>()?;

    let mut upgradable_tools: Vec<(String, String, String)> = Vec::new();
    let mut pypi_cache: HashMap<String, PypiRoot> = HashMap::new();

    for item in plan {
        let tool = item.tool;
        let target = match item.target {
            Ok(target) => target,
            Err(err) => {
                let outcome = ItemOutcome::error(
                    Manager::Uv,
                    tool.name,
                    tool.current.clone(),
                    tool.current,
                    Manager::Uv.as_str(),
                    REASON_COMMAND_FAILED,
                    err,
                );
                emit_text_outcome(&outcome);
                continue;
            }
        };

        if pep440_compare(&target, &tool.current) == Some(Ordering::Less) {
            let outcome = ItemOutcome::delayed_no_eligible(
                Manager::Uv,
                tool.name,
                tool.current,
                Manager::Uv.as_str(),
                format!("{}d", UV_DELAY_DAYS),
            );
            emit_text_outcome(&outcome);
            continue;
        }

        if target == tool.current {
            if let Some(age_secs) =
                pypi_release_age_secs(&mut pypi_cache, &tool.name, &tool.current, now)?
                && age_secs < min_age.as_secs()
            {
                let outcome = ItemOutcome::delayed_no_eligible(
                    Manager::Uv,
                    tool.name,
                    tool.current,
                    Manager::Uv.as_str(),
                    format!("{}d", UV_DELAY_DAYS),
                );
                emit_text_outcome(&outcome);
            } else {
                let outcome = ItemOutcome::skipped_no_change(
                    Manager::Uv,
                    tool.name,
                    tool.current,
                    Manager::Uv.as_str(),
                );
                emit_text_outcome(&outcome);
            }
            continue;
        }

        let outcome = if let Some(latest) = outdated_latest.get(&tool.name)
            && latest != &target
        {
            let latest_age =
                pypi_release_age_secs(&mut pypi_cache, &tool.name, latest, now)?.unwrap_or(0);
            ItemOutcome::update_with_delayed_latest(
                Manager::Uv,
                tool.name.clone(),
                tool.current.clone(),
                target.clone(),
                Manager::Uv.as_str(),
                latest.clone(),
                human_age(latest_age),
                human_age(min_age.as_secs()),
            )
        } else {
            ItemOutcome::update(
                Manager::Uv,
                tool.name.clone(),
                tool.current.clone(),
                target.clone(),
                Manager::Uv.as_str(),
            )
        };

        emit_text_outcome(&outcome);
        upgradable_tools.push((tool.name, tool.current, target));
    }

    if cli.dry_run {
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
            UV_DELAY.to_string(),
            tool.clone(),
        ];

        if let Err(err) = run_uv_owned(&args) {
            let outcome = ItemOutcome::error(
                Manager::Uv,
                tool,
                current,
                target,
                Manager::Uv.as_str(),
                REASON_COMMAND_FAILED,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
        }
    }

    Ok(())
}

fn uv_tool_dir() -> Result<String> {
    let stdout = run_uv(&["tool", "dir"])?;
    let path = String::from_utf8(stdout).context("uv tool dir output not UTF-8")?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        bail!("uv tool dir returned an empty path");
    }
    Ok(trimmed.to_string())
}

fn uv_installed_tools(tool_dir: &str) -> Result<Vec<UvTool>> {
    let stdout = run_uv(&["tool", "list", "--show-version-specifiers"])?;
    let text = String::from_utf8(stdout).context("uv tool list output not UTF-8")?;

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
    let script = r#"import importlib.metadata as m
import sys
name = sys.argv[1]
print(m.version(name))
"#;

    let output = Command::new(python_path)
        .arg("-c")
        .arg(script)
        .arg(package_name)
        .output()
        .with_context(|| {
            format!("failed to run python at {python_path} to query package version")
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to query installed version for uv tool '{package_name}': {}",
            stderr.trim()
        );
    }

    let stdout =
        String::from_utf8(output.stdout).context("python version query output not UTF-8")?;
    let version = stdout.trim();
    if version.is_empty() {
        bail!("python returned empty version for uv tool '{package_name}'");
    }

    Ok(version.to_string())
}

fn uv_outdated_latest_map() -> Result<BTreeMap<String, String>> {
    let stdout = run_uv(&["tool", "list", "--outdated"])?;
    let text = String::from_utf8(stdout).context("uv tool list --outdated output not UTF-8")?;

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

fn uv_resolve_target_with_exclude_newer(tool: &UvTool) -> Result<String> {
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
        UV_DELAY.to_string(),
        requirement,
    ];

    let (stdout, stderr) = run_uv_owned_with_stderr(&args)?;

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

fn pypi_release_age_secs(
    cache: &mut HashMap<String, PypiRoot>,
    package: &str,
    version: &str,
    now_unix_secs: u64,
) -> Result<Option<u64>> {
    if !cache.contains_key(package) {
        let url = format!("https://pypi.org/pypi/{package}/json");
        let body = reqwest::blocking::get(&url)
            .with_context(|| format!("failed to GET {url}"))?
            .error_for_status()
            .with_context(|| format!("PyPI returned error for {package}"))?
            .text()
            .with_context(|| format!("failed to read PyPI response body for {package}"))?;

        let root: PypiRoot = serde_json::from_str(&body)
            .with_context(|| format!("failed to parse PyPI JSON for {package}"))?;
        cache.insert(package.to_string(), root);
    }

    let Some(root) = cache.get(package) else {
        return Ok(None);
    };

    let Some(files) = root.releases.get(version) else {
        return Ok(None);
    };

    let mut newest_ts = None::<u64>;
    for file in files {
        let raw = file
            .upload_time_iso_8601
            .as_deref()
            .or(file.upload_time.as_deref());

        if let Some(raw) = raw {
            let ts = parse_rfc3339_unix(raw).with_context(|| {
                format!("invalid upload timestamp for {package}@{version}: {raw}")
            })?;
            newest_ts = Some(newest_ts.map_or(ts, |curr| curr.max(ts)));
        }
    }

    Ok(newest_ts.map(|ts| now_unix_secs.saturating_sub(ts)))
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

fn run_uv(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("uv");
    command.args(args);
    run_command_checked_stdout(command)
}

fn run_uv_owned(args: &[String]) -> Result<Vec<u8>> {
    let mut command = Command::new("uv");
    command.args(args);
    run_command_checked_stdout(command)
}

fn run_uv_owned_with_stderr(args: &[String]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut command = Command::new("uv");
    command.args(args);
    let output = run_command_checked(command)?;
    Ok((output.stdout, output.stderr))
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
