use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, Pep440AgeResolution, Pep440Timestamp, PlanMeta, ResolvedPlanTarget,
    emit_manager_level_error, emit_plan_and_collect_upgradable, emit_version_scan_outcomes,
    parse_pep440_release_timestamps, plan_decision_from_resolution,
    release_age_secs_for_pep440_version, resolve_pep440_with_min_age, run_per_item_apply_flow,
    verbose_now_unix_secs,
};
use crate::outcome::{ItemOutcome, ReasonCode, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};
use crate::util::time::now_unix_secs;
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use std::collections::BTreeMap;
use std::time::Duration;

const PIPX_MAX_PARALLEL_CHECKS: usize = 4;

pub struct PipxPlugin;

impl ManagerPlugin for PipxPlugin {
    fn id(&self) -> &'static str {
        "pipx"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub static PLUGIN: PipxPlugin = PipxPlugin;

#[derive(Debug, serde::Deserialize)]
struct PipxListRoot {
    #[serde(default)]
    venvs: BTreeMap<String, PipxVenv>,
}

#[derive(Debug, serde::Deserialize)]
struct PipxVenv {
    metadata: PipxMetadata,
}

#[derive(Debug, serde::Deserialize)]
struct PipxMetadata {
    main_package: PipxMainPackage,
}

#[derive(Debug, serde::Deserialize)]
struct PipxMainPackage {
    package: String,
    package_version: String,
}

#[derive(Debug, serde::Deserialize)]
struct PypiRoot {
    #[serde(default)]
    info: PypiInfo,
    #[serde(default)]
    releases: BTreeMap<String, Vec<PypiReleaseFile>>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct PypiInfo {
    version: String,
}

#[derive(Debug, serde::Deserialize)]
struct PypiReleaseFile {
    upload_time_iso_8601: Option<String>,
    upload_time: Option<String>,
}

struct PipxPlanItem {
    name: String,
    current: String,
    resolved: Result<PypiResolvedTarget, String>,
}

fn run(ctx: &ManagerCtx) -> Result<()> {
    if ctx.is_scan() {
        return scan(ctx);
    }

    let min_age = ctx.policy.min_release_age.duration();

    let installed = pipx_installed_main_packages()?;
    if installed.is_empty() {
        return Ok(());
    }

    let now = now_unix_secs()?;

    let pypi_client =
        crate::util::http::default_blocking_client().context("failed to build PyPI HTTP client")?;

    let plan = resolve_pipx_plan(
        installed,
        &pypi_client,
        now,
        min_age,
        ctx.max_parallel_checks,
    )?;

    let upgradable = emit_plan_and_collect_upgradable(
        plan,
        |item| {
            let PipxPlanItem {
                name,
                current,
                resolved,
            } = item;

            let decision = plan_decision_from_resolution(&current, resolved, min_age);

            (
                PlanMeta {
                    manager: PLUGIN.id(),
                    source: "pypi",
                    name,
                    current,
                },
                decision,
            )
        },
        ctx.is_interactive_apply(),
        Some(&ctx.policy.pinned),
    );

    run_per_item_apply_flow(ctx, PLUGIN.id(), upgradable, apply_pipx_updates)?;

    Ok(())
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let installed = match pipx_installed_main_packages() {
        Ok(installed) => installed,
        Err(err) => {
            emit_pipx_manager_error(format!("failed to read installed pipx tools: {err}"));
            return Ok(());
        }
    };

    if installed.is_empty() {
        return Ok(());
    }

    let now = verbose_now_unix_secs()?;

    let pypi_client = if now.is_some() {
        crate::util::http::default_blocking_client().ok()
    } else {
        None
    };

    emit_version_scan_outcomes(
        PLUGIN.id(),
        "pypi",
        installed,
        now,
        ctx.scan_old_age_threshold,
        |name, version, now_unix_secs| {
            pypi_client.as_ref().map_or(Ok(None), |client| {
                pypi_release_age_secs(client, name, version, now_unix_secs)
            })
        },
    );
    Ok(())
}

fn resolve_pipx_plan(
    installed: BTreeMap<String, String>,
    pypi_client: &Client,
    now_unix_secs: u64,
    min_age: Duration,
    max_parallel_checks: usize,
) -> Result<Vec<PipxPlanItem>> {
    let jobs: Vec<(String, String)> = installed.into_iter().collect();

    let threads = effective_parallelism(max_parallel_checks, PIPX_MAX_PARALLEL_CHECKS);
    run_indexed_parallel(jobs, threads, PLUGIN.id(), |(name, current)| {
        let resolved =
            pypi_resolve_target_with_min_age(pypi_client, &name, &current, now_unix_secs, min_age)
                .map_err(|err| err.to_string());

        PipxPlanItem {
            name,
            current,
            resolved,
        }
    })
}

fn apply_pipx_updates(upgradable: Vec<crate::managers::common::PlannedUpdate>) {
    for item in upgradable {
        let pkg = item.name;
        let current = item.current;
        let target = item.target;
        let spec = format!("{pkg}=={target}");
        if let Err(err) = run_cmd("pipx", ["upgrade", &spec], CmdStatus::Success)
            .mutating()
            .output()
        {
            let outcome = ItemOutcome::error(
                PLUGIN.id(),
                pkg,
                current,
                target,
                "pypi",
                ReasonCode::CommandFailed,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
        }
    }
}

fn pipx_installed_main_packages() -> Result<BTreeMap<String, String>> {
    let root: PipxListRoot = run_cmd("pipx", ["list", "--json"], CmdStatus::Success)
        .output()?
        .json()?;

    let mut out = BTreeMap::new();
    for (_venv_name, venv) in root.venvs {
        out.insert(
            venv.metadata.main_package.package,
            venv.metadata.main_package.package_version,
        );
    }

    Ok(out)
}

struct PypiResolvedTarget {
    selected_version: Option<String>,
    latest_version: Option<String>,
    latest_age_secs: Option<u64>,
}

impl ResolvedPlanTarget for PypiResolvedTarget {
    fn selected_version(&self) -> Option<&str> {
        self.selected_version.as_deref()
    }

    fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest> {
        DelayedLatest::from_too_fresh_latest(
            self.selected_version.as_deref(),
            self.latest_version.as_deref(),
            self.latest_age_secs,
            min_age,
        )
    }
}

fn pypi_resolve_target_with_min_age(
    pypi_client: &Client,
    pkg: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<PypiResolvedTarget> {
    let root = pypi_root(pypi_client, pkg)?;
    let releases = pypi_pep440_releases(pkg, &root)?;

    let Pep440AgeResolution {
        selected_version,
        latest_version,
        latest_age_secs,
    } = resolve_pep440_with_min_age(current, &releases, now_unix_secs, min_age)
        .with_context(|| format!("failed to resolve eligible PEP440 target for {pkg}"))?;

    let _ = root.info.version;
    Ok(PypiResolvedTarget {
        selected_version,
        latest_version,
        latest_age_secs,
    })
}

fn pypi_release_age_secs(
    pypi_client: &Client,
    pkg: &str,
    version: &str,
    now_unix_secs: u64,
) -> Result<Option<u64>> {
    let root = pypi_root(pypi_client, pkg)?;
    let releases = pypi_pep440_releases(pkg, &root)?;

    Ok(release_age_secs_for_pep440_version(
        &releases,
        version,
        now_unix_secs,
    ))
}

fn pypi_pep440_releases(pkg: &str, root: &PypiRoot) -> Result<Vec<Pep440Timestamp>> {
    parse_pep440_release_timestamps(
        pkg,
        &root.releases,
        |file| file.upload_time_iso_8601.as_deref(),
        |file| file.upload_time.as_deref(),
    )
}

fn pypi_root(pypi_client: &Client, pkg: &str) -> Result<PypiRoot> {
    let base_url = pypi_base_url();
    let url = format!("{base_url}/pypi/{pkg}/json");
    let body = pypi_client
        .get(&url)
        .send()
        .with_context(|| format!("failed to GET {url}"))?
        .error_for_status()
        .with_context(|| format!("PyPI returned error for {pkg}"))?
        .text()
        .with_context(|| format!("failed to read PyPI response body for {pkg}"))?;

    serde_json::from_str(&body).with_context(|| format!("failed to parse PyPI JSON for {pkg}"))
}

fn pypi_base_url() -> String {
    std::env::var("UPNOW_PIPX_PYPI_BASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://pypi.org".to_string())
}

fn emit_pipx_manager_error(detail: impl AsRef<str>) {
    emit_manager_level_error(PLUGIN.id(), "pypi", detail);
}
