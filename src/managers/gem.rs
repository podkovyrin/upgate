use crate::config::ManagerMode;
use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, emit_manager_level_error, emit_scan_current, verbose_now_unix_secs,
};
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::now_unix_secs;
use crate::util::timefmt::human_age;
use crate::util::timeparse::parse_rfc3339_unix;
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use semver::Version;
use std::collections::BTreeMap;
use std::time::Duration;

const GEM_MAX_PARALLEL_CHECKS: usize = 4;

pub(crate) struct GemPlugin;

impl ManagerPlugin for GemPlugin {
    fn id(&self) -> &'static str {
        "gem"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn default_mode(&self) -> ManagerMode {
        ManagerMode::Off
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub(crate) static PLUGIN: GemPlugin = GemPlugin;

#[derive(Debug, Clone)]
struct GemInstalledEntry {
    version: String,
    is_default: bool,
}

#[derive(Debug, Clone)]
struct OutdatedGem {
    current: String,
}

#[derive(Debug, Clone)]
enum GemDiscoveredItem {
    Managed { name: String, current: String },
}

struct GemPlanItem {
    name: String,
    current: String,
    resolved: Result<GemResolvedTarget, String>,
}

struct GemResolvedTarget {
    selected_version: Option<String>,
    latest_version: Option<String>,
    latest_age_secs: Option<u64>,
}

impl GemResolvedTarget {
    fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest> {
        DelayedLatest::from_too_fresh_latest(
            self.selected_version.as_deref(),
            self.latest_version.as_deref(),
            self.latest_age_secs,
            min_age,
        )
    }
}

#[derive(Debug, serde::Deserialize)]
struct RubyGemsVersionItem {
    number: String,
    created_at: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    ruby_version: Option<String>,
}

#[allow(clippy::too_many_lines)]
fn run(ctx: &ManagerCtx) -> Result<()> {
    if ctx.is_scan() {
        return scan(ctx);
    }

    let min_age = ctx.policy.min_release_age.duration();

    let installed = match gem_installed_inventory() {
        Ok(installed) => installed,
        Err(err) => {
            emit_gem_manager_error(format!("failed to read installed gems: {err}"));
            return Ok(());
        }
    };

    let outdated = match gem_outdated_map() {
        Ok(outdated) => outdated,
        Err(err) => {
            emit_gem_manager_error(format!("failed to query outdated gems: {err}"));
            return Ok(());
        }
    };

    if outdated.is_empty() {
        return Ok(());
    }

    let now = now_unix_secs()?;

    let ruby_runtime = match ruby_runtime_version() {
        Ok(runtime) => runtime,
        Err(err) => {
            emit_gem_manager_error(format!("failed to detect Ruby runtime version: {err}"));
            return Ok(());
        }
    };

    let rubygems_client = match crate::util::http::default_blocking_client() {
        Ok(client) => client,
        Err(err) => {
            emit_gem_manager_error(format!("failed to initialize metadata HTTP client: {err}"));
            return Ok(());
        }
    };

    let discovered: Vec<GemDiscoveredItem> = outdated
        .into_iter()
        .filter_map(|(name, item)| {
            let is_default = installed.get(&name).is_some_and(|g| g.is_default);
            if is_default {
                None
            } else {
                Some(GemDiscoveredItem::Managed {
                    name,
                    current: item.current,
                })
            }
        })
        .collect();

    let managed_jobs: Vec<(String, String)> = discovered
        .iter()
        .map(|item| match item {
            GemDiscoveredItem::Managed { name, current } => (name.clone(), current.clone()),
        })
        .collect();

    let threads = effective_parallelism(ctx.max_parallel_checks, GEM_MAX_PARALLEL_CHECKS);
    let plan: Vec<GemPlanItem> =
        run_indexed_parallel(managed_jobs, threads, PLUGIN.id(), |(name, current)| {
            let resolved = rubygems_resolve_target_with_min_age(
                &rubygems_client,
                &name,
                &current,
                &ruby_runtime,
                now,
                min_age,
            )
            .map_err(|err| err.to_string());

            GemPlanItem {
                name,
                current,
                resolved,
            }
        })?;

    let mut upgradable: Vec<(String, String, String)> = Vec::new();
    let mut plan_iter = plan.into_iter();

    for _item in discovered {
        let planned = plan_iter
            .next()
            .context("internal error: missing gem plan entry")?;

        match planned.resolved {
            Err(err) => {
                let outcome = ItemOutcome::error(
                    PLUGIN.id(),
                    planned.name,
                    planned.current.clone(),
                    planned.current,
                    "rubygems",
                    REASON_COMMAND_FAILED,
                    err,
                );
                emit_text_outcome(&outcome);
            }
            Ok(target) => {
                let delayed_latest = target.delayed_latest(min_age);

                if let Some(selected) = target.selected_version {
                    if selected == planned.current {
                        let outcome = ItemOutcome::skipped_no_change(
                            PLUGIN.id(),
                            planned.name,
                            planned.current,
                            "rubygems",
                        );
                        emit_text_outcome(&outcome);
                        continue;
                    }

                    let outcome = if let Some(DelayedLatest {
                        latest_version,
                        latest_age,
                        required_age,
                    }) = delayed_latest
                    {
                        ItemOutcome::update_with_delayed_latest(
                            PLUGIN.id(),
                            planned.name.clone(),
                            planned.current.clone(),
                            selected.clone(),
                            "rubygems",
                            latest_version,
                            latest_age,
                            required_age,
                        )
                    } else {
                        ItemOutcome::update(
                            PLUGIN.id(),
                            planned.name.clone(),
                            planned.current.clone(),
                            selected.clone(),
                            "rubygems",
                        )
                    };

                    emit_text_outcome(&outcome);
                    upgradable.push((planned.name, planned.current, selected));
                } else {
                    let outcome = if let Some(DelayedLatest {
                        latest_version,
                        latest_age,
                        required_age,
                    }) = delayed_latest
                    {
                        ItemOutcome::delayed_no_eligible_with_latest(
                            PLUGIN.id(),
                            planned.name,
                            planned.current,
                            "rubygems",
                            latest_version,
                            latest_age,
                            required_age,
                        )
                    } else {
                        ItemOutcome::delayed_no_eligible(
                            PLUGIN.id(),
                            planned.name,
                            planned.current,
                            "rubygems",
                            human_age(min_age.as_secs()),
                        )
                    };

                    emit_text_outcome(&outcome);
                }
            }
        }
    }

    if plan_iter.next().is_some() {
        bail!("internal error: unexpected extra gem plan entries");
    }

    if ctx.is_dry_run() {
        return Ok(());
    }

    for (name, current, target) in upgradable {
        if let Err(err) = run_cmd("gem", ["install", &name, "-v", &target], CmdStatus::Success)
            .mutating()
            .output()
        {
            let outcome = ItemOutcome::error(
                PLUGIN.id(),
                name,
                current,
                target,
                "rubygems",
                REASON_COMMAND_FAILED,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
        }
    }

    Ok(())
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let installed = match gem_installed_inventory() {
        Ok(installed) => installed,
        Err(err) => {
            emit_gem_manager_error(format!("failed to read installed gems: {err}"));
            return Ok(());
        }
    };

    if installed.is_empty() {
        return Ok(());
    }

    let now = verbose_now_unix_secs()?;

    let rubygems_client = if now.is_some() {
        crate::util::http::default_blocking_client().ok()
    } else {
        None
    };

    for (name, installed) in installed {
        if installed.is_default {
            continue;
        }

        let age_secs = if let (Some(client), Some(now_unix_secs)) = (rubygems_client.as_ref(), now)
        {
            rubygems_release_age_secs(client, &name, &installed.version, now_unix_secs)
                .ok()
                .flatten()
        } else {
            None
        };

        emit_scan_current(
            PLUGIN.id(),
            "rubygems",
            name,
            installed.version,
            age_secs,
            ctx.scan_old_age_threshold,
        );
    }

    Ok(())
}

fn gem_installed_inventory() -> Result<BTreeMap<String, GemInstalledEntry>> {
    let output = run_cmd("gem", ["list"], CmdStatus::Success).output()?;
    let text = output.stdout()?;

    Ok(parse_gem_installed_inventory(text))
}

fn parse_gem_installed_inventory(text: &str) -> BTreeMap<String, GemInstalledEntry> {
    let mut out = BTreeMap::new();

    for line in text.lines() {
        let trimmed = line.trim();
        let Some((name, rest)) = trimmed.split_once(" (") else {
            continue;
        };

        let Some(inner) = rest.strip_suffix(')') else {
            continue;
        };

        let mut parsed_version = None::<String>;
        let mut is_default = false;
        for part in inner.split(',').map(str::trim) {
            if let Some(v) = part.strip_prefix("default:") {
                is_default = true;
                let vv = v.trim();
                if !vv.is_empty() {
                    parsed_version = Some(vv.to_string());
                }
            } else if parsed_version.is_none()
                && part.chars().next().is_some_and(|c| c.is_ascii_digit())
            {
                parsed_version = Some(part.to_string());
            }
        }

        let Some(version) = parsed_version else {
            continue;
        };

        out.entry(name.to_string())
            .and_modify(|existing: &mut GemInstalledEntry| {
                if !existing.is_default && is_default {
                    existing.is_default = true;
                }
                if existing.version.is_empty() {
                    existing.version.clone_from(&version);
                }
            })
            .or_insert(GemInstalledEntry {
                version,
                is_default,
            });
    }

    out
}

fn gem_outdated_map() -> Result<BTreeMap<String, OutdatedGem>> {
    let output = run_cmd("gem", ["outdated"], CmdStatus::Success).output()?;
    let text = output.stdout()?;

    Ok(parse_gem_outdated_output(text))
}

fn parse_gem_outdated_output(text: &str) -> BTreeMap<String, OutdatedGem> {
    let mut out = BTreeMap::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Some((name, rest)) = trimmed.split_once(" (") else {
            continue;
        };

        let Some(inner) = rest.strip_suffix(')') else {
            continue;
        };

        let Some((current, _latest)) = inner.split_once(" < ") else {
            continue;
        };

        let current = current
            .trim()
            .strip_prefix("default:")
            .map_or_else(|| current.trim().to_string(), |v| v.trim().to_string());

        if name.trim().is_empty() || current.is_empty() {
            continue;
        }

        out.insert(name.trim().to_string(), OutdatedGem { current });
    }

    out
}

fn ruby_runtime_version() -> Result<Version> {
    let output = run_cmd("ruby", ["-e", "print RUBY_VERSION"], CmdStatus::Success).output()?;
    let stdout = output.stdout()?;

    parse_version_for_compare(stdout)
        .with_context(|| format!("failed to parse runtime Ruby version: {stdout}"))
}

fn rubygems_resolve_target_with_min_age(
    rubygems_client: &Client,
    gem_name: &str,
    current: &str,
    ruby_runtime: &Version,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<GemResolvedTarget> {
    let current_ver = parse_version_for_compare(current).with_context(|| {
        format!("failed to parse current gem version for {gem_name}: {current}")
    })?;

    let versions = rubygems_versions(rubygems_client, gem_name)?;

    let mut newest_any: Option<(Version, String, u64)> = None;
    let mut eligible: Option<(Version, String, u64)> = None;

    for item in versions {
        if item.prerelease {
            continue;
        }

        if !ruby_requirement_allows(ruby_runtime, item.ruby_version.as_deref()) {
            continue;
        }

        let Some(version) = parse_version_for_compare(&item.number) else {
            continue;
        };

        let released_at_unix = parse_rfc3339_unix(&item.created_at).with_context(|| {
            format!(
                "invalid RubyGems release timestamp for {gem_name}@{}: {}",
                item.number, item.created_at
            )
        })?;

        if newest_any
            .as_ref()
            .is_none_or(|(curr, _, _)| version > *curr)
        {
            newest_any = Some((version.clone(), item.number.clone(), released_at_unix));
        }

        if version >= current_ver {
            let age_secs = now_unix_secs.saturating_sub(released_at_unix);
            if age_secs >= min_age.as_secs()
                && eligible.as_ref().is_none_or(|(curr, _, _)| version > *curr)
            {
                eligible = Some((version, item.number, released_at_unix));
            }
        }
    }

    let selected_version = eligible.map(|(_, raw, _)| raw);
    let (latest_version, latest_age_secs) =
        if let Some((_latest, latest_raw, latest_released_at)) = newest_any {
            (
                Some(latest_raw),
                Some(now_unix_secs.saturating_sub(latest_released_at)),
            )
        } else {
            (None, None)
        };

    Ok(GemResolvedTarget {
        selected_version,
        latest_version,
        latest_age_secs,
    })
}

fn rubygems_release_age_secs(
    client: &Client,
    gem_name: &str,
    version: &str,
    now_unix_secs: u64,
) -> Result<Option<u64>> {
    let versions = rubygems_versions(client, gem_name)?;

    let ts = versions
        .into_iter()
        .find(|item| item.number == version)
        .map(|item| parse_rfc3339_unix(&item.created_at))
        .transpose()
        .with_context(|| format!("invalid RubyGems release timestamp for {gem_name}@{version}"))?;

    Ok(ts.map(|created| now_unix_secs.saturating_sub(created)))
}

fn rubygems_versions(client: &Client, gem_name: &str) -> Result<Vec<RubyGemsVersionItem>> {
    let url = format!("https://rubygems.org/api/v1/versions/{gem_name}.json");

    let body = client
        .get(&url)
        .send()
        .with_context(|| format!("failed to GET {url}"))?
        .error_for_status()
        .with_context(|| format!("RubyGems returned error for {gem_name}"))?
        .text()
        .with_context(|| format!("failed to read RubyGems response body for {gem_name}"))?;

    serde_json::from_str(&body)
        .with_context(|| format!("failed to parse RubyGems JSON for {gem_name}"))
}

fn ruby_requirement_allows(runtime: &Version, requirement_raw: Option<&str>) -> bool {
    let Some(raw) = requirement_raw.map(str::trim) else {
        return true;
    };

    if raw.is_empty() {
        return true;
    }

    for token in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let Some(matches) = requirement_token_matches(runtime, token) else {
            return false;
        };

        if !matches {
            return false;
        }
    }

    true
}

fn requirement_token_matches(runtime: &Version, token: &str) -> Option<bool> {
    let token = token.trim();

    if let Some(rest) = token.strip_prefix("~>") {
        let lower_raw = rest.trim();
        let lower = parse_version_for_compare(lower_raw)?;
        let upper = pessimistic_upper_bound(lower_raw)?;
        return Some(runtime >= &lower && runtime < &upper);
    }

    for op in [">=", "<=", "==", "!=", ">", "<", "="] {
        if let Some(rest) = token.strip_prefix(op) {
            let rhs = parse_version_for_compare(rest.trim())?;
            return Some(match op {
                ">=" => runtime >= &rhs,
                "<=" => runtime <= &rhs,
                "==" | "=" => runtime == &rhs,
                "!=" => runtime != &rhs,
                ">" => runtime > &rhs,
                "<" => runtime < &rhs,
                _ => false,
            });
        }
    }

    let rhs = parse_version_for_compare(token)?;
    Some(runtime == &rhs)
}

fn pessimistic_upper_bound(raw: &str) -> Option<Version> {
    let normalized = raw.trim().strip_prefix('v').unwrap_or(raw.trim());
    let segments: Vec<&str> = normalized.split('.').collect();
    if segments.is_empty() {
        return None;
    }

    if segments.iter().any(|s| s.is_empty()) {
        return None;
    }

    let original_len = segments.len();
    let mut nums: Vec<u64> = segments
        .iter()
        .map(|s| s.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;

    while nums.len() < 3 {
        nums.push(0);
    }

    if original_len <= 2 {
        nums[0] = nums[0].saturating_add(1);
        nums[1] = 0;
        nums[2] = 0;
    } else {
        nums[1] = nums[1].saturating_add(1);
        nums[2] = 0;
    }

    Version::parse(&format!("{}.{}.{}", nums[0], nums[1], nums[2])).ok()
}

fn parse_version_for_compare(raw: &str) -> Option<Version> {
    let trimmed = raw.strip_prefix('v').unwrap_or(raw);

    if let Ok(v) = Version::parse(trimmed) {
        return Some(v);
    }

    let parts: Vec<&str> = trimmed.split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }

    if parts.iter().any(|part| part.is_empty()) {
        return None;
    }

    if !parts
        .iter()
        .all(|part| part.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }

    let mut nums: Vec<u64> = parts
        .iter()
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;

    while nums.len() < 3 {
        nums.push(0);
    }

    Version::parse(&format!("{}.{}.{}", nums[0], nums[1], nums[2])).ok()
}

fn emit_gem_manager_error(detail: impl AsRef<str>) {
    emit_manager_level_error(PLUGIN.id(), "rubygems", detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gem_outdated_line() {
        let raw = "rake (13.2.1 < 13.3.1)\nbundler (2.6.9 < 4.0.10)\n";
        let parsed = parse_gem_outdated_output(raw);
        assert_eq!(
            parsed.get("rake").map(|g| g.current.as_str()),
            Some("13.2.1")
        );
        assert_eq!(
            parsed.get("bundler").map(|g| g.current.as_str()),
            Some("2.6.9")
        );
    }

    #[test]
    fn parse_gem_list_marks_default() {
        let raw = "bundler (default: 2.6.9)\nrake (13.2.1)\n";
        let parsed = parse_gem_installed_inventory(raw);
        assert!(parsed.get("bundler").is_some_and(|g| g.is_default));
        assert!(parsed.get("rake").is_some_and(|g| !g.is_default));
    }

    #[test]
    fn ruby_requirement_basic_range() {
        let runtime = Version::new(3, 4, 9);
        assert!(ruby_requirement_allows(&runtime, Some(">= 2.6, < 4.0")));
        assert!(!ruby_requirement_allows(&runtime, Some(">= 4.0")));
    }

    #[test]
    fn ruby_requirement_pessimistic_operator() {
        let runtime = Version::new(3, 4, 9);
        assert!(ruby_requirement_allows(&runtime, Some("~> 3.4")));
        assert!(!ruby_requirement_allows(&runtime, Some("~> 3.5")));
    }

    #[test]
    fn delayed_latest_hidden_when_latest_not_delayed() {
        let target = GemResolvedTarget {
            selected_version: Some("13.3.1".to_string()),
            latest_version: Some("13.3.1".to_string()),
            latest_age_secs: Some(10 * 24 * 60 * 60),
        };

        assert!(
            target
                .delayed_latest(Duration::from_secs(7 * 24 * 60 * 60))
                .is_none()
        );
    }

    #[test]
    fn delayed_latest_present_when_latest_too_fresh() {
        let target = GemResolvedTarget {
            selected_version: Some("13.2.1".to_string()),
            latest_version: Some("13.3.1".to_string()),
            latest_age_secs: Some(2 * 24 * 60 * 60),
        };

        assert!(
            target
                .delayed_latest(Duration::from_secs(7 * 24 * 60 * 60))
                .is_some()
        );
    }
}
