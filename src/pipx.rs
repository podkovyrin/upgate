use crate::Cli;
use crate::manager::Manager;
use crate::outcome::{ItemOutcome, emit_text_outcome};
use anyhow::{Context, Result, bail};
use pep440::Version;
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
        let resolved = pypi_resolve_target_with_min_age(&name, &current, now, min_age)?;

        let Some(target) = resolved else {
            let outcome = ItemOutcome::delayed_no_eligible(
                Manager::Pipx,
                name.clone(),
                current.clone(),
                "pypi",
                format!("{}d", PIPX_DELAY_DAYS),
            );
            emit_text_outcome(&outcome);
            continue;
        };

        if target.version == current {
            let outcome =
                ItemOutcome::skipped_no_change(Manager::Pipx, name.clone(), current, "pypi");
            emit_text_outcome(&outcome);
            continue;
        }

        let outcome = if let (Some(age_secs), Some(skipped_ver)) = (
            target.skipped_latest_age_secs,
            target.skipped_latest_version.as_deref(),
        ) {
            ItemOutcome::update_with_delayed_latest(
                Manager::Pipx,
                name.clone(),
                current,
                target.version,
                "pypi",
                skipped_ver.to_string(),
                human_age(age_secs),
                human_age(min_age.as_secs()),
            )
        } else {
            ItemOutcome::update(Manager::Pipx, name.clone(), current, target.version, "pypi")
        };

        emit_text_outcome(&outcome);
        upgradable.push(name);
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
        .with_context(|| format!("failed to run {} list --json", Manager::Pipx.as_str()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} list --json failed: {}",
            Manager::Pipx.as_str(),
            stderr.trim()
        );
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

struct PypiResolvedTarget {
    version: String,
    skipped_latest_age_secs: Option<u64>,
    skipped_latest_version: Option<String>,
}

fn pypi_resolve_target_with_min_age(
    pkg: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<Option<PypiResolvedTarget>> {
    let url = format!("https://pypi.org/pypi/{pkg}/json");
    let body = reqwest::blocking::get(&url)
        .with_context(|| format!("failed to GET {url}"))?
        .error_for_status()
        .with_context(|| format!("PyPI returned error for {pkg}"))?
        .text()
        .with_context(|| format!("failed to read PyPI response body for {pkg}"))?;

    let root: PypiRoot = serde_json::from_str(&body)
        .with_context(|| format!("failed to parse PyPI JSON for {pkg}"))?;

    let current_ver = Version::parse(current)
        .with_context(|| format!("failed to parse current PEP440 version for {pkg}: {current}"))?;

    let mut eligible: Option<(Version, String, u64)> = None;
    let mut newest_any: Option<(Version, String, u64)> = None;

    for (ver_str, files) in &root.releases {
        let Some(ver) = Version::parse(ver_str) else {
            continue;
        };

        let mut newest_file_ts = None::<u64>;
        for f in files {
            let raw = f
                .upload_time_iso_8601
                .as_deref()
                .or(f.upload_time.as_deref());

            if let Some(raw) = raw {
                let ts = parse_rfc3339_unix(raw).with_context(|| {
                    format!("invalid upload timestamp for {pkg}@{ver_str}: {raw}")
                })?;
                newest_file_ts = Some(newest_file_ts.map_or(ts, |curr| curr.max(ts)));
            }
        }

        let Some(ts) = newest_file_ts else {
            continue;
        };

        if newest_any.as_ref().is_none_or(|(curr, _, _)| ver > *curr) {
            newest_any = Some((ver.clone(), ver_str.clone(), ts));
        }

        if ver >= current_ver {
            let age_secs = now_unix_secs.saturating_sub(ts);
            if age_secs >= min_age.as_secs()
                && eligible.as_ref().is_none_or(|(curr, _, _)| ver > *curr)
            {
                eligible = Some((ver, ver_str.clone(), ts));
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

    let _ = root.info.version;
    Ok(Some(PypiResolvedTarget {
        version: eligible_str,
        skipped_latest_age_secs,
        skipped_latest_version,
    }))
}

fn parse_rfc3339_unix(raw: &str) -> Result<u64> {
    let dt = chrono::DateTime::parse_from_rfc3339(raw)
        .with_context(|| format!("invalid RFC3339 date: {raw}"))?;

    u64::try_from(dt.timestamp()).context("timestamp before UNIX_EPOCH")
}

fn run_pipx(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("pipx").args(args).output().with_context(|| {
        format!(
            "failed to run {} {}",
            Manager::Pipx.as_str(),
            args.join(" ")
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} {} failed: {}",
            Manager::Pipx.as_str(),
            args.join(" "),
            stderr.trim()
        );
    }

    Ok(output.stdout)
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
