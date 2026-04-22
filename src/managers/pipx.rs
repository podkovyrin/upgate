use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::blocking::Client;

use crate::managers::shared::versioning::policy::VersionPolicy;
#[allow(clippy::wildcard_imports)]
use crate::managers::*;
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::{CmdStatus, run_cmd};

const PIPX_MAX_PARALLEL_CHECKS: usize = 4;

pub struct PipxPlugin;

impl ManagerPlugin for PipxPlugin {
    fn id(&self) -> &'static str {
        "pipx"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn supports_version_policy(&self, _policy: VersionPolicy) -> bool {
        true
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

type PipxPlanItem = ResolvedPlanItem<AgeResolvedTarget>;

fn run(ctx: &ManagerCtx) -> Result<()> {
    run_manager_pipeline(ctx, scan, run_plan_apply)
}

fn run_plan_apply(ctx: &ManagerCtx) -> Result<()> {
    run_plan_apply_framework(
        ctx,
        PLUGIN.id(),
        PlanApplyFrameworkPolicy::STRICT_FETCH_STRICT_RESOLVE,
        || pipx_installed_main_packages().context("failed to read installed pipx tools"),
        BTreeMap::is_empty,
        |installed, runtime| {
            resolve_pipx_plan(
                installed.clone(),
                runtime.now_unix_secs,
                runtime.min_age,
                runtime.max_parallel_checks,
                ctx.policy.version_policy,
            )
            .context("planning execution failed")
        },
        |_installed, plan, runtime| {
            Ok(collect_upgradable_from_resolved_plan(
                PLUGIN.id(),
                plan,
                runtime.min_age,
                runtime.suppress_update_outcomes,
                runtime.pinned,
            ))
        },
        |ctx, _installed, upgradable| {
            run_per_item_apply_flow(ctx, PLUGIN.id(), upgradable, apply_pipx_updates)
        },
    )
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let Some(installed) = soft_fail(
        pipx_installed_main_packages(),
        PLUGIN.id(),
        "failed to read installed pipx tools",
    ) else {
        return Ok(());
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
    now_unix_secs: u64,
    min_age: Duration,
    max_parallel_checks: usize,
    version_policy: VersionPolicy,
) -> Result<Vec<PipxPlanItem>> {
    let pypi_client =
        crate::util::http::default_blocking_client().context("failed to build PyPI HTTP client")?;
    let jobs: Vec<(String, String)> = installed.into_iter().collect();

    let threads = effective_parallelism(max_parallel_checks, PIPX_MAX_PARALLEL_CHECKS);
    run_indexed_parallel(jobs, threads, PLUGIN.id(), |(name, current)| {
        let resolved = pypi_resolve_target_with_min_age(
            &pypi_client,
            &name,
            &current,
            now_unix_secs,
            min_age,
            version_policy,
        )
        .map_err(|err| err.to_string());

        PipxPlanItem::new(name, current, resolved)
    })
}

fn apply_pipx_updates(upgradable: Vec<crate::managers::PlannedUpdate>) {
    for item in upgradable {
        let pkg = item.name;
        let current = item.current;
        let target = item.target;
        let spec = format!("{pkg}=={target}");
        if let Err(err) = run_cmd("pipx", ["upgrade", &spec], CmdStatus::Success)
            .mutating()
            .output()
        {
            emit_apply_error(PLUGIN.id(), pkg, current, target, err);
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

fn pypi_resolve_target_with_min_age(
    pypi_client: &Client,
    pkg: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
    version_policy: VersionPolicy,
) -> Result<AgeResolvedTarget> {
    let root = pypi_root(pypi_client, pkg)?;
    let releases = pypi_pep440_releases(pkg, &root)?;

    let resolved =
        resolve_pep440_with_min_age(current, &releases, now_unix_secs, min_age, version_policy)
            .with_context(|| format!("failed to resolve eligible PEP440 target for {pkg}"))?;

    let _ = root.info.version;
    Ok(resolved.into())
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
    crate::util::http::env_base_url("UPNOW_PIPX_PYPI_BASE_URL", "https://pypi.org")
}
