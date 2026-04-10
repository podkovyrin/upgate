use crate::Cli;
use crate::manager::Manager;
use crate::managers::common::{
    DelayedLatest, PlanDecision, PlanMeta, emit_plan_and_collect_upgradable,
};
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::run_command_checked_stdout;
use crate::util::timefmt::human_age;
use crate::util::timeparse::parse_rfc3339_unix;
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use semver::Version;
use std::collections::{BTreeMap, HashSet};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CARGO_MIN_AGE_DAYS: u64 = 7;
const CARGO_MAX_PARALLEL_CHECKS: usize = 4;

#[derive(Debug)]
struct InstalledCrate {
    version: String,
}

struct CargoPlanItem {
    name: String,
    current: String,
    resolved: Result<Option<CargoResolvedTarget>, String>,
}

pub(crate) fn run(cli: &Cli) -> Result<()> {
    let min_age = Duration::from_secs(CARGO_MIN_AGE_DAYS * 24 * 60 * 60);

    let installed = cargo_installed_crates()?;
    if installed.is_empty() {
        return Ok(());
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX_EPOCH")?
        .as_secs();

    let crates_client = crate::util::http::default_blocking_client()
        .context("failed to build crates.io HTTP client")?;

    let jobs: Vec<(String, String)> = installed
        .iter()
        .map(|(name, entry)| (name.clone(), entry.version.clone()))
        .collect();

    let threads = effective_parallelism(cli.max_parallel_checks, CARGO_MAX_PARALLEL_CHECKS);
    let plan: Vec<CargoPlanItem> = run_indexed_parallel(
        jobs,
        threads,
        "failed to build cargo planning thread pool",
        "internal error: missing cargo plan slot",
        |(name, current)| {
            let resolved =
                cargo_resolve_target_with_min_age(&crates_client, &name, &current, now, min_age)
                    .map_err(|err| err.to_string());

            CargoPlanItem {
                name,
                current,
                resolved,
            }
        },
    )?;

    let upgradable = emit_plan_and_collect_upgradable(
        plan,
        |item| PlanMeta {
            manager: Manager::Cargo,
            source: "crates.io",
            name: item.name.clone(),
            current: item.current.clone(),
        },
        |item| {
            let target = match &item.resolved {
                Ok(Some(target)) => target,
                Ok(None) => {
                    return PlanDecision::DelayedNoEligible {
                        required_age: format!("{CARGO_MIN_AGE_DAYS}d"),
                    };
                }
                Err(err) => return PlanDecision::Error(err.clone()),
            };

            if target.version == item.current {
                return PlanDecision::NoChange;
            }

            let delayed_latest = if let (Some(age_secs), Some(skipped_ver)) = (
                target.skipped_latest_age_secs,
                target.skipped_latest_version.as_deref(),
            ) {
                Some(DelayedLatest {
                    latest_version: skipped_ver.to_string(),
                    latest_age: human_age(age_secs),
                    required_age: human_age(min_age.as_secs()),
                })
            } else {
                None
            };

            PlanDecision::Update {
                target: target.version.clone(),
                delayed_latest,
            }
        },
    );

    if cli.dry_run {
        return Ok(());
    }

    for (name, current, version) in upgradable {
        let spec = format!("{name}@{version}");
        if let Err(err) = run_cargo(&["install", "--force", &spec]) {
            let outcome = ItemOutcome::error(
                Manager::Cargo,
                name,
                current,
                version,
                "crates.io",
                REASON_COMMAND_FAILED,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
        }
    }

    Ok(())
}

fn cargo_installed_crates() -> Result<BTreeMap<String, InstalledCrate>> {
    let stdout = run_cargo(&["install", "--list"])?;
    let text = String::from_utf8(stdout).context("cargo install --list output not UTF-8")?;

    Ok(parse_cargo_install_list(&text))
}

fn parse_cargo_install_list(text: &str) -> BTreeMap<String, InstalledCrate> {
    let mut out = BTreeMap::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.ends_with(':') {
            continue;
        }

        let Some((name, ver_raw)) = trimmed.trim_end_matches(':').split_once(" v") else {
            continue;
        };

        if name.is_empty() || ver_raw.is_empty() {
            continue;
        }

        out.insert(
            name.to_string(),
            InstalledCrate {
                version: ver_raw.to_string(),
            },
        );
    }

    out
}

struct CargoResolvedTarget {
    version: String,
    skipped_latest_age_secs: Option<u64>,
    skipped_latest_version: Option<String>,
}

fn cargo_resolve_target_with_min_age(
    crates_client: &Client,
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<Option<CargoResolvedTarget>> {
    let output = Command::new("cargo")
        .args(["search", name, "--limit", "1"])
        .output()
        .with_context(|| {
            format!(
                "failed to run {} search {name} --limit 1",
                Manager::Cargo.as_str()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} search {name} --limit 1 failed: {}",
            Manager::Cargo.as_str(),
            stderr.trim()
        );
    }

    let stdout = String::from_utf8(output.stdout).context("cargo search output not UTF-8")?;
    let latest = parse_cargo_search_latest_version(name, &stdout)?;

    let current_ver = Version::parse(current)
        .with_context(|| format!("failed to parse current semver for {name}: {current}"))?;

    let all_versions = crates_io_versions(crates_client, name)?;
    let mut newest_any: Option<(Version, String, u64)> = None;
    let mut eligible: Option<(Version, String, u64)> = None;

    for item in &all_versions {
        let version = match Version::parse(&item.version) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if newest_any
            .as_ref()
            .is_none_or(|(curr, _, _)| version > *curr)
        {
            newest_any = Some((version.clone(), item.version.clone(), item.created_at_unix));
        }

        if version >= current_ver {
            let age_secs = now_unix_secs.saturating_sub(item.created_at_unix);
            if age_secs >= min_age.as_secs()
                && eligible.as_ref().is_none_or(|(curr, _, _)| version > *curr)
            {
                eligible = Some((version, item.version.clone(), item.created_at_unix));
            }
        }
    }

    let Some((eligible_ver, eligible_str, _)) = eligible else {
        return Ok(None);
    };

    let mut skipped_latest_age_secs = None;
    let mut skipped_latest_version = None;

    if let Some((latest_ver, latest_str, latest_ts)) = newest_any
        && latest_ver > eligible_ver
    {
        skipped_latest_age_secs = Some(now_unix_secs.saturating_sub(latest_ts));
        skipped_latest_version = Some(latest_str);
    }

    // Keep the parsed search latest in scope to validate semver hygiene and avoid stale data.
    let _ = latest;
    let _ = eligible_str;

    Ok(Some(CargoResolvedTarget {
        version: eligible_ver.to_string(),
        skipped_latest_age_secs,
        skipped_latest_version,
    }))
}

fn parse_cargo_search_latest_version(crate_name: &str, stdout: &str) -> Result<Version> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("...") || trimmed.starts_with("note:") {
            continue;
        }

        let prefix = format!("{crate_name} = \"");
        if let Some(rest) = trimmed.strip_prefix(&prefix)
            && let Some((ver, _)) = rest.split_once('"')
        {
            return Version::parse(ver).with_context(|| {
                format!("failed to parse cargo search version for {crate_name}: {ver}")
            });
        }
    }

    bail!("failed to parse cargo search latest version for {crate_name}")
}

