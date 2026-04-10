use crate::config::ManagerMode;
use crate::manager::{ManagerCtx, ManagerPlugin};
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
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
        let (Some(latest_version), Some(latest_age_secs)) =
            (self.latest_version.as_deref(), self.latest_age_secs)
        else {
            return None;
        };

        if latest_age_secs >= min_age.as_secs() {
            return None;
        }

        if self.selected_version.as_deref() == Some(latest_version) {
            return None;
        }

        Some(DelayedLatest {
            latest_version: latest_version.to_string(),
            latest_age: human_age(latest_age_secs),
            required_age: human_age(min_age.as_secs()),
        })
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
    let min_age = ctx.policy.min_release_age.duration();

    let installed = dotnet_global_tools()?;
    if installed.is_empty() {
        return Ok(());
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX_EPOCH")?
        .as_secs();

    let nuget_client = crate::util::http::default_blocking_client()
        .context("failed to build NuGet HTTP client")?;

    let jobs: Vec<(String, String)> = installed
        .into_iter()
        .map(|entry| (entry.package_id, entry.version))
        .collect();

    let threads = effective_parallelism(ctx.max_parallel_checks, DOTNET_MAX_PARALLEL_CHECKS);
    let plan: Vec<DotnetPlanItem> = run_indexed_parallel(
        jobs,
        threads,
        "failed to build dotnet planning thread pool",
        "internal error: missing dotnet plan slot",
        |(name, current)| {
            let resolved = nuget_resolve_target_with_min_age(&nuget_client, &name, &current, now, min_age)
                .map_err(|err| err.to_string());

            DotnetPlanItem {
                name,
                current,
                resolved,
            }
        },
    )?;

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

    for (name, current, target) in upgradable {
        if let Err(err) = run_dotnet(&[
            "tool",
            "update",
            "--global",
            &name,
            "--version",
            &target,
            "--allow-downgrade",
        ]) {
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

    Ok(())
}

fn dotnet_global_tools() -> Result<Vec<DotnetToolEntry>> {
    let stdout = run_dotnet(&["tool", "list", "--global", "--format", "json"])?;
    let text = String::from_utf8(stdout).context("dotnet tool list output not UTF-8")?;

    if text.trim().is_empty() {
        return Ok(Vec::new());
    }

    let parsed: DotnetToolListRoot =
        serde_json::from_str(&text).context("failed to parse dotnet tool list JSON")?;
    Ok(parsed.data)
}

fn nuget_resolve_target_with_min_age(
    nuget_client: &Client,
    package_id: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<NugetResolvedTarget> {
    let current_ver = Version::parse(current)
        .with_context(|| format!("failed to parse current semver for {package_id}: {current}"))?;

    let versions = nuget_versions_with_publish_times(nuget_client, package_id)?;

    let mut newest_any: Option<(Version, String, u64)> = None;
    let mut eligible: Option<(Version, String, u64)> = None;

    for (version_raw, published_unix) in versions {
        let Ok(version) = Version::parse(&version_raw) else {
            continue;
        };

        if newest_any
            .as_ref()
            .is_none_or(|(curr, _, _)| version > *curr)
        {
            newest_any = Some((version.clone(), version_raw.clone(), published_unix));
        }

        if version >= current_ver {
            let age_secs = now_unix_secs.saturating_sub(published_unix);
            if age_secs >= min_age.as_secs()
                && eligible.as_ref().is_none_or(|(curr, _, _)| version > *curr)
            {
                eligible = Some((version, version_raw, published_unix));
            }
        }
    }

    let selected_version = eligible.map(|(_ver, raw, _)| raw);
    let (latest_version, latest_age_secs) =
        if let Some((_latest, latest_raw, latest_ts)) = newest_any {
            (
                Some(latest_raw),
                Some(now_unix_secs.saturating_sub(latest_ts)),
            )
        } else {
            (None, None)
        };

    Ok(NugetResolvedTarget {
        selected_version,
        latest_version,
        latest_age_secs,
    })
}

fn nuget_versions_with_publish_times(
    client: &Client,
    package_id: &str,
) -> Result<Vec<(String, u64)>> {
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
) -> Result<Vec<(String, u64)>> {
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

            out.push((entry.version, published));
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

fn run_dotnet(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("dotnet");
    command.args(args);
    run_command_checked_stdout(command)
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

        assert!(target.delayed_latest(Duration::from_secs(7 * 24 * 60 * 60)).is_none());
    }

    #[test]
    fn delayed_latest_present_when_latest_too_fresh() {
        let target = NugetResolvedTarget {
            selected_version: Some("10.0.4".to_string()),
            latest_version: Some("10.0.5".to_string()),
            latest_age_secs: Some(2 * 24 * 60 * 60),
        };

        assert!(target.delayed_latest(Duration::from_secs(7 * 24 * 60 * 60)).is_some());
    }
}
