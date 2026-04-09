use crate::Cli;
use crate::manager::Manager;
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::process::run_command_checked_stdout;
use crate::util::timefmt::human_age;
use crate::util::timeparse::parse_rfc3339_unix;
use anyhow::{Context, Result, bail};
use semver::Version;
use std::collections::BTreeMap;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const YARN_MIN_AGE_DAYS: u64 = 7;

#[derive(Debug)]
struct InstalledEntry {
    current: String,
}

pub(crate) fn run(cli: &Cli) -> Result<()> {
    let min_age = Duration::from_secs(YARN_MIN_AGE_DAYS * 24 * 60 * 60);

    let installed = yarn_global_installed()?;
    if installed.is_empty() {
        return Ok(());
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX_EPOCH")?
        .as_secs();

    let mut upgradable: Vec<(String, String, String)> = Vec::new();

    for (name, entry) in &installed {
        let resolved = match yarn_resolve_target_with_min_age(name, &entry.current, now, min_age) {
            Ok(resolved) => resolved,
            Err(err) => {
                let outcome = ItemOutcome::error(
                    Manager::Yarn,
                    name.clone(),
                    entry.current.clone(),
                    entry.current.clone(),
                    Manager::Yarn.as_str(),
                    REASON_COMMAND_FAILED,
                    err.to_string(),
                );
                emit_text_outcome(&outcome);
                continue;
            }
        };

        let Some(target) = resolved else {
            let outcome = ItemOutcome::delayed_no_eligible(
                Manager::Yarn,
                name.clone(),
                entry.current.clone(),
                Manager::Yarn.as_str(),
                format!("{}d", YARN_MIN_AGE_DAYS),
            );
            emit_text_outcome(&outcome);
            continue;
        };

        if target.version == entry.current {
            let outcome = ItemOutcome::skipped_no_change(
                Manager::Yarn,
                name.clone(),
                entry.current.clone(),
                Manager::Yarn.as_str(),
            );
            emit_text_outcome(&outcome);
            continue;
        }

        let outcome = if let (Some(age_secs), Some(skipped_ver)) = (
            target.skipped_latest_age_secs,
            target.skipped_latest_version.as_deref(),
        ) {
            ItemOutcome::update_with_delayed_latest(
                Manager::Yarn,
                name.clone(),
                entry.current.clone(),
                target.version.clone(),
                Manager::Yarn.as_str(),
                skipped_ver.to_string(),
                human_age(age_secs),
                human_age(min_age.as_secs()),
            )
        } else {
            ItemOutcome::update(
                Manager::Yarn,
                name.clone(),
                entry.current.clone(),
                target.version.clone(),
                Manager::Yarn.as_str(),
            )
        };

        emit_text_outcome(&outcome);
        upgradable.push((name.clone(), entry.current.clone(), target.version));
    }

    if cli.dry_run {
        return Ok(());
    }

    for (name, current, version) in upgradable {
        let spec = format!("{name}@{version}");
        if let Err(err) = run_yarn(&["global", "add", &spec]) {
            let outcome = ItemOutcome::error(
                Manager::Yarn,
                name,
                current,
                version,
                Manager::Yarn.as_str(),
                REASON_COMMAND_FAILED,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
        }
    }

    Ok(())
}

fn yarn_global_installed() -> Result<BTreeMap<String, InstalledEntry>> {
    let stdout = run_yarn(&["global", "list", "--depth=0"])?;
    let text = String::from_utf8(stdout).context("yarn global list output not UTF-8")?;

    Ok(parse_yarn_global_list(&text))
}

fn parse_yarn_global_list(text: &str) -> BTreeMap<String, InstalledEntry> {
    let mut out = BTreeMap::new();

    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("info \"") else {
            continue;
        };

        let Some((spec, _)) = rest.split_once('"') else {
            continue;
        };

        let Some((name, version)) = spec.rsplit_once('@') else {
            continue;
        };

        if name.is_empty() || version.is_empty() {
            continue;
        }

        out.insert(
            name.to_string(),
            InstalledEntry {
                current: version.to_string(),
            },
        );
    }

    out
}

struct YarnResolvedTarget {
    version: String,
    skipped_latest_age_secs: Option<u64>,
    skipped_latest_version: Option<String>,
}

fn yarn_resolve_target_with_min_age(
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<Option<YarnResolvedTarget>> {
    let stdout = run_yarn(&["info", name, "time", "--json"])?;
    let text = String::from_utf8(stdout).context("yarn info output not UTF-8")?;

    let obj = parse_yarn_inspect_object(&text, "time")?;

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

        let version = match Version::parse(&ver_str) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let ts = parse_rfc3339_unix(ts_raw)
            .with_context(|| format!("invalid yarn timestamp for {name}@{ver_str}: {ts_raw}"))?;

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
    Ok(Some(YarnResolvedTarget {
        version: eligible_ver.to_string(),
        skipped_latest_age_secs,
        skipped_latest_version,
    }))
}

fn parse_yarn_inspect_object(
    text: &str,
    field: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let val: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let Some(obj) = val.as_object() else {
            continue;
        };

        if obj.get("type").and_then(serde_json::Value::as_str) != Some("inspect") {
            continue;
        }

        let Some(data) = obj.get("data") else {
            continue;
        };

        let Some(data_obj) = data.as_object() else {
            bail!("yarn {field} payload is not an object");
        };

        return Ok(data_obj.clone());
    }

    bail!("failed to parse yarn {field} JSON payload")
}

fn run_yarn(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("yarn");
    command.args(args);
    run_command_checked_stdout(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_global_list_with_scoped_package() {
        let raw = r#"yarn global v1.22.22
info "npm@11.12.0" has binaries:
info "@scope/tool@2.3.4" has binaries:
Done in 0.05s.
"#;

        let parsed = parse_yarn_global_list(raw);
        assert_eq!(
            parsed.get("npm").map(|e| e.current.as_str()),
            Some("11.12.0")
        );
        assert_eq!(
            parsed.get("@scope/tool").map(|e| e.current.as_str()),
            Some("2.3.4")
        );
    }

    #[test]
    fn parse_inspect_data_line() {
        let raw = "{\"type\":\"inspect\",\"data\":{\"1.0.0\":\"2025-01-01T00:00:00.000Z\"}}\n";
        let parsed = parse_yarn_inspect_object(raw, "time").expect("should parse");
        assert_eq!(
            parsed.get("1.0.0").and_then(serde_json::Value::as_str),
            Some("2025-01-01T00:00:00.000Z")
        );
    }
}
