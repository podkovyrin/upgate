use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, Pep440AgeResolution, Pep440Timestamp, PlanDecision, PlanMeta,
    emit_manager_level_error, emit_plan_and_collect_upgradable, emit_version_scan_outcomes,
    parse_pep440_release_timestamps, release_age_secs_for_pep440_version,
    resolve_pep440_with_min_age, verbose_now_unix_secs,
};
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::run_command_checked_stdout;
use crate::util::time::now_unix_secs;
use crate::util::timefmt::human_age;
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

const PIPX_MAX_PARALLEL_CHECKS: usize = 4;

pub(crate) struct PipxPlugin;

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

pub(crate) static PLUGIN: PipxPlugin = PipxPlugin;

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
        |item| PlanMeta {
            manager: PLUGIN.id(),
            source: "pypi",
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

    apply_pipx_updates(upgradable);

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
    run_indexed_parallel(
        jobs,
        threads,
        "failed to build pipx planning thread pool",
        "internal error: missing pipx plan slot",
        |(name, current)| {
            let resolved = pypi_resolve_target_with_min_age(
                pypi_client,
                &name,
                &current,
                now_unix_secs,
                min_age,
            )
            .map_err(|err| err.to_string());

            PipxPlanItem {
                name,
                current,
                resolved,
            }
        },
    )
}

fn apply_pipx_updates(upgradable: Vec<(String, String, String)>) {
    for (pkg, current, target) in upgradable {
        if let Err(err) = run_pipx(&["upgrade", &pkg]) {
            let outcome = ItemOutcome::error(
                PLUGIN.id(),
                pkg,
                current,
                target,
                "pypi",
                REASON_COMMAND_FAILED,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
        }
    }
}

fn pipx_installed_main_packages() -> Result<BTreeMap<String, String>> {
    let output = Command::new("pipx")
        .args(["list", "--json"])
        .output()
        .with_context(|| "failed to run pipx list --json")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("pipx list --json failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout).context("pipx list output not UTF-8")?;
    let root: PipxListRoot =
        serde_json::from_str(&stdout).context("failed to parse pipx list JSON")?;

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

impl PypiResolvedTarget {
    fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest> {
        DelayedLatest::from_latest(
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
    let url = format!("https://pypi.org/pypi/{pkg}/json");
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

fn run_pipx(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("pipx");
    command.args(args);
    run_command_checked_stdout(command)
}

fn emit_pipx_manager_error(detail: impl AsRef<str>) {
    emit_manager_level_error(PLUGIN.id(), "pypi", detail);
}
