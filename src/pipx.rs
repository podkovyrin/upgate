use crate::Cli;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PIPX_DELAY_DAYS: u64 = 7;

#[derive(Debug, serde::Deserialize)]
struct PipxListRoot {
    #[serde(default)]
    venvs: BTreeMap<String, PipxVenv>,
}

#[derive(Debug, serde::Deserialize)]
struct PipxVenv {
    metadata: PipxMetadata,
}

#[derive(Debug, serde::Deserialize)]
struct PipxMetadata {
    main_package: PipxMainPackage,
}

#[derive(Debug, serde::Deserialize)]
struct PipxMainPackage {
    package: String,
    package_version: String,
}

#[derive(Debug, serde::Deserialize)]
struct PypiRoot {
    #[serde(default)]
    info: PypiInfo,
    #[serde(default)]
    releases: BTreeMap<String, Vec<PypiReleaseFile>>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct PypiInfo {
    version: String,
}

#[derive(Debug, serde::Deserialize)]
struct PypiReleaseFile {
    upload_time_iso_8601: Option<String>,
    upload_time: Option<String>,
}

pub(crate) fn run(cli: &Cli) -> Result<()> {
    let min_age = Duration::from_secs(PIPX_DELAY_DAYS * 24 * 60 * 60);

    let installed = pipx_installed_main_packages()?;
    if installed.is_empty() {
        return Ok(());
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX_EPOCH")?
        .as_secs();

    let mut upgradable = Vec::new();

    for (name, current) in installed {
        let (latest, latest_ts) = pypi_latest_version_and_time(&name)?;

        if latest == current {
            continue;
        }

        let age_secs = now.saturating_sub(latest_ts);
        let from = version_label(&current);
        let to = version_label(&latest);

        if age_secs >= min_age.as_secs() {
            println!("pipx: {name} {from} -> {to} (source: pypi)");
            upgradable.push(name);
        } else {
            println!(
                "pipx: {name} {from} -> {to} (delayed, {} < {}, source: pypi)",
                human_age(age_secs),
                human_age(min_age.as_secs())
            );
        }
    }

    if cli.dry_run {
        return Ok(());
    }

    for pkg in upgradable {
        run_pipx(&["upgrade", &pkg])?;
    }

    Ok(())
}

fn pipx_installed_main_packages() -> Result<BTreeMap<String, String>> {
    let output = Command::new("pipx")
        .args(["list", "--json"])
        .output()
        .context("failed to run pipx list --json")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("pipx list --json failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout).context("pipx list output not UTF-8")?;
    let root: PipxListRoot =
        serde_json::from_str(&stdout).context("failed to parse pipx list JSON")?;

    let mut out = BTreeMap::new();
    for (_venv_name, venv) in root.venvs {
        out.insert(
            venv.metadata.main_package.package,
            venv.metadata.main_package.package_version,
        );
    }

    Ok(out)
}

fn pypi_latest_version_and_time(pkg: &str) -> Result<(String, u64)> {
    let url = format!("https://pypi.org/pypi/{pkg}/json");
    let body = reqwest::blocking::get(&url)
        .with_context(|| format!("failed to GET {url}"))?
        .error_for_status()
        .with_context(|| format!("PyPI returned error for {pkg}"))?
        .text()
        .with_context(|| format!("failed to read PyPI response body for {pkg}"))?;

    let root: PypiRoot = serde_json::from_str(&body)
        .with_context(|| format!("failed to parse PyPI JSON for {pkg}"))?;

    let latest = root.info.version;
    if latest.is_empty() {
        bail!("PyPI latest version missing for {pkg}");
    }

    let files = root
        .releases
        .get(&latest)
        .with_context(|| format!("PyPI release metadata missing for {pkg}@{latest}"))?;

    let mut newest_ts = None::<u64>;
    for f in files {
        let raw = f
            .upload_time_iso_8601
            .as_deref()
            .or(f.upload_time.as_deref());

        if let Some(raw) = raw {
            let ts = parse_rfc3339_unix(raw)
                .with_context(|| format!("invalid upload timestamp for {pkg}@{latest}: {raw}"))?;
            newest_ts = Some(newest_ts.map_or(ts, |curr| curr.max(ts)));
        }
    }

    let newest_ts =
        newest_ts.with_context(|| format!("no upload timestamp found for {pkg}@{latest}"))?;

    Ok((latest, newest_ts))
}

fn parse_rfc3339_unix(raw: &str) -> Result<u64> {
    let dt = chrono::DateTime::parse_from_rfc3339(raw)
        .with_context(|| format!("invalid RFC3339 date: {raw}"))?;

    u64::try_from(dt.timestamp()).context("timestamp before UNIX_EPOCH")
}

fn run_pipx(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("pipx")
        .args(args)
        .output()
        .with_context(|| format!("failed to run pipx {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("pipx {} failed: {}", args.join(" "), stderr.trim());
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
