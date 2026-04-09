use crate::Cli;
use anyhow::{Context, Result, bail};
use semver::Version;
use std::collections::BTreeMap;
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BUN_MIN_AGE_DAYS: u64 = 7;
const BUN_MIN_AGE_SECS: &str = "604800";

#[derive(Debug)]
struct OutdatedEntry {
    current: String,
}

pub(crate) fn run(cli: &Cli) -> Result<()> {
    let min_age = Duration::from_secs(BUN_MIN_AGE_DAYS * 24 * 60 * 60);
    let bun = bun_executable();

    let outdated = bun_outdated_global(&bun)?;
    if outdated.is_empty() {
        return Ok(());
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX_EPOCH")?
        .as_secs();

    let global_cwd = bun_global_cwd()?;

    for (name, entry) in &outdated {
        let from = version_label(&entry.current);

        let resolved =
            bun_resolve_target_with_min_age(&bun, &global_cwd, name, &entry.current, now, min_age)?;

        let Some(target) = resolved else {
            println!(
                "bun: {name} {from} -> {from} (delayed, no eligible release >= current within 7d window, source: bun)"
            );
            continue;
        };

        if target.version == entry.current {
            continue;
        }

        let to = version_label(&target.version);
        if let (Some(age_secs), Some(skipped_ver)) = (
            target.skipped_latest_age_secs,
            target.skipped_latest_version.as_deref(),
        ) {
            println!(
                "bun: {name} {from} -> {to} (source: bun; latest {} delayed: {} < {})",
                version_label(skipped_ver),
                human_age(age_secs),
                human_age(min_age.as_secs())
            );
        } else {
            println!("bun: {name} {from} -> {to} (source: bun)");
        }
    }

    if cli.dry_run {
        return Ok(());
    }

    run_bun(
        &bun,
        &["update", "-g", "--minimum-release-age", BUN_MIN_AGE_SECS],
    )?;

    Ok(())
}

fn bun_outdated_global(bun: &str) -> Result<BTreeMap<String, OutdatedEntry>> {
    let output = run_bun_raw(bun, &["outdated", "-g"])?;

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

    parse_bun_outdated_table(&stdout)
}

fn parse_bun_outdated_table(stdout: &str) -> Result<BTreeMap<String, OutdatedEntry>> {
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

    Ok(out)
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
    version: String,
    skipped_latest_age_secs: Option<u64>,
    skipped_latest_version: Option<String>,
}

fn bun_resolve_target_with_min_age(
    bun: &str,
    global_cwd: &str,
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<Option<BunResolvedTarget>> {
    let output = run_bun_raw(
        bun,
        &["pm", "view", name, "time", "--json", "--cwd", global_cwd],
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("bun pm view {name} time --json failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout).context("bun pm view output not UTF-8")?;
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .with_context(|| format!("failed to parse bun pm view JSON for {name}"))?;

    let obj = val
        .as_object()
        .with_context(|| format!("bun pm view time JSON is not an object for {name}"))?;

    let current_ver = Version::parse(current)
        .with_context(|| format!("failed to parse current semver for {name}: {current}"))?;

    let mut eligible: Option<(Version, String, u64)> = None;
    let mut newest_any: Option<(Version, String, u64)> = None;

    for (ver_str, ts_val) in obj {
        if ver_str == "created" || ver_str == "modified" {
            continue;
        }

        let Some(ts_raw) = ts_val.as_str() else {
            continue;
        };

        let version = match Version::parse(ver_str) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let ts = parse_rfc3339_unix(ts_raw)
            .with_context(|| format!("invalid bun timestamp for {name}@{ver_str}: {ts_raw}"))?;

        if newest_any
            .as_ref()
            .is_none_or(|(curr, _, _)| version > *curr)
        {
            newest_any = Some((version.clone(), ver_str.clone(), ts));
        }

        if version >= current_ver {
            let age_secs = now_unix_secs.saturating_sub(ts);
            if age_secs >= min_age.as_secs()
                && eligible.as_ref().is_none_or(|(curr, _, _)| version > *curr)
            {
                eligible = Some((version, ver_str.clone(), ts));
            }
        }
    }

    let Some((eligible_ver, eligible_str, _)) = eligible else {
        return Ok(None);
    };

    let (skipped_latest_age_secs, skipped_latest_version) =
        if let Some((latest_ver, latest_str, latest_ts)) = newest_any {
            if latest_ver > eligible_ver {
                (
                    Some(now_unix_secs.saturating_sub(latest_ts)),
                    Some(latest_str),
                )
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

    let _ = eligible_str;
    Ok(Some(BunResolvedTarget {
        version: eligible_ver.to_string(),
        skipped_latest_age_secs,
        skipped_latest_version,
    }))
}

fn parse_rfc3339_unix(raw: &str) -> Result<u64> {
    let dt = chrono::DateTime::parse_from_rfc3339(raw)
        .with_context(|| format!("invalid RFC3339 timestamp: {raw}"))?;

    u64::try_from(dt.timestamp()).context("bun publish timestamp is negative")
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

    "bun".to_string()
}

fn bun_from_mise() -> Option<String> {
    let output = Command::new("mise").args(["which", "bun"]).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8(output.stdout).ok()?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn run_bun_raw(bun: &str, args: &[&str]) -> Result<Output> {
    Command::new(bun)
        .args(args)
        .output()
        .with_context(|| format!("failed to run bun {}", args.join(" ")))
}

fn run_bun(bun: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = run_bun_raw(bun, args)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("bun {} failed: {}", args.join(" "), stderr.trim());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_outdated_table() {
        let raw = r#"bun outdated v1.3.11 (af24e281)
|----------------------------------------|
| Package  | Current | Update  | Latest  |
|----------|---------|---------|---------|
| npm      | 11.12.0 | 11.12.0 | 11.12.1 |
| typescript | 5.9.3 | 5.9.3 | 5.9.4 |
|----------------------------------------|
"#;

        let parsed = parse_bun_outdated_table(raw).expect("should parse");
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
        let parsed = parse_bun_outdated_table(raw).expect("should parse");
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
