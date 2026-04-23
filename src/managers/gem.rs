use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use semver::Version;

use crate::config::ManagerMode;
use crate::managers::shared::versioning::policy::{
    GateBypass, OrderedCandidate, PolicyWarning, RecommendedOutcome, VersionPolicy,
    classify_semver_release, evaluate_candidates,
};
#[allow(clippy::wildcard_imports)]
use crate::managers::*;
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::parse_rfc3339_unix;

const GEM_MAX_PARALLEL_CHECKS: usize = 4;

pub struct GemPlugin;

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

    fn supports_version_policy(&self, policy: VersionPolicy) -> bool {
        matches!(policy, VersionPolicy::Disabled | VersionPolicy::Stable)
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub static PLUGIN: GemPlugin = GemPlugin;

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

type GemPlanItem = ResolvedPlanItem<AgeResolvedTarget>;

#[derive(Debug, serde::Deserialize)]
struct RubyGemsVersionItem {
    number: String,
    created_at: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    ruby_version: Option<String>,
}

fn run(ctx: &ManagerCtx) -> Result<()> {
    run_manager_pipeline(ctx, scan, run_plan_apply)
}

#[allow(clippy::too_many_lines)]
fn run_plan_apply(ctx: &ManagerCtx) -> Result<()> {
    run_plan_apply_framework(
        ctx,
        PLUGIN.id(),
        PlanApplyFrameworkPolicy::SOFT_FETCH_STRICT_RESOLVE,
        || {
            let installed = gem_installed_inventory().context("failed to read installed gems")?;
            let outdated = gem_outdated_map().context("failed to query outdated gems")?;

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

            Ok(discovered)
        },
        Vec::is_empty,
        |discovered, runtime| {
            let Some(ruby_runtime) = soft_fail(
                ruby_runtime_version(),
                PLUGIN.id(),
                "failed to detect Ruby runtime version",
            ) else {
                return Ok(Vec::new());
            };

            let Some(rubygems_client) = soft_fail(
                crate::util::http::default_blocking_client(),
                PLUGIN.id(),
                "failed to initialize metadata HTTP client",
            ) else {
                return Ok(Vec::new());
            };

            let managed_jobs: Vec<(String, String)> = discovered
                .iter()
                .map(|item| match item {
                    GemDiscoveredItem::Managed { name, current } => (name.clone(), current.clone()),
                })
                .collect();

            let threads =
                effective_parallelism(runtime.max_parallel_checks, GEM_MAX_PARALLEL_CHECKS);
            run_indexed_parallel(managed_jobs, threads, PLUGIN.id(), |(name, current)| {
                let resolved = rubygems_resolve_target_with_min_age(
                    &rubygems_client,
                    &name,
                    &current,
                    &ruby_runtime,
                    runtime.now_unix_secs,
                    runtime.min_age,
                    ctx.policy.version_policy,
                )
                .map_err(|err| err.to_string());

                GemPlanItem::new(name, current, resolved)
            })
            .context("planning execution failed")
        },
        |_discovered, plan, runtime| {
            Ok(collect_upgradable_from_resolved_plan(
                PLUGIN.id(),
                plan,
                runtime.min_age,
                runtime.suppress_update_outcomes,
                runtime.pinned,
            ))
        },
        |ctx, _discovered, upgradable| {
            run_per_item_apply_flow(ctx, PLUGIN.id(), upgradable, apply_gem_updates)
        },
    )
}

fn apply_gem_updates(upgradable: Vec<crate::managers::PlannedUpdate>) {
    for item in upgradable {
        let name = item.name;
        let current = item.current;
        let target = item.target;
        if let Err(err) = run_cmd("gem", ["install", &name, "-v", &target], CmdStatus::Success)
            .mutating()
            .output()
        {
            emit_apply_error(PLUGIN.id(), name, current, target, err);
        }
    }
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let Some(installed) = soft_fail(
        gem_installed_inventory(),
        PLUGIN.id(),
        "failed to read installed gems",
    ) else {
        return Ok(());
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

        let Some(name) = crate::util::text::trim_non_empty(name) else {
            continue;
        };

        if current.is_empty() {
            continue;
        }

        out.insert(name.to_string(), OutdatedGem { current });
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
    version_policy: VersionPolicy,
) -> Result<AgeResolvedTarget> {
    let current_ver = parse_version_for_compare(current).with_context(|| {
        format!("failed to parse current gem version for {gem_name}: {current}")
    })?;
    let installed_class = classify_semver_release(current);

    let versions = rubygems_versions(rubygems_client, gem_name)?;
    let mut candidates: Vec<OrderedCandidate<Version>> = Vec::new();

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

        candidates.push(OrderedCandidate {
            version: item.number.clone(),
            parsed: version,
            release_class: classify_semver_release(&item.number),
            published_unix: released_at_unix,
        });
    }

    let resolution = evaluate_candidates(
        &current_ver,
        &candidates,
        installed_class,
        version_policy,
        now_unix_secs,
        min_age,
        GateBypass::NONE,
    );

    let (selected_version, current_blocked_by_policy) = match resolution.recommendation {
        RecommendedOutcome::Update { target_version } => (Some(target_version), false),
        RecommendedOutcome::DelayedByAge => (None, false),
        RecommendedOutcome::CurrentNoNewer => (Some(current.to_string()), false),
        RecommendedOutcome::CurrentBlockedByPolicy => (Some(current.to_string()), true),
    };
    let latest_blocked_by_policy_version = resolution
        .evaluations
        .iter()
        .find(|eval| !eval.policy_allowed)
        .map(|eval| eval.version.clone());
    let version_policy_warning = resolution
        .evaluations
        .iter()
        .find_map(|eval| eval.policy_warning)
        .map(PolicyWarning::as_note)
        .map(str::to_string);

    Ok(AgeResolvedTarget {
        selected_version,
        latest_version: resolution.latest_overall_version,
        latest_age_secs: resolution.latest_overall_age_secs,
        current_blocked_by_policy,
        version_policy: (version_policy != VersionPolicy::Disabled)
            .then(|| version_policy.as_str().to_string()),
        latest_blocked_by_policy_version,
        version_policy_warning,
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
    let base_url = rubygems_base_url();
    let url = format!("{base_url}/api/v1/versions/{gem_name}.json");

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

fn rubygems_base_url() -> String {
    crate::util::http::env_base_url("UPNOW_GEM_RUBYGEMS_BASE_URL", "https://rubygems.org")
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
    let trimmed = raw.trim();
    let normalized = crate::util::text::strip_v_prefix(trimmed);
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
    } else {
        nums[1] = nums[1].saturating_add(1);
    }
    nums[2] = 0;

    Version::parse(&format!("{}.{}.{}", nums[0], nums[1], nums[2])).ok()
}

fn parse_version_for_compare(raw: &str) -> Option<Version> {
    let trimmed = crate::util::text::strip_v_prefix(raw);

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
        let target = AgeResolvedTarget::new(
            Some("13.3.1".to_string()),
            Some("13.3.1".to_string()),
            Some(10 * 24 * 60 * 60),
        );

        assert!(
            target
                .delayed_latest(Duration::from_secs(7 * 24 * 60 * 60))
                .is_none()
        );
    }

    #[test]
    fn delayed_latest_present_when_latest_too_fresh() {
        let target = AgeResolvedTarget::new(
            Some("13.2.1".to_string()),
            Some("13.3.1".to_string()),
            Some(2 * 24 * 60 * 60),
        );

        assert!(
            target
                .delayed_latest(Duration::from_secs(7 * 24 * 60 * 60))
                .is_some()
        );
    }
}
