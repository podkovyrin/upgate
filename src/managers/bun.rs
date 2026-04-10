use crate::Cli;
use crate::manager::Manager;
use crate::managers::common::{
    DelayedLatest, PlanDecision, PlanMeta, emit_plan_and_collect_upgradable,
};
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{run_command, run_command_checked_stdout};
use crate::util::timefmt::human_age;
use crate::util::timeparse::parse_rfc3339_unix;
use anyhow::{Context, Result, bail};
use semver::Version;
use std::collections::BTreeMap;
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BUN_MIN_AGE_DAYS: u64 = 7;
const BUN_MIN_AGE_SECS: u64 = BUN_MIN_AGE_DAYS * 24 * 60 * 60;
const BUN_MAX_PARALLEL_CHECKS: usize = 6;

#[derive(Debug)]
struct OutdatedEntry {
    current: String,
}

struct BunPlanItem {
    name: String,
    current: String,
    resolved: Result<Option<BunResolvedTarget>, String>,
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

    let jobs: Vec<(String, String)> = outdated
        .iter()
        .map(|(name, entry)| (name.clone(), entry.current.clone()))
        .collect();

    let threads = effective_parallelism(cli.max_parallel_checks, BUN_MAX_PARALLEL_CHECKS);
    let bun_path = bun.clone();
    let global_cwd_path = global_cwd.clone();
    let plan: Vec<BunPlanItem> = run_indexed_parallel(
        jobs,
        threads,
        "failed to build bun planning thread pool",
        "internal error: missing bun plan slot",
        move |(name, current)| {
            let resolved = bun_resolve_target_with_min_age(
                &bun_path,
                &global_cwd_path,
                &name,
                &current,
                now,
                min_age,
            )
            .map_err(|err| err.to_string());

            BunPlanItem {
                name,
                current,
                resolved,
            }
        },
    )?;

    let _upgradable = emit_plan_and_collect_upgradable(
        plan,
        |item| PlanMeta {
            manager: Manager::Bun,
            source: Manager::Bun.as_str(),
            name: item.name.clone(),
            current: item.current.clone(),
        },
        |item| {
            let target = match &item.resolved {
                Ok(Some(target)) => target,
                Ok(None) => {
                    return PlanDecision::DelayedNoEligible {
                        required_age: format!("{BUN_MIN_AGE_DAYS}d"),
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

    if let Err(err) = run_bun(
        &bun,
        &[
            "update",
            "-g",
            "--minimum-release-age",
            &BUN_MIN_AGE_SECS.to_string(),
        ],
    ) {
        let outcome = ItemOutcome::error(
            Manager::Bun,
            "*",
            "*",
            "*",
            Manager::Bun.as_str(),
            REASON_COMMAND_FAILED,
            err.to_string(),
        );
        emit_text_outcome(&outcome);
    }

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
        bail!("{} outdated -g failed: {err_text}", Manager::Bun.as_str());
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
        bail!(
            "{} pm view {name} time --json failed: {}",
            Manager::Bun.as_str(),
            stderr.trim()
        );
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

    Manager::Bun.as_str().to_string()
}

fn bun_from_mise() -> Option<String> {
    let output = Command::new("mise")
        .args(["which", Manager::Bun.as_str()])
        .output()
        .ok()?;
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
    let mut command = Command::new(bun);
    command.args(args);
    run_command(command)
}

fn run_bun(bun: &str, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new(bun);
    command.args(args);
    run_command_checked_stdout(command)
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
