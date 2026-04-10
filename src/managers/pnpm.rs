use crate::Cli;
use crate::manager::Manager;
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::run_command_checked_stdout;
use crate::util::timefmt::human_age;
use crate::util::timeparse::parse_rfc3339_unix;
use anyhow::{Context, Result, bail};
use semver::Version;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PNPM_MIN_AGE_DAYS: u64 = 7;
const PNPM_MAX_PARALLEL_CHECKS: usize = 6;

#[derive(Debug, Deserialize)]
struct OutdatedEntry {
    current: String,
}

struct PnpmPlanItem {
    name: String,
    current: String,
    resolved: Result<Option<PnpmResolvedTarget>, String>,
}

pub(crate) fn run(cli: &Cli) -> Result<()> {
    let min_age = Duration::from_secs(PNPM_MIN_AGE_DAYS * 24 * 60 * 60);

    let outdated = pnpm_outdated_global()?;
    if outdated.is_empty() {
        return Ok(());
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX_EPOCH")?
        .as_secs();

    let jobs: Vec<(String, String)> = outdated
        .iter()
        .map(|(name, entry)| (name.clone(), entry.current.clone()))
        .collect();

    let threads = effective_parallelism(cli.max_parallel_checks, PNPM_MAX_PARALLEL_CHECKS);
    let plan: Vec<PnpmPlanItem> = run_indexed_parallel(
        jobs,
        threads,
        "failed to build pnpm planning thread pool",
        "internal error: missing pnpm plan slot",
        |(name, current)| {
            let resolved = pnpm_resolve_target_with_min_age(&name, &current, now, min_age)
                .map_err(|err| err.to_string());

            PnpmPlanItem {
                name,
                current,
                resolved,
            }
        },
    )?;

    let mut upgradable: Vec<(String, String, String)> = Vec::new();

    for item in plan {
        let target = match item.resolved {
            Ok(target) => target,
            Err(err) => {
                let outcome = ItemOutcome::error(
                    Manager::Pnpm,
                    item.name,
                    item.current.clone(),
                    item.current,
                    Manager::Pnpm.as_str(),
                    REASON_COMMAND_FAILED,
                    err,
                );
                emit_text_outcome(&outcome);
                continue;
            }
        };

        let Some(target) = target else {
            let outcome = ItemOutcome::delayed_no_eligible(
                Manager::Pnpm,
                item.name,
                item.current,
                Manager::Pnpm.as_str(),
                format!("{}d", PNPM_MIN_AGE_DAYS),
            );
            emit_text_outcome(&outcome);
            continue;
        };

        if target.version == item.current {
            let outcome = ItemOutcome::skipped_no_change(
                Manager::Pnpm,
                item.name,
                item.current,
                Manager::Pnpm.as_str(),
            );
            emit_text_outcome(&outcome);
            continue;
        }

        let target_version = target.version;
        let outcome = if let (Some(age_secs), Some(skipped_ver)) = (
            target.skipped_latest_age_secs,
            target.skipped_latest_version.as_deref(),
        ) {
            ItemOutcome::update_with_delayed_latest(
                Manager::Pnpm,
                item.name.clone(),
                item.current.clone(),
                target_version.clone(),
                Manager::Pnpm.as_str(),
                skipped_ver.to_string(),
                human_age(age_secs),
                human_age(min_age.as_secs()),
            )
        } else {
            ItemOutcome::update(
                Manager::Pnpm,
                item.name.clone(),
                item.current.clone(),
                target_version.clone(),
                Manager::Pnpm.as_str(),
            )
        };

        emit_text_outcome(&outcome);
        upgradable.push((item.name, item.current, target_version));
    }

    if cli.dry_run {
        return Ok(());
    }

    for (name, current, version) in upgradable {
        let spec = format!("{name}@{version}");
        if let Err(err) = run_pnpm(&["add", "-g", &spec]) {
            let outcome = ItemOutcome::error(
                Manager::Pnpm,
                name,
                current,
                version,
                Manager::Pnpm.as_str(),
                REASON_COMMAND_FAILED,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
        }
    }

    Ok(())
}

fn pnpm_outdated_global() -> Result<BTreeMap<String, OutdatedEntry>> {
    let output = Command::new("pnpm")
        .args(["outdated", "-g", "--json"])
        .output()
        .with_context(|| {
            format!(
                "failed to run {} outdated -g --json",
                Manager::Pnpm.as_str()
            )
        })?;

    let stdout = String::from_utf8(output.stdout).context("pnpm outdated output not UTF-8")?;
    let stderr = String::from_utf8_lossy(&output.stderr);

    if is_no_importer_manifest_error(&stdout) || is_no_importer_manifest_error(&stderr) {
        return Ok(BTreeMap::new());
    }

    // Similar to npm, pnpm can return non-zero when outdated packages exist.
    if !output.status.success() && output.status.code() != Some(1) {
        let err_text = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        bail!(
            "{} outdated -g --json failed: {err_text}",
            Manager::Pnpm.as_str()
        );
    }

    if stdout.trim().is_empty() {
        return Ok(BTreeMap::new());
    }

    parse_pnpm_outdated_json(&stdout)
}

fn is_no_importer_manifest_error(text: &str) -> bool {
    text.contains("ERR_PNPM_NO_IMPORTER_MANIFEST_FOUND")
}

fn parse_pnpm_outdated_json(stdout: &str) -> Result<BTreeMap<String, OutdatedEntry>> {
    let val: serde_json::Value =
        serde_json::from_str(stdout).context("failed to parse pnpm outdated JSON")?;

    let mut out = BTreeMap::new();

    if let Some(obj) = val.as_object() {
        for (name, entry_val) in obj {
            let Some(current) = entry_val
                .as_object()
                .and_then(|o| o.get("current"))
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };

            out.insert(
                name.clone(),
                OutdatedEntry {
                    current: current.to_string(),
                },
            );
        }

        return Ok(out);
    }

    if let Some(items) = val.as_array() {
        for item in items {
            let Some(obj) = item.as_object() else {
                continue;
            };

            let name = obj
                .get("name")
                .or_else(|| obj.get("packageName"))
                .or_else(|| obj.get("package"))
                .and_then(serde_json::Value::as_str);

            let current = obj.get("current").and_then(serde_json::Value::as_str);

            let (Some(name), Some(current)) = (name, current) else {
                continue;
            };

            out.insert(
                name.to_string(),
                OutdatedEntry {
                    current: current.to_string(),
                },
            );
        }

        return Ok(out);
    }

    bail!("unsupported pnpm outdated JSON shape")
}

struct PnpmResolvedTarget {
    version: String,
    skipped_latest_age_secs: Option<u64>,
    skipped_latest_version: Option<String>,
}

fn pnpm_resolve_target_with_min_age(
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<Option<PnpmResolvedTarget>> {
    let output = Command::new("pnpm")
        .args(["view", name, "time", "--json"])
        .output()
        .with_context(|| {
            format!(
                "failed to run {} view {name} time --json",
                Manager::Pnpm.as_str()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} view {name} time --json failed: {}",
            Manager::Pnpm.as_str(),
            stderr.trim()
        );
    }

    let stdout = String::from_utf8(output.stdout).context("pnpm view output not UTF-8")?;
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .with_context(|| format!("failed to parse pnpm view JSON for {name}"))?;

    let obj = val
        .as_object()
        .with_context(|| format!("pnpm view time JSON is not an object for {name}"))?;

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
            .with_context(|| format!("invalid pnpm timestamp for {name}@{ver_str}: {ts_raw}"))?;

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

    let Some((eligible_ver, eligible_str, _eligible_ts)) = eligible else {
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
    Ok(Some(PnpmResolvedTarget {
        version: eligible_ver.to_string(),
        skipped_latest_age_secs,
        skipped_latest_version,
    }))
}

fn run_pnpm(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("pnpm");
    command.args(args);
    run_command_checked_stdout(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_outdated_object_shape() {
        let raw = r#"{
          "foo": { "current": "1.0.0" },
          "bar": { "current": "2.0.0" }
        }"#;

        let parsed = parse_pnpm_outdated_json(raw).expect("should parse");
        assert_eq!(parsed.get("foo").map(|e| e.current.as_str()), Some("1.0.0"));
        assert_eq!(parsed.get("bar").map(|e| e.current.as_str()), Some("2.0.0"));
    }

    #[test]
    fn parse_outdated_array_shape() {
        let raw = r#"[
          { "name": "foo", "current": "1.0.0" },
          { "packageName": "bar", "current": "2.0.0" }
        ]"#;

        let parsed = parse_pnpm_outdated_json(raw).expect("should parse");
        assert_eq!(parsed.get("foo").map(|e| e.current.as_str()), Some("1.0.0"));
        assert_eq!(parsed.get("bar").map(|e| e.current.as_str()), Some("2.0.0"));
    }

    #[test]
    fn no_importer_manifest_detection() {
        let stderr = "ERR_PNPM_NO_IMPORTER_MANIFEST_FOUND: no package.json";
        assert!(is_no_importer_manifest_error(stderr));
    }

    #[test]
    fn no_importer_manifest_detection_with_pnpm_styled_text() {
        let stdout = " ERR_PNPM_NO_IMPORTER_MANIFEST_FOUND  No package.json found";
        assert!(is_no_importer_manifest_error(stdout));
    }
}