#[derive(Debug, serde::Deserialize)]
struct CratesIoRoot {
    #[serde(default)]
    versions: Vec<CratesIoVersion>,
}

#[derive(Debug, serde::Deserialize)]
struct CratesIoVersion {
    num: String,
    created_at: String,
    yanked: bool,
}

#[derive(Debug)]
struct CrateVersionItem {
    version: String,
    created_at_unix: u64,
}

fn crates_io_versions(client: &Client, crate_name: &str) -> Result<Vec<CrateVersionItem>> {
    let url = format!("https://crates.io/api/v1/crates/{crate_name}");

    let body = client
        .get(&url)
        .send()
        .with_context(|| format!("failed to GET {url}"))?
        .error_for_status()
        .with_context(|| format!("crates.io returned error for {crate_name}"))?
        .text()
        .with_context(|| format!("failed to read crates.io response body for {crate_name}"))?;

    let root: CratesIoRoot = serde_json::from_str(&body)
        .with_context(|| format!("failed to parse crates.io JSON for {crate_name}"))?;

    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for v in root.versions {
        if v.yanked {
            continue;
        }

        if !seen.insert(v.num.clone()) {
            continue;
        }

        let ts = parse_rfc3339_unix(&v.created_at).with_context(|| {
            format!(
                "invalid crates.io version timestamp for {crate_name}@{}: {}",
                v.num, v.created_at
            )
        })?;

        out.push(CrateVersionItem {
            version: v.num,
            created_at_unix: ts,
        });
    }

    Ok(out)
}

fn run_cargo(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("cargo");
    command.args(args);
    run_command_checked_stdout(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_install_list_entries() {
        let raw = r#"cargo-deny v0.19.0:
    cargo-deny
cbindgen v0.29.2:
    cbindgen
"#;

        let parsed = parse_cargo_install_list(raw);
        assert_eq!(
            parsed.get("cargo-deny").map(|e| e.version.as_str()),
            Some("0.19.0")
        );
        assert_eq!(
            parsed.get("cbindgen").map(|e| e.version.as_str()),
            Some("0.29.2")
        );
    }

    #[test]
    fn parse_search_latest() {
        let raw = "cargo-deny = \"0.19.0\"    # comment\n... and 12 crates more\n";
        let parsed = parse_cargo_search_latest_version("cargo-deny", raw).expect("should parse");
        assert_eq!(parsed, Version::new(0, 19, 0));
    }
}
