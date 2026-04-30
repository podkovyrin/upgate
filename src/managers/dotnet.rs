use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;

use crate::config::ManagerMode;
use crate::managers::shared::versioning::policy::VersionPolicy;
#[allow(clippy::wildcard_imports)]
use crate::managers::*;
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::parse_rfc3339_unix;

const DOTNET_MAX_PARALLEL_CHECKS: usize = 4;

pub struct DotnetPlugin;

impl ManagerPlugin for DotnetPlugin {
    fn id(&self) -> &'static str {
        "dotnet"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn default_mode(&self) -> ManagerMode {
        ManagerMode::Off
    }

    fn supports_version_policy(&self, _policy: VersionPolicy) -> bool {
        true
    }

    crate::impl_manager_pipeline!();
}

pub static PLUGIN: DotnetPlugin = DotnetPlugin;

#[derive(Debug, serde::Deserialize)]
struct DotnetToolListRoot {
    #[serde(default)]
    data: Vec<DotnetToolEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DotnetToolEntry {
    package_id: String,
    version: String,
}

type DotnetPlanItem = ResolvedPlanItem<VersionPolicyResolution>;

#[derive(Debug, serde::Deserialize)]
struct NugetRegistrationIndex {
    #[serde(default)]
    items: Vec<NugetRegistrationPageRef>,
}

#[derive(Debug, serde::Deserialize)]
struct NugetRegistrationPageRef {
    #[serde(rename = "@id")]
    id: String,
}

#[derive(Debug, serde::Deserialize)]
struct NugetRegistrationPage {
    #[serde(default)]
    items: Vec<NugetRegistrationLeaf>,
}

#[derive(Debug, serde::Deserialize)]
struct NugetRegistrationLeaf {
    #[serde(rename = "catalogEntry")]
    catalog_entry: NugetCatalogEntry,
}

#[derive(Debug, serde::Deserialize)]
struct NugetCatalogEntry {
    version: String,
    published: Option<String>,
    #[serde(default)]
    listed: Option<bool>,
}

fn apply(ctx: &ManagerCtx) -> Result<()> {
    run_planned_apply(ctx, plan_apply(ctx)?, apply_planned_updates)
}

fn plan_apply(ctx: &ManagerCtx) -> Result<Option<PlannedApply<()>>> {
    run_plan_apply_framework(
        ctx,
        PLUGIN.id(),
        PlanApplyFrameworkPolicy::SOFT_FETCH_SOFT_RESOLVE,
        || dotnet_global_tools().context("failed to read installed .NET tools"),
        Vec::is_empty,
        |installed, runtime| {
            resolve_dotnet_plan(
                installed.clone(),
                runtime.now_unix_secs,
                runtime.min_age,
                runtime.max_parallel_checks,
                ctx.policy.version_policy,
            )
            .context("planning execution failed")
        },
        |_installed, plan, runtime| {
            let candidates = collect_apply_candidates_from_resolved_plan(
                PLUGIN.id(),
                plan,
                runtime.min_age,
                runtime.suppress_update_outcomes,
                runtime.pinned,
                true,
            );
            Ok(PlannedApplyPayload::new((), candidates))
        },
    )
}

fn interactive_apply(
    ctx: &ManagerCtx,
) -> Result<Option<crate::interactive::apply::InteractiveApplyPlan>> {
    Ok(plan_interactive_apply_from_planned(
        plan_apply(ctx)?,
        apply_planned_updates,
    ))
}

fn apply_planned_updates(
    ctx: &ManagerCtx,
    (): (),
    selection: crate::interactive::apply::ApplySelection,
) {
    apply_per_item_selection(ctx, selection, apply_dotnet_updates);
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let Some(installed) = soft_fail(
        dotnet_global_tools(),
        PLUGIN.id(),
        "failed to read installed .NET tools",
    ) else {
        return Ok(());
    };

    if installed.is_empty() {
        return Ok(());
    }

    let now = verbose_now_unix_secs()?;

    let nuget_client = if now.is_some() {
        crate::util::http::default_blocking_client().ok()
    } else {
        None
    };

    emit_dotnet_scan_outcomes(
        installed,
        nuget_client.as_ref(),
        now,
        ctx.scan_old_age_threshold,
    );
    Ok(())
}

fn resolve_dotnet_plan(
    installed: Vec<DotnetToolEntry>,
    now_unix_secs: u64,
    min_age: Duration,
    max_parallel_checks: usize,
    version_policy: VersionPolicy,
) -> Result<Vec<DotnetPlanItem>> {
    let Some(nuget_client) = soft_fail(
        crate::util::http::default_blocking_client(),
        PLUGIN.id(),
        "failed to initialize metadata HTTP client",
    ) else {
        return Ok(Vec::new());
    };

    let jobs: Vec<(String, String)> = installed
        .into_iter()
        .map(|entry| (entry.package_id, entry.version))
        .collect();

    let threads = effective_parallelism(max_parallel_checks, DOTNET_MAX_PARALLEL_CHECKS);
    run_indexed_parallel(jobs, threads, PLUGIN.id(), |(name, current)| {
        let resolved = nuget_resolve_target_with_min_age(
            &nuget_client,
            &name,
            &current,
            now_unix_secs,
            min_age,
            version_policy,
        )
        .map_err(|err| err.to_string());

        DotnetPlanItem::new(name, current, resolved)
    })
}

fn apply_dotnet_updates(upgradable: Vec<crate::managers::PlannedUpdate>) {
    for item in upgradable {
        let name = item.name;
        let current = item.current;
        let target = item.target;
        if let Err(err) = run_cmd(
            "dotnet",
            [
                "tool",
                "update",
                "--global",
                &name,
                "--version",
                &target,
                "--allow-downgrade",
            ],
            CmdStatus::Success,
        )
        .mutating()
        .output()
        {
            emit_apply_error(PLUGIN.id(), name, current, target, err);
        }
    }
}

fn emit_dotnet_scan_outcomes(
    installed: Vec<DotnetToolEntry>,
    nuget_client: Option<&Client>,
    now_unix_secs: Option<u64>,
    old_threshold: Duration,
) {
    for entry in installed {
        let age_secs = if let (Some(client), Some(now_unix_secs)) = (nuget_client, now_unix_secs) {
            nuget_release_age_secs(client, &entry.package_id, &entry.version, now_unix_secs)
                .ok()
                .flatten()
        } else {
            None
        };

        emit_scan_current(
            PLUGIN.id(),
            entry.package_id,
            entry.version,
            age_secs,
            old_threshold,
        );
    }
}

fn dotnet_global_tools() -> Result<Vec<DotnetToolEntry>> {
    let output = run_cmd(
        "dotnet",
        ["tool", "list", "--global", "--format", "json"],
        CmdStatus::IgnoreStatus,
    )
    .output()?;

    let stdout = output.stdout()?;
    let stderr = output.stderr().unwrap_or_default();

    if !output.success() {
        if dotnet_missing_sdk_hint(stdout) || dotnet_missing_sdk_hint(stderr) {
            return Ok(Vec::new());
        }

        let err_text = crate::util::text::read_non_empty(stderr, stdout);
        bail!("dotnet tool list --global --format json failed: {err_text}");
    }

    if crate::util::text::is_blank(stdout) {
        return Ok(Vec::new());
    }

    let parsed: DotnetToolListRoot =
        serde_json::from_str(stdout).context("failed to parse dotnet tool list JSON")?;
    Ok(parsed.data)
}

fn dotnet_missing_sdk_hint(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();

    let no_sdk_found =
        lower.contains(".net sdk") && lower.contains("no") && lower.contains("found");
    let cannot_find_installed = lower.contains("installed .net sdk")
        && lower.contains("not possible")
        && lower.contains("find");

    no_sdk_found || cannot_find_installed
}

fn nuget_resolve_target_with_min_age(
    nuget_client: &Client,
    package_id: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
    version_policy: VersionPolicy,
) -> Result<VersionPolicyResolution> {
    let versions = nuget_versions_with_publish_times(nuget_client, package_id)?;

    let resolved =
        resolve_semver_with_min_age(current, &versions, now_unix_secs, min_age, version_policy)
            .with_context(|| {
                format!("failed to resolve eligible semver target for {package_id}")
            })?;

    Ok(resolved)
}

fn nuget_release_age_secs(
    client: &Client,
    package_id: &str,
    version: &str,
    now_unix_secs: u64,
) -> Result<Option<u64>> {
    let versions = nuget_versions_with_publish_times(client, package_id)?;
    Ok(release_age_secs_for_version(
        &versions,
        version,
        now_unix_secs,
    ))
}

fn nuget_versions_with_publish_times(
    client: &Client,
    package_id: &str,
) -> Result<Vec<SemverTimestamp>> {
    let id_lower = package_id.to_ascii_lowercase();
    let base_url = nuget_base_url();

    let mut out = nuget_versions_with_publish_times_from_registration(
        client,
        package_id,
        &format!("{base_url}/v3/registration5-gz-semver2/{id_lower}/index.json"),
        true,
    )?;

    if out.is_empty() {
        out = nuget_versions_with_publish_times_from_registration(
            client,
            package_id,
            &format!("{base_url}/v3/registration5-semver1/{id_lower}/index.json"),
            false,
        )?;
    }

    if out.is_empty() {
        bail!("no NuGet versions with publish times found for {package_id}");
    }

    Ok(out)
}

fn nuget_base_url() -> String {
    crate::util::http::env_base_url("UPNOW_DOTNET_NUGET_BASE_URL", "https://api.nuget.org")
}

fn nuget_versions_with_publish_times_from_registration(
    client: &Client,
    package_id: &str,
    index_url: &str,
    gzipped: bool,
) -> Result<Vec<SemverTimestamp>> {
    let index_body = fetch_text(client, index_url, gzipped)
        .with_context(|| format!("failed to read NuGet registration index for {package_id}"))?;

    let index: NugetRegistrationIndex = serde_json::from_str(&index_body)
        .with_context(|| format!("failed to parse NuGet registration index for {package_id}"))?;

    let mut out = Vec::new();

    for page_ref in index.items {
        let page_body = fetch_text(client, &page_ref.id, gzipped).with_context(|| {
            format!(
                "failed to read NuGet registration page for {package_id} ({})",
                page_ref.id
            )
        })?;

        let page: NugetRegistrationPage = serde_json::from_str(&page_body)
            .with_context(|| format!("failed to parse NuGet registration page for {package_id}"))?;

        for leaf in page.items {
            let entry = leaf.catalog_entry;
            if entry.listed == Some(false) {
                continue;
            }

            let Some(published_raw) = entry.published.as_deref() else {
                continue;
            };

            let published = parse_rfc3339_unix(published_raw).with_context(|| {
                format!(
                    "invalid NuGet published timestamp for {package_id}@{}: {published_raw}",
                    entry.version
                )
            })?;

            out.push(SemverTimestamp {
                version: entry.version,
                published_unix: published,
            });
        }
    }

    Ok(out)
}

fn fetch_text(client: &Client, url: &str, gzipped: bool) -> Result<String> {
    let resp = client
        .get(url)
        .send()
        .with_context(|| format!("failed to GET {url}"))?
        .error_for_status()
        .with_context(|| format!("NuGet returned error for URL {url}"))?;

    let bytes = resp
        .bytes()
        .with_context(|| format!("failed to read NuGet response body for URL {url}"))?;

    if gzipped {
        let mut decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
        let mut out = String::new();
        std::io::Read::read_to_string(&mut decoder, &mut out)
            .with_context(|| format!("failed to gunzip NuGet payload from URL {url}"))?;
        Ok(out)
    } else {
        String::from_utf8(bytes.to_vec())
            .with_context(|| format!("NuGet payload was not UTF-8 for URL {url}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::shared::versioning::policy::{
        RecommendedOutcome, delayed_candidate_for_test,
    };

    fn resolution_for_delayed_note(
        selected: &str,
        latest_policy_eligible: &str,
        latest_age_secs: u64,
    ) -> VersionPolicyResolution {
        VersionPolicyResolution {
            configured_policy: VersionPolicy::Disabled,
            recommendation: RecommendedOutcome::Update {
                target_version: selected.to_string(),
            },
            latest_overall_version: Some(latest_policy_eligible.to_string()),
            latest_overall_age_secs: Some(latest_age_secs),
            latest_policy_eligible_version: Some(latest_policy_eligible.to_string()),
            latest_policy_eligible_age_secs: Some(latest_age_secs),
            latest_age_eligible_version: None,
            has_newer_versions: true,
            blocked_by_policy_count: 0,
            blocked_by_age_count: 1,
            evaluations: Vec::new(),
        }
    }

    #[test]
    fn parse_tool_list_json() {
        let raw = r#"{"version":1,"data":[{"packageId":"dotnet-ef","version":"10.0.5","commands":["dotnet-ef"]}]}"#;
        let parsed: DotnetToolListRoot = serde_json::from_str(raw).expect("should parse");
        assert_eq!(parsed.data.len(), 1);
        assert_eq!(parsed.data[0].package_id, "dotnet-ef");
        assert_eq!(parsed.data[0].version, "10.0.5");
    }

    #[test]
    fn manager_mode_defaults_to_off() {
        assert_eq!(PLUGIN.default_mode(), ManagerMode::Off);
    }

    #[test]
    fn delayed_latest_hidden_when_latest_not_delayed() {
        let target = resolution_for_delayed_note("10.0.5", "10.0.5", 10 * 24 * 60 * 60);

        assert!(
            delayed_candidate_for_test(&target, Duration::from_secs(7 * 24 * 60 * 60)).is_none()
        );
    }

    #[test]
    fn delayed_latest_present_when_latest_too_fresh() {
        let target = resolution_for_delayed_note("10.0.4", "10.0.5", 2 * 24 * 60 * 60);

        assert!(
            delayed_candidate_for_test(&target, Duration::from_secs(7 * 24 * 60 * 60)).is_some()
        );
    }

    #[test]
    fn detects_dotnet_sdk_missing_hints() {
        assert!(dotnet_missing_sdk_hint("No .NET SDKs were found"));
        assert!(dotnet_missing_sdk_hint(
            "It was not possible to find any installed .NET SDKs"
        ));
        assert!(!dotnet_missing_sdk_hint("some other error"));
    }
}
