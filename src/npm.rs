use crate::Cli;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const NPM_MIN_AGE_DAYS: u64 = 7;

#[derive(Debug, Deserialize)]
struct OutdatedEntry {
    current: String,
    latest: String,
}

pub(crate) fn run(cli: &Cli) -> Result<()> {
    let min_age = Duration::from_secs(NPM_MIN_AGE_DAYS * 24 * 60 * 60);

    let outdated = npm_outdated_global()?;
    if outdated.is_empty() {
        return Ok(());
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX_EPOCH")?
        .as_secs();

    for (name, entry) in &outdated {
        let latest_time = npm_version_publish_unix_secs(name, &entry.latest)?;
        let age_secs = now.saturating_sub(latest_time);

        let from = version_label(&entry.current);
        let to = version_label(&entry.latest);

        if age_secs >= min_age.as_secs() {
            println!("npm: {name} {from} -> {to} (source: npm)");
        } else {
            println!(
                "npm: {name} {from} -> {to} (delayed, {} < {}, source: npm)",
                human_age(age_secs),
                human_age(min_age.as_secs())
            );
        }
    }

    if cli.dry_run {
        return Ok(());
    }

    run_npm(&["-g", "update", "--min-release-age", "7"])?;

    Ok(())
}

fn npm_outdated_global() -> Result<BTreeMap<String, OutdatedEntry>> {
    let output = Command::new("npm")
        .args(["outdated", "-g", "--json"])
        .output()
        .context("failed to run npm outdated -g --json")?;

    // npm outdated returns exit code 1 when outdated packages exist.
    if !output.status.success() && output.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("npm outdated -g --json failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout).context("npm outdated output not UTF-8")?;
    if stdout.trim().is_empty() {
        return Ok(BTreeMap::new());
    }

    let parsed: BTreeMap<String, OutdatedEntry> =
        serde_json::from_str(&stdout).context("failed to parse npm outdated JSON")?;

    Ok(parsed)
}

fn npm_version_publish_unix_secs(name: &str, version: &str) -> Result<u64> {
    let spec = format!("{name}@{version}");
    let output = Command::new("npm")
        .args(["view", &spec, "time", "--json"])
        .output()
        .with_context(|| format!("failed to run npm view {spec} time --json"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("npm view {spec} time --json failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout).context("npm view output not UTF-8")?;
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .with_context(|| format!("failed to parse npm view JSON for {spec}"))?;

    let version_time = val
        .get(version)
        .and_then(serde_json::Value::as_str)
        .with_context(|| format!("npm view time missing timestamp for {spec}"))?;

    let dt = chrono::DateTime::parse_from_rfc3339(version_time)
        .with_context(|| format!("invalid RFC3339 timestamp for {spec}: {version_time}"))?;

    u64::try_from(dt.timestamp()).context("npm publish timestamp is negative")
}

fn run_npm(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("npm")
        .args(args)
        .output()
        .with_context(|| format!("failed to run npm {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("npm {} failed: {}", args.join(" "), stderr.trim());
    }

    Ok(output.stdout)
}

fn version_label(version: &str) -> String {
    if version.starts_with('v') {
        return version.to_string();
    }

    match version.chars().next() {
        Some(c) if c.is_ascii_digit() => format!("v{version}"),
        _ => version.to_string(),
    }
}

fn human_age(total_secs: u64) -> String {
    if total_secs < 60 {
        return format!("{total_secs}s");
    }

    if total_secs < 60 * 60 {
        return format!("{}m", total_secs / 60);
    }

    if total_secs < 24 * 60 * 60 {
        let hours = total_secs / 3600;
        let minutes = (total_secs % 3600) / 60;
        return if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{minutes}m")
        };
    }

    let days = total_secs / (24 * 60 * 60);
    let hours = (total_secs % (24 * 60 * 60)) / 3600;
    if hours == 0 {
        format!("{days}d")
    } else {
        format!("{days}d{hours}h")
    }
}
