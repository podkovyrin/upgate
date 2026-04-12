use crate::config::ManagerMode;
use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, PlanDecision, PlanMeta, SemverAgeResolution, SemverTimestamp,
    emit_manager_level_error, emit_plan_and_collect_upgradable, emit_scan_current,
    release_age_secs_for_version, resolve_semver_with_min_age, verbose_now_unix_secs,
};
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::now_unix_secs;
use crate::util::timefmt::human_age;
use crate::util::timeparse::parse_rfc3339_unix;
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use std::time::Duration;

const DOTNET_MAX_PARALLEL_CHECKS: usize = 4;

pub(crate) struct DotnetPlugin;

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

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub(crate) static PLUGIN: DotnetPlugin = DotnetPlugin;

#[derive(Debug, serde::Deserialize)]
struct DotnetToolListRoot {
    #[serde(default)]
    data: Vec<DotnetToolEntry>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DotnetToolEntry {
    package_id: String,
    version: String,
}

struct DotnetPlanItem {
    name: String,
    current: String,
    resolved: Result<NugetResolvedTarget, String>,
}

struct NugetResolvedTarget {
    selected_version: Option<String>,
    latest_version: Option<String>,
    latest_age_secs: Option<u64>,
}

impl NugetResolvedTarget {
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

fn run(ctx: &ManagerCtx) -> Result<()> {
    if ctx.is_scan() {
        return scan(ctx);
    }

    let min_age = ctx.policy.min_release_age.duration();

    let installed = match dotnet_global_tools() {
        Ok(installed) => installed,
        Err(err) => {
            emit_dotnet_manager_error(format!("failed to read installed .NET tools: {err}"));
            return Ok(());
        }
    };

    if installed.is_empty() {
        return Ok(());
    }

    let now = now_unix_secs()?;

    let nuget_client = match crate::util::http::default_blocking_client() {
        Ok(client) => client,
        Err(err) => {
            emit_dotnet_manager_error(format!("failed to initialize metadata HTTP client: {err}"));
            return Ok(());
        }
    };

    let plan = match resolve_dotnet_plan(
        installed,
        &nuget_client,
        now,
        min_age,
        ctx.max_parallel_checks,
    ) {
        Ok(plan) => plan,
        Err(err) => {
            emit_dotnet_manager_error(format!("planning execution failed: {err}"));
            return Ok(());
        }
    };

    let upgradable = emit_plan_and_collect_upgradable(
        plan,
        |item| PlanMeta {
            manager: PLUGIN.id(),
            source: "nuget",
            name: item.name.clone(),
            current: item.current.clone(),
        },
        |item| {
            let target = match &item.resolved {
                Ok(target) => target,
                Err(err) => return PlanDecision::Error(err.clone()),
            };

            if let Some(selected) = target.selected_version.as_deref() {
                if selected == item.current {
                    return PlanDecision::NoChange;
                }

                return PlanDecision::Update {
                    target: selected.to_string(),
                    delayed_latest: target.delayed_latest(min_age),
                };
            }

            PlanDecision::DelayedNoEligible {
                required_age: human_age(min_age.as_secs()),
                delayed_latest: target.delayed_latest(min_age),
            }
        },
    );

    if ctx.is_dry_run() {
        return Ok(());
    }

    apply_dotnet_updates(upgradable);

    Ok(())
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let installed = match dotnet_global_tools() {
        Ok(installed) => installed,
        Err(err) => {
            emit_dotnet_manager_error(format!("failed to read installed .NET tools: {err}"));
            return Ok(());
        }
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
    nuget_client: &Client,
    now_unix_secs: u64,
    min_age: Duration,
    max_parallel_checks: usize,
) -> Result<Vec<DotnetPlanItem>> {
    let jobs: Vec<(String, String)> = installed
        .into_iter()
        .map(|entry| (entry.package_id, entry.version))
        .collect();

    let threads = effective_parallelism(max_parallel_checks, DOTNET_MAX_PARALLEL_CHECKS);
    run_indexed_parallel(
        jobs,
        threads,
        "failed to build dotnet planning thread pool",
        "internal error: missing dotnet plan slot",
        |(name, current)| {
            let resolved = nuget_resolve_target_with_min_age(
                nuget_client,
                &name,
                &current,
                now_unix_secs,
                min_age,
            )
            .map_err(|err| err.to_string());

            DotnetPlanItem {
                name,
                current,
                resolved,
            }
        },
    )
}

fn apply_dotnet_updates(upgradable: Vec<(String, String, String)>) {
    for (name, current, target) in upgradable {
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
        ) {
            let outcome = ItemOutcome::error(
                PLUGIN.id(),
                name,
                current,
                target,
                "nuget",
                REASON_COMMAND_FAILED,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
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
            "nuget",
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
    )?;

    let stdout = output.stdout()?;
    let stderr = output.stderr().unwrap_or_default();

    if !output.success() {
        if dotnet_missing_sdk_hint(&stdout) || dotnet_missing_sdk_hint(&stderr) {
            return Ok(Vec::new());
        }

        let err_text = if stderr.is_empty() { stdout } else { stderr };
        bail!("dotnet tool list --global --format json failed: {err_text}");
    }

    if stdout.trim().is_empty() {
        return Ok(Vec::new());
    }

    let parsed: DotnetToolListRoot =
        serde_json::from_str(&stdout).context("failed to parse dotnet tool list JSON")?;
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
) -> Result<NugetResolvedTarget> {
    let versions = nuget_versions_with_publish_times(nuget_client, package_id)?;

    let SemverAgeResolution {
        selected_version,
        latest_version,
        latest_age_secs,
    } = resolve_semver_with_min_age(current, &versions, now_unix_secs, min_age)
        .with_context(|| format!("failed to resolve eligible semver target for {package_id}"))?;

    Ok(NugetResolvedTarget {
        selected_version,
        latest_version,
        latest_age_secs,
    })
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

    let mut out = nuget_versions_with_publish_times_from_registration(
        client,
        package_id,
        &format!("https://api.nuget.org/v3/registration5-gz-semver2/{id_lower}/index.json"),
        true,
    )?;

    if out.is_empty() {
        out = nuget_versions_with_publish_times_from_registration(
            client,
            package_id,
            &format!("https://api.nuget.org/v3/registration5-semver1/{id_lower}/index.json"),
            false,
        )?;
    }

    if out.is_empty() {
        bail!("no NuGet versions with publish times found for {package_id}");
    }

    Ok(out)
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

fn emit_dotnet_manager_error(detail: impl AsRef<str>) {
    emit_manager_level_error(PLUGIN.id(), "nuget", detail);
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let target = NugetResolvedTarget {
            selected_version: Some("10.0.5".to_string()),
            latest_version: Some("10.0.5".to_string()),
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
        let target = NugetResolvedTarget {
            selected_version: Some("10.0.4".to_string()),
            latest_version: Some("10.0.5".to_string()),
            latest_age_secs: Some(2 * 24 * 60 * 60),
        };

        assert!(
            target
                .delayed_latest(Duration::from_secs(7 * 24 * 60 * 60))
                .is_some()
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
