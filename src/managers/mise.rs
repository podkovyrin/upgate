use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use pep440_rs::Version as Pep440Version;
use reqwest::StatusCode;
use reqwest::blocking::Client;
use semver::Version;
use serde::Deserialize;

use crate::config::is_pinned;
use crate::managers::shared::plan::DelayedLatest;
use crate::managers::shared::versioning::policy::{GateBypass, VersionPolicy};
#[allow(clippy::wildcard_imports)]
use crate::managers::*;
use crate::outcome::{ItemOutcome, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::text::strip_v_prefix;
use crate::util::time::{human_age, now_unix_secs, parse_rfc3339_unix};

const MISE_AGE_MAX_PARALLEL_CHECKS: usize = 4;
const MISE_VERSIONS_HOST_BASE_URL: &str = "https://mise-versions.jdx.dev";
const MISE_VERSIONS_HOST_BASE_URL_ENV: &str = "UPNOW_MISE_VERSIONS_BASE_URL";

pub struct MisePlugin;

impl ManagerPlugin for MisePlugin {
    fn id(&self) -> &'static str {
        "mise"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn supports_version_policy(&self, policy: VersionPolicy) -> bool {
        policy == VersionPolicy::Disabled
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub static PLUGIN: MisePlugin = MisePlugin;

#[derive(Clone)]
struct MisePlanCheck {
    target_age: Result<MiseTargetAge, String>,
    delayed_latest: Option<MiseDelayedLatestCheck>,
}

#[derive(Clone)]
enum MiseTargetAge {
    Known(u64),
    MissingMetadata(String),
}

#[derive(Clone)]
struct MiseDelayedLatestCheck {
    version: String,
    age_secs: Result<Option<u64>, String>,
}

enum MisePlanDecision {
    Update {
        delayed_latest: Option<DelayedLatest>,
    },
    DelayedTooFresh {
        age_secs: u64,
    },
    MissingMetadata {
        reason: String,
    },
    Error(String),
}

type NpmTimeMap = BTreeMap<String, String>;

#[derive(Debug, Deserialize)]
struct MiseLsByToolEntry {
    version: Option<String>,
}

type MiseLsJson = BTreeMap<String, Vec<MiseLsByToolEntry>>;

#[derive(Debug, Deserialize)]
struct MiseLsRemoteVersion {
    version: String,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MiseRegistryTool {
    #[serde(default)]
    backends: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MiseRegistryEntry {
    short: String,
    #[serde(default)]
    backends: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MiseVersionsHostRoot {
    #[serde(default)]
    versions: BTreeMap<String, MiseVersionsHostVersion>,
}

#[derive(Debug, Deserialize)]
struct MiseVersionsHostVersion {
    created_at: MiseVersionsHostCreatedAt,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MiseVersionsHostCreatedAt {
    Datetime(toml::value::Datetime),
    String(String),
}

#[derive(Debug)]
struct MiseLsRemoteProbe {
    versions: Vec<String>,
    releases: Vec<SemverTimestamp>,
}

#[derive(Debug)]
enum MiseReleaseTimeline {
    Semver(Vec<SemverTimestamp>),
    Pep440(Vec<Pep440Timestamp>),
}

#[derive(Debug, Clone)]
struct MiseVersionTimestamp {
    version: String,
    published_unix: u64,
}

fn run(ctx: &ManagerCtx) -> Result<()> {
    run_manager_pipeline(ctx, scan, run_plan_apply)
}

fn run_plan_apply(ctx: &ManagerCtx) -> Result<()> {
    let min_age_raw = ctx.policy.min_release_age.cli_arg().to_string();

    run_plan_apply_framework(
        ctx,
        PLUGIN.id(),
        PlanApplyFrameworkPolicy::SOFT_FETCH_STRICT_RESOLVE,
        || collect_mise_plan_inputs(&min_age_raw),
        |(plan_pairs, _latest_map)| plan_pairs.is_empty(),
        |(plan_pairs, latest_map), runtime| {
            let check_by_index = resolve_mise_plan_checks(
                plan_pairs,
                latest_map,
                runtime.now_unix_secs,
                runtime.max_parallel_checks,
            )?;

            Ok(MiseResolved {
                plan_pairs: plan_pairs.clone(),
                check_by_index,
            })
        },
        |_discovered, resolved, runtime| {
            Ok(collect_mise_plan_and_upgradable(
                resolved,
                runtime.min_age,
                runtime.suppress_update_outcomes,
                runtime.pinned,
            ))
        },
        |ctx, _discovered, collected| {
            if collected.global_apply_allowed {
                return run_selective_or_global_apply_flow(
                    ctx,
                    PLUGIN.id(),
                    collected.upgradable,
                    |selected| apply_mise_selected_updates(&min_age_raw, selected),
                    || apply_mise_updates(&min_age_raw),
                );
            }

            run_per_item_apply_flow(ctx, PLUGIN.id(), collected.upgradable, |selected| {
                apply_mise_selected_updates(&min_age_raw, selected);
            })
        },
    )
}

fn apply_mise_updates(min_age_raw: &str) -> Result<()> {
    run_cmd(
        "mise",
        ["upgrade", "--before", min_age_raw],
        CmdStatus::Success,
    )
    .mutating()
    .output()?;

    Ok(())
}

fn apply_mise_selected_updates(min_age_raw: &str, upgradable: Vec<crate::managers::PlannedUpdate>) {
    for item in upgradable {
        let name = item.name;
        let current = item.current;
        let target = item.target;
        let args = [
            "upgrade".to_string(),
            "--before".to_string(),
            min_age_raw.to_string(),
            name.clone(),
        ];

        if let Err(err) = run_cmd("mise", &args, CmdStatus::Success)
            .mutating()
            .output()
        {
            emit_apply_error(PLUGIN.id(), name, current, target, err);
        }
    }
}

fn collect_mise_plan_inputs(before: &str) -> Result<(Vec<MisePlanItem>, BTreeMap<String, String>)> {
    let plan_pairs = mise_upgrade_dry_run_with_before(before)
        .with_context(|| "failed to build mise upgrade plan")?;

    let latest_map = soft_fail_or(
        mise_outdated_latest_map(),
        BTreeMap::new,
        PLUGIN.id(),
        "failed to fetch latest version map",
    );

    Ok((plan_pairs, latest_map))
}

fn resolve_mise_plan_checks(
    plan_pairs: &[MisePlanItem],
    latest_map: &BTreeMap<String, String>,
    now_unix_secs: u64,
    max_parallel_checks: usize,
) -> Result<BTreeMap<usize, MisePlanCheck>> {
    // Mise owns target selection, but not every backend has reliable native
    // publish-date filtering. We trust the dry-run target, then independently
    // require date metadata for that target so `min_release_age` remains real.
    // Delayed-latest metadata is only explanatory and must not block the plan.
    let mut age_jobs: Vec<(usize, String, String, String, Option<String>)> = Vec::new();
    for (idx, item) in plan_pairs.iter().enumerate() {
        let delayed_latest = latest_map
            .get(&item.tool)
            .filter(|latest| *latest != &item.to_version)
            .cloned();
        age_jobs.push((
            idx,
            item.tool.clone(),
            item.from_version.clone(),
            item.to_version.clone(),
            delayed_latest,
        ));
    }

    let threads = effective_parallelism(max_parallel_checks, MISE_AGE_MAX_PARALLEL_CHECKS);
    let age_results_indexed: Vec<(usize, MisePlanCheck)> = run_indexed_parallel(
        age_jobs,
        threads,
        PLUGIN.id(),
        |(idx, tool, current, target, delayed_latest)| {
            let target_age = mise_version_age_secs(&tool, &current, &target, now_unix_secs)
                .map(|age_secs| {
                    age_secs.map_or_else(
                        || MiseTargetAge::MissingMetadata(missing_mise_publish_dates_reason(&tool)),
                        MiseTargetAge::Known,
                    )
                })
                .map_err(|err| err.to_string());

            let delayed_latest = delayed_latest.map(|version| MiseDelayedLatestCheck {
                age_secs: mise_version_age_secs(&tool, &current, &version, now_unix_secs)
                    .map_err(|err| err.to_string()),
                version,
            });

            (
                idx,
                MisePlanCheck {
                    target_age,
                    delayed_latest,
                },
            )
        },
    )?;

    let mut check_by_index: BTreeMap<usize, MisePlanCheck> = BTreeMap::new();
    for (idx, age_result) in age_results_indexed {
        check_by_index.insert(idx, age_result);
    }

    Ok(check_by_index)
}

fn mise_plan_decision(
    idx: usize,
    _item: &MisePlanItem,
    check_by_index: &mut BTreeMap<usize, MisePlanCheck>,
    min_age: Duration,
) -> MisePlanDecision {
    let Some(check) = check_by_index.remove(&idx) else {
        return MisePlanDecision::Error("internal error: missing mise age check".to_string());
    };

    match check.target_age {
        Ok(MiseTargetAge::Known(age_secs)) if age_secs < min_age.as_secs() => {
            return MisePlanDecision::DelayedTooFresh { age_secs };
        }
        Ok(MiseTargetAge::Known(_)) => {}
        Ok(MiseTargetAge::MissingMetadata(reason)) => {
            return MisePlanDecision::MissingMetadata { reason };
        }
        Err(err) => return MisePlanDecision::Error(err),
    }

    let delayed_latest = match check.delayed_latest {
        Some(MiseDelayedLatestCheck {
            version,
            age_secs: Ok(Some(age_secs)),
        }) => DelayedLatest::new_if_fresh(version, age_secs, min_age),
        Some(MiseDelayedLatestCheck {
            age_secs: Ok(None) | Err(_),
            ..
        })
        | None => None,
    };

    MisePlanDecision::Update { delayed_latest }
}

fn collect_mise_plan_and_upgradable(
    mut resolved: MiseResolved,
    min_age: Duration,
    suppress_update_outcomes: bool,
    pinned: &BTreeSet<String>,
) -> MiseCollectedPlan {
    let mut upgradable = Vec::new();
    let mut global_apply_allowed = true;
    for (idx, item) in resolved.plan_pairs.into_iter().enumerate() {
        let decision = mise_plan_decision(idx, &item, &mut resolved.check_by_index, min_age);
        if !matches!(&decision, MisePlanDecision::Update { .. }) {
            global_apply_allowed = false;
        }

        if is_pinned(&item.tool, pinned) {
            handle_pinned_mise_decision(&mut upgradable, item, decision, suppress_update_outcomes);
            continue;
        }

        handle_regular_mise_decision(
            &mut upgradable,
            item,
            decision,
            min_age,
            suppress_update_outcomes,
        );
    }

    MiseCollectedPlan {
        upgradable,
        global_apply_allowed,
    }
}

fn handle_pinned_mise_decision(
    upgradable: &mut Vec<PlannedUpdate>,
    item: MisePlanItem,
    decision: MisePlanDecision,
    suppress_update_outcomes: bool,
) {
    if suppress_update_outcomes {
        if let MisePlanDecision::Update { delayed_latest } = decision {
            upgradable.push(PlannedUpdate {
                manager: PLUGIN.id(),
                name: item.tool,
                current: item.from_version,
                target: item.to_version,
                delayed_latest,
                version_policy: None,
                apply_spec_base: None,
                gate_bypass: GateBypass::NONE,
            });
        }
        return;
    }

    let outcome =
        ItemOutcome::skipped_pinned(PLUGIN.id(), item.tool, item.from_version, item.to_version);
    emit_text_outcome(&outcome);
}

fn handle_regular_mise_decision(
    upgradable: &mut Vec<PlannedUpdate>,
    item: MisePlanItem,
    decision: MisePlanDecision,
    min_age: Duration,
    suppress_update_outcomes: bool,
) {
    match decision {
        MisePlanDecision::Update { delayed_latest } => {
            let planned = PlannedUpdate {
                manager: PLUGIN.id(),
                name: item.tool,
                current: item.from_version,
                target: item.to_version,
                delayed_latest,
                version_policy: None,
                apply_spec_base: None,
                gate_bypass: GateBypass::NONE,
            };

            if !suppress_update_outcomes {
                emit_text_outcome(&planned.to_update_outcome());
            }
            upgradable.push(planned);
        }
        MisePlanDecision::DelayedTooFresh { age_secs } => {
            let outcome = ItemOutcome::delayed_too_fresh(
                PLUGIN.id(),
                item.tool,
                item.from_version,
                item.to_version,
                human_age(age_secs),
                human_age(min_age.as_secs()),
            );
            emit_text_outcome(&outcome);
        }
        MisePlanDecision::MissingMetadata { reason } => {
            let outcome = ItemOutcome::skipped_missing_metadata(
                PLUGIN.id(),
                item.tool,
                item.from_version,
                item.to_version,
                reason,
            );
            emit_text_outcome(&outcome);
        }
        MisePlanDecision::Error(err) => {
            let outcome = ItemOutcome::resolver_error(
                PLUGIN.id(),
                item.tool,
                item.from_version.clone(),
                item.from_version,
                err,
            );
            emit_text_outcome(&outcome);
        }
    }
}

fn mise_version_age_secs(
    tool: &str,
    current: &str,
    version: &str,
    now_unix_secs: u64,
) -> Result<Option<u64>> {
    if tool.starts_with("npm:") {
        return npm_version_age_secs(tool, version, now_unix_secs).map(Some);
    }

    let Some(timeline) = mise_release_timeline(tool, current, version, now_unix_secs)? else {
        return Ok(None);
    };

    Ok(mise_release_timeline_age_secs(
        &timeline,
        version,
        now_unix_secs,
    ))
}

fn mise_release_timeline_age_secs(
    timeline: &MiseReleaseTimeline,
    version: &str,
    now_unix_secs: u64,
) -> Option<u64> {
    match timeline {
        MiseReleaseTimeline::Semver(releases) => {
            release_age_secs_for_version(releases, version, now_unix_secs).or_else(|| {
                releases
                    .iter()
                    .find(|release| semver_versions_equivalent(version, &release.version))
                    .map(|release| now_unix_secs.saturating_sub(release.published_unix))
            })
        }
        MiseReleaseTimeline::Pep440(releases) => {
            release_age_secs_for_pep440_version(releases, version, now_unix_secs).or_else(|| {
                releases
                    .iter()
                    .find(|release| pep440_versions_equivalent(version, &release.version))
                    .map(|release| now_unix_secs.saturating_sub(release.published_unix))
            })
        }
    }
}

fn semver_versions_equivalent(left: &str, right: &str) -> bool {
    let left = strip_v_prefix(left.trim());
    let right = strip_v_prefix(right.trim());

    if left == right {
        return true;
    }

    match (Version::parse(left), Version::parse(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn pep440_versions_equivalent(left: &str, right: &str) -> bool {
    let left = strip_v_prefix(left.trim());
    let right = strip_v_prefix(right.trim());

    if left == right {
        return true;
    }

    match (
        Pep440Version::from_str(left),
        Pep440Version::from_str(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[derive(Clone, Debug)]
struct MisePlanItem {
    tool: String,
    from_version: String,
    to_version: String,
}

struct MiseResolved {
    plan_pairs: Vec<MisePlanItem>,
    check_by_index: BTreeMap<usize, MisePlanCheck>,
}

struct MiseCollectedPlan {
    upgradable: Vec<PlannedUpdate>,
    global_apply_allowed: bool,
}

fn parse_mise_upgrade_dry_run(text: &str) -> Result<Vec<MisePlanItem>> {
    let mut old_versions: BTreeMap<String, String> = BTreeMap::new();
    let mut result = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("Would uninstall ") {
            let (tool, from_ver) = split_tool_and_version(rest)
                .with_context(|| format!("invalid mise dry-run uninstall line: {trimmed}"))?;
            old_versions.insert(tool.to_string(), from_ver.to_string());
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("Would install ") {
            let (tool, to_ver) = split_tool_and_version(rest)
                .with_context(|| format!("invalid mise dry-run install line: {trimmed}"))?;
            let from = old_versions.remove(tool).with_context(|| {
                format!("mise dry-run install for {tool} was not preceded by matching uninstall")
            })?;
            result.push(MisePlanItem {
                tool: tool.to_string(),
                from_version: from,
                to_version: to_ver.to_string(),
            });
        } else {
            // Ignore unrelated human-facing output from mise, but keep strict
            // validation for recognized action lines and their pair structure.
        }
    }

    if let Some((tool, _from_ver)) = old_versions.into_iter().next() {
        bail!("mise dry-run uninstall for {tool} was not followed by matching install");
    }

    Ok(result)
}

fn split_tool_and_version(input: &str) -> Option<(&str, &str)> {
    let idx = input.rfind('@')?;
    let (tool, ver) = input.split_at(idx);
    Some((tool, ver.strip_prefix('@')?))
}

fn mise_upgrade_dry_run_with_before(before: &str) -> Result<Vec<MisePlanItem>> {
    let output = run_cmd(
        "mise",
        ["upgrade", "--dry-run", "--before", before],
        CmdStatus::Success,
    )
    .output()?;
    let text = output.stdout()?;
    parse_mise_upgrade_dry_run(text)
}

#[derive(Debug, serde::Deserialize)]
struct MiseOutdatedItem {
    latest: String,
}

fn mise_outdated_latest_map() -> Result<BTreeMap<String, String>> {
    let parsed: BTreeMap<String, MiseOutdatedItem> =
        run_cmd("mise", ["outdated", "--json"], CmdStatus::Success)
            .output()?
            .json()?;

    Ok(parsed.into_iter().map(|(k, v)| (k, v.latest)).collect())
}

fn mise_release_timeline(
    tool: &str,
    current: &str,
    target: &str,
    now_unix_secs: u64,
) -> Result<Option<MiseReleaseTimeline>> {
    if tool.contains(':') {
        let probe = mise_ls_remote_probe(tool)?;
        if !probe.releases.is_empty() {
            let timeline = MiseReleaseTimeline::Semver(probe.releases);
            if mise_release_timeline_age_secs(&timeline, target, now_unix_secs).is_some() {
                return Ok(Some(timeline));
            }
        }

        return mise_versions_host_release_timeline(tool, current);
    }

    for candidate in mise_registry_backends(tool)? {
        let probe = mise_ls_remote_probe(&candidate)?;
        if probe.releases.is_empty() {
            continue;
        }

        if probe
            .versions
            .iter()
            .any(|version| mise_version_matches_installed_family(current, version))
        {
            return Ok(Some(MiseReleaseTimeline::Semver(probe.releases)));
        }
    }

    mise_versions_host_release_timeline(tool, current)
}

fn mise_registry_backends(tool: &str) -> Result<Vec<String>> {
    let parsed: MiseRegistryTool =
        run_cmd("mise", ["registry", tool, "--json"], CmdStatus::Success)
            .output()?
            .json()?;

    if parsed.backends.is_empty() {
        bail!("mise registry returned no backends for {tool}");
    }

    Ok(parsed.backends)
}

fn mise_ls_remote_probe(tool: &str) -> Result<MiseLsRemoteProbe> {
    let raw: serde_json::Value = run_cmd("mise", ["ls-remote", "--json", tool], CmdStatus::Success)
        .output()?
        .json()?;

    parse_mise_ls_remote_probe(tool, raw)
}

fn parse_mise_ls_remote_probe(tool: &str, raw: serde_json::Value) -> Result<MiseLsRemoteProbe> {
    if let Ok(entries) = serde_json::from_value::<Vec<MiseLsRemoteVersion>>(raw.clone()) {
        let mut versions = Vec::with_capacity(entries.len());
        let mut releases = Vec::new();

        for entry in entries {
            versions.push(entry.version.clone());

            let Some(created_at) = entry.created_at else {
                continue;
            };

            if let Ok(published_unix) = parse_mise_timestamp_unix(&created_at) {
                releases.push(SemverTimestamp {
                    version: entry.version,
                    published_unix,
                });
            }
        }

        return Ok(MiseLsRemoteProbe { versions, releases });
    }

    if let Ok(versions) = serde_json::from_value::<Vec<String>>(raw) {
        return Ok(MiseLsRemoteProbe {
            versions,
            releases: Vec::new(),
        });
    }

    bail!("failed to parse mise ls-remote JSON for {tool}")
}

fn mise_versions_host_release_timeline(
    tool: &str,
    current: &str,
) -> Result<Option<MiseReleaseTimeline>> {
    let versions_host_tools = mise_versions_host_tools(tool)?;
    if versions_host_tools.is_empty() {
        return Ok(None);
    }

    let client = crate::util::http::default_blocking_client()
        .context("failed to initialize mise versions-host HTTP client")?;

    for versions_host_tool in versions_host_tools {
        let Some(releases) = fetch_mise_versions_host_releases(&client, &versions_host_tool)?
        else {
            continue;
        };

        if let Some(timeline) = mise_versions_host_timeline_from_releases(current, releases) {
            return Ok(Some(timeline));
        }
    }

    Ok(None)
}

fn mise_versions_host_tools(tool: &str) -> Result<Vec<String>> {
    if !tool.contains(':') {
        return Ok(vec![tool.to_string()]);
    }

    mise_registry_shorts_for_backend(tool)
}

fn mise_registry_shorts_for_backend(backend: &str) -> Result<Vec<String>> {
    let parsed: Vec<MiseRegistryEntry> =
        run_cmd("mise", ["registry", "--json"], CmdStatus::Success)
            .output()?
            .json()?;

    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for entry in parsed {
        if entry
            .backends
            .iter()
            .any(|candidate| mise_backend_matches(candidate, backend))
            && seen.insert(entry.short.clone())
        {
            out.push(entry.short);
        }
    }

    Ok(out)
}

fn mise_backend_matches(candidate: &str, installed: &str) -> bool {
    candidate == installed
        || strip_mise_backend_options(candidate) == strip_mise_backend_options(installed)
}

fn strip_mise_backend_options(raw: &str) -> &str {
    raw.split_once('[')
        .map_or(raw, |(backend_without_options, _opts)| {
            backend_without_options
        })
}

fn fetch_mise_versions_host_releases(
    client: &Client,
    tool: &str,
) -> Result<Option<Vec<MiseVersionTimestamp>>> {
    let base_url = mise_versions_host_base_url();
    let url = format!("{base_url}/tools/{tool}.toml");

    let response = client
        .get(&url)
        .send()
        .with_context(|| format!("failed to GET {url}"))?;

    if matches!(
        response.status(),
        StatusCode::NOT_FOUND | StatusCode::TOO_MANY_REQUESTS
    ) {
        return Ok(None);
    }

    let body = response
        .error_for_status()
        .with_context(|| format!("mise versions host returned error for {tool}"))?
        .text()
        .with_context(|| format!("failed to read mise versions-host response body for {tool}"))?;

    let releases = parse_mise_versions_host_releases(tool, &body)?;
    if releases.is_empty() {
        Ok(None)
    } else {
        Ok(Some(releases))
    }
}

fn parse_mise_versions_host_releases(tool: &str, raw: &str) -> Result<Vec<MiseVersionTimestamp>> {
    let root: MiseVersionsHostRoot = toml::from_str(raw)
        .with_context(|| format!("failed to parse mise versions-host TOML for {tool}"))?;
    let mut releases = Vec::new();

    for (version, entry) in root.versions {
        let created_at = entry.created_at.to_timestamp_string();
        if let Ok(published_unix) = parse_mise_timestamp_unix(&created_at) {
            releases.push(MiseVersionTimestamp {
                version,
                published_unix,
            });
        }
    }

    Ok(releases)
}

impl MiseVersionsHostCreatedAt {
    fn to_timestamp_string(&self) -> String {
        match self {
            Self::Datetime(value) => value.to_string(),
            Self::String(value) => value.clone(),
        }
    }
}

fn mise_versions_host_timeline_from_releases(
    current: &str,
    releases: Vec<MiseVersionTimestamp>,
) -> Option<MiseReleaseTimeline> {
    let mut semver_releases = Vec::new();
    let mut pep440_releases = Vec::new();
    let mut has_pep440_only_version = false;

    for release in releases {
        let semver_parseable = Version::parse(&release.version).is_ok();
        let pep440_parseable = Pep440Version::from_str(&release.version).is_ok();

        if semver_parseable {
            semver_releases.push(SemverTimestamp {
                version: release.version.clone(),
                published_unix: release.published_unix,
            });
        }

        if pep440_parseable {
            if !semver_parseable {
                has_pep440_only_version = true;
            }
            pep440_releases.push(Pep440Timestamp {
                version: release.version,
                published_unix: release.published_unix,
            });
        }
    }

    let semver_matches = semver_releases
        .iter()
        .any(|release| mise_version_matches_installed_family(current, &release.version));
    let pep440_matches = pep440_releases
        .iter()
        .any(|release| pep440_versions_equivalent(current, &release.version));

    if pep440_matches && (has_pep440_only_version || !semver_matches) {
        return Some(MiseReleaseTimeline::Pep440(pep440_releases));
    }

    if semver_matches {
        return Some(MiseReleaseTimeline::Semver(semver_releases));
    }

    pep440_matches.then_some(MiseReleaseTimeline::Pep440(pep440_releases))
}

fn parse_mise_timestamp_unix(raw: &str) -> Result<u64> {
    let trimmed = raw.trim();
    match parse_rfc3339_unix(trimmed) {
        Ok(ts) => Ok(ts),
        Err(err) if mise_timestamp_missing_timezone(trimmed) => {
            parse_rfc3339_unix(&format!("{trimmed}Z")).or(Err(err))
        }
        Err(err) => Err(err),
    }
}

fn mise_timestamp_missing_timezone(raw: &str) -> bool {
    let Some((_date, time)) = raw.split_once('T') else {
        return false;
    };

    !time.ends_with('Z') && !time.contains('+') && !time.contains('-')
}

fn mise_versions_host_base_url() -> String {
    crate::util::http::env_base_url(MISE_VERSIONS_HOST_BASE_URL_ENV, MISE_VERSIONS_HOST_BASE_URL)
}

fn missing_mise_publish_dates_reason(tool: &str) -> String {
    if tool.contains(':') {
        "selected mise backend does not provide publish-date metadata for the planned version"
            .to_string()
    } else {
        "no compatible mise backend provides publish-date metadata for the planned version"
            .to_string()
    }
}

fn mise_version_matches_installed_family(installed: &str, candidate: &str) -> bool {
    let installed = strip_v_prefix(installed.trim());
    let candidate = strip_v_prefix(candidate.trim());

    if installed == candidate {
        return true;
    }

    let Ok(installed) = Version::parse(installed) else {
        return false;
    };
    let Ok(candidate) = Version::parse(candidate) else {
        return false;
    };

    installed.major == candidate.major
        && installed.minor == candidate.minor
        && installed.patch == candidate.patch
}

fn npm_version_age_secs(tool: &str, version: &str, now_unix_secs: u64) -> Result<u64> {
    let pkg = tool.trim_start_matches("npm:");
    let spec = format!("{pkg}@{version}");
    let timestamps_by_version: NpmTimeMap =
        run_cmd("npm", ["view", &spec, "time", "--json"], CmdStatus::Success)
            .output()?
            .json()?;

    let ts_raw = timestamps_by_version
        .get(version)
        .with_context(|| format!("npm view time missing timestamp for {spec}"))?;

    let ts = parse_rfc3339_unix(ts_raw)
        .with_context(|| format!("invalid RFC3339 timestamp for {spec}: {ts_raw}"))?;

    Ok(now_unix_secs.saturating_sub(ts))
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let Some(installed) = soft_fail(
        mise_installed_versions(),
        PLUGIN.id(),
        "failed to read installed mise tools",
    ) else {
        return Ok(());
    };

    if installed.is_empty() {
        return Ok(());
    }

    let now = if crate::ui::output_theme().verbose {
        Some(now_unix_secs()?)
    } else {
        None
    };

    emit_mise_scan_outcomes(installed, now, ctx.scan_old_age_threshold);
    Ok(())
}

fn emit_mise_scan_outcomes(
    installed: BTreeMap<String, String>,
    now_unix_secs: Option<u64>,
    old_threshold: Duration,
) {
    for (tool, version) in installed {
        let age_secs = now_unix_secs.and_then(|now_unix_secs| {
            if tool.starts_with("npm:") {
                npm_version_age_secs(&tool, &version, now_unix_secs).ok()
            } else {
                None
            }
        });

        emit_scan_current(PLUGIN.id(), tool, version, age_secs, old_threshold);
    }
}

fn mise_installed_versions() -> Result<BTreeMap<String, String>> {
    let parsed: MiseLsJson = run_cmd("mise", ["ls", "--json"], CmdStatus::Success)
        .output()?
        .json()?;

    let mut out = BTreeMap::new();
    for (tool, entries) in parsed {
        for entry in entries {
            let Some(version) = entry.version else {
                continue;
            };

            out.insert(tool.clone(), version);
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_update_when_planned_target_is_old_enough() {
        let mut checks = BTreeMap::from([(
            0,
            MisePlanCheck {
                target_age: Ok(MiseTargetAge::Known(86_400 * 10)),
                delayed_latest: None,
            },
        )]);
        let item = MisePlanItem {
            tool: "node".to_string(),
            from_version: "20.0.0".to_string(),
            to_version: "20.1.0".to_string(),
        };

        let decision = mise_plan_decision(0, &item, &mut checks, Duration::from_secs(86_400 * 7));

        match decision {
            MisePlanDecision::Update { delayed_latest } => assert!(delayed_latest.is_none()),
            _ => panic!("expected update decision"),
        }
    }

    #[test]
    fn delays_update_when_planned_target_is_too_fresh() {
        let mut checks = BTreeMap::from([(
            0,
            MisePlanCheck {
                target_age: Ok(MiseTargetAge::Known(3_600)),
                delayed_latest: None,
            },
        )]);
        let item = MisePlanItem {
            tool: "node".to_string(),
            from_version: "20.0.0".to_string(),
            to_version: "20.1.0".to_string(),
        };

        let decision = mise_plan_decision(0, &item, &mut checks, Duration::from_secs(86_400 * 7));

        match decision {
            MisePlanDecision::DelayedTooFresh { age_secs } => assert_eq!(age_secs, 3_600),
            _ => panic!("expected delayed decision"),
        }
    }

    #[test]
    fn reports_missing_metadata_when_planned_target_has_no_publish_date() {
        let mut checks = BTreeMap::from([(
            0,
            MisePlanCheck {
                target_age: Ok(MiseTargetAge::MissingMetadata("no dates".to_string())),
                delayed_latest: None,
            },
        )]);
        let item = MisePlanItem {
            tool: "node".to_string(),
            from_version: "20.0.0".to_string(),
            to_version: "20.1.0".to_string(),
        };

        let decision = mise_plan_decision(0, &item, &mut checks, Duration::from_secs(86_400 * 7));

        match decision {
            MisePlanDecision::MissingMetadata { reason } => assert_eq!(reason, "no dates"),
            _ => panic!("expected missing metadata decision"),
        }
    }

    #[test]
    fn delayed_latest_annotation_is_dropped_on_age_lookup_failure() {
        let mut checks = BTreeMap::from([(
            0,
            MisePlanCheck {
                target_age: Ok(MiseTargetAge::Known(86_400 * 10)),
                delayed_latest: Some(MiseDelayedLatestCheck {
                    version: "9.0.0".to_string(),
                    age_secs: Err("lookup failed".to_string()),
                }),
            },
        )]);
        let item = MisePlanItem {
            tool: "npm:eslint".to_string(),
            from_version: "1.0.0".to_string(),
            to_version: "8.0.0".to_string(),
        };

        let decision = mise_plan_decision(0, &item, &mut checks, Duration::from_secs(86_400 * 7));

        match decision {
            MisePlanDecision::Update { delayed_latest } => assert!(delayed_latest.is_none()),
            _ => panic!("expected update decision"),
        }
    }

    #[test]
    fn delayed_latest_annotation_is_dropped_when_latest_is_old_enough() {
        let mut checks = BTreeMap::from([(
            0,
            MisePlanCheck {
                target_age: Ok(MiseTargetAge::Known(86_400 * 10)),
                delayed_latest: Some(MiseDelayedLatestCheck {
                    version: "9.0.0".to_string(),
                    age_secs: Ok(Some(86_400 * 7)),
                }),
            },
        )]);
        let item = MisePlanItem {
            tool: "npm:eslint".to_string(),
            from_version: "1.0.0".to_string(),
            to_version: "8.0.0".to_string(),
        };

        let decision = mise_plan_decision(0, &item, &mut checks, Duration::from_secs(86_400 * 7));

        match decision {
            MisePlanDecision::Update { delayed_latest } => assert!(delayed_latest.is_none()),
            _ => panic!("expected update decision"),
        }
    }

    #[test]
    fn parses_upgrade_dry_run_pairs_strictly() {
        let parsed = parse_mise_upgrade_dry_run(
            r"
Would uninstall npm:alpha-ready@1.0.0
Would install npm:alpha-ready@1.2.0
Would uninstall npm:@scope/pkg@2.0.0
Would install npm:@scope/pkg@2.1.0
",
        )
        .expect("should parse");

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].tool, "npm:alpha-ready");
        assert_eq!(parsed[0].from_version, "1.0.0");
        assert_eq!(parsed[0].to_version, "1.2.0");
        assert_eq!(parsed[1].tool, "npm:@scope/pkg");
        assert_eq!(parsed[1].from_version, "2.0.0");
        assert_eq!(parsed[1].to_version, "2.1.0");
    }

    #[test]
    fn upgrade_dry_run_parse_ignores_unrelated_output() {
        let parsed = parse_mise_upgrade_dry_run(
            r"
mise WARN something changed
Would uninstall node@20.0.0
Would install node@20.1.0
Done
",
        )
        .expect("should parse recognized dry-run actions");

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].tool, "node");
        assert_eq!(parsed[0].from_version, "20.0.0");
        assert_eq!(parsed[0].to_version, "20.1.0");
    }

    #[test]
    fn upgrade_dry_run_parse_rejects_install_without_uninstall() {
        let err = parse_mise_upgrade_dry_run("Would install node@20.1.0\n")
            .expect_err("should reject missing uninstall");

        assert!(
            err.to_string()
                .contains("was not preceded by matching uninstall")
        );
    }

    #[test]
    fn upgrade_dry_run_parse_rejects_uninstall_without_install() {
        let err = parse_mise_upgrade_dry_run("Would uninstall node@20.0.0\n")
            .expect_err("should reject missing install");

        assert!(
            err.to_string()
                .contains("was not followed by matching install")
        );
    }

    #[test]
    fn parses_ls_remote_entries_with_created_at() {
        let raw: serde_json::Value = serde_json::from_str(
            r#"
[
  {"version":"1.0.0","created_at":"2020-01-01T00:00:00Z"},
  {"version":"1.1.0","created_at":"2021-01-01T00:00:00Z"}
]
"#,
        )
        .expect("valid json");

        let parsed = parse_mise_ls_remote_probe("node", raw).expect("should parse");
        assert_eq!(parsed.releases.len(), 2);
        assert_eq!(parsed.releases[0].version, "1.0.0");
        assert_eq!(parsed.releases[1].version, "1.1.0");
    }

    #[test]
    fn parses_ls_remote_timestamp_without_timezone_as_utc() {
        let raw: serde_json::Value = serde_json::from_str(
            r#"
[
  {"version":"17.0.2","created_at":"2025-03-28T22:02:59.599901"}
]
"#,
        )
        .expect("valid json");

        let parsed = parse_mise_ls_remote_probe("java", raw).expect("should parse");
        assert_eq!(parsed.releases.len(), 1);
        assert_eq!(parsed.releases[0].version, "17.0.2");
    }

    #[test]
    fn ls_remote_string_only_json_is_supported_as_empty_release_timeline() {
        let raw: serde_json::Value =
            serde_json::from_str(r#"["1.0.0","1.1.0"]"#).expect("valid json");

        let parsed = parse_mise_ls_remote_probe("node", raw).expect("should parse");
        assert!(parsed.releases.is_empty());
        assert_eq!(parsed.versions, vec!["1.0.0", "1.1.0"]);
    }

    #[test]
    fn parses_versions_host_toml_entries_with_created_at() {
        let raw = r#"
[versions]
"5.0.4" = { created_at = 2026-03-23T20:01:59.174Z }
"5.0.5" = { created_at = 2026-04-03T19:15:36.977 }
"5.0.6" = { created_at = "2026-04-15T00:05:04.128Z" }
"#;

        let parsed = parse_mise_versions_host_releases("emsdk", raw).expect("should parse");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].version, "5.0.4");
        assert_eq!(parsed[1].version, "5.0.5");
        assert_eq!(parsed[2].version, "5.0.6");
    }

    #[test]
    fn versions_host_timeline_prefers_pep440_when_pep440_only_versions_exist() {
        let releases = vec![
            MiseVersionTimestamp {
                version: "3.14.3".to_string(),
                published_unix: 1,
            },
            MiseVersionTimestamp {
                version: "3.15.0a8".to_string(),
                published_unix: 2,
            },
        ];

        let timeline = mise_versions_host_timeline_from_releases("3.14.3", releases)
            .expect("should build timeline");
        assert!(matches!(timeline, MiseReleaseTimeline::Pep440(_)));
    }

    #[test]
    fn versions_host_uses_short_tool_name_directly() {
        assert_eq!(
            mise_versions_host_tools("swiftformat").expect("should resolve"),
            vec!["swiftformat".to_string()]
        );
    }

    #[test]
    fn backend_matching_ignores_bracketed_options() {
        assert!(mise_backend_matches(
            "github:withgraphite/homebrew-tap[exe=gt]",
            "github:withgraphite/homebrew-tap"
        ));
    }

    #[test]
    fn matching_version_family_accepts_same_semver_core() {
        assert!(mise_version_matches_installed_family(
            "v1.2.3-beta.1",
            "1.2.3+build.5"
        ));
    }

    #[test]
    fn matching_version_family_rejects_different_patch() {
        assert!(!mise_version_matches_installed_family("1.2.3", "1.2.4"));
    }

    #[test]
    fn matching_version_family_rejects_non_semver_mismatch() {
        assert!(!mise_version_matches_installed_family(
            "temurin-21",
            "21.0.1"
        ));
    }
}
