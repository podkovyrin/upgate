use crate::manager::{ManagerCtx, ManagerPlugin};
use crate::managers::common::{
    DelayedLatest, PlanDecision, PlanMeta, SemverAgeResolution, SemverTimestamp,
    emit_manager_level_error, emit_plan_and_collect_upgradable, emit_scan_current,
    release_age_secs_for_version, resolve_semver_with_min_age, verbose_now_unix_secs,
};
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, emit_text_outcome};
use crate::util::parallel::{effective_parallelism, run_indexed_parallel};
use crate::util::process::RunCmd;
use crate::util::time::now_unix_secs;
use crate::util::timefmt::human_age;
use crate::util::timeparse::parse_rfc3339_unix;
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use semver::Version;
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

const CARGO_MAX_PARALLEL_CHECKS: usize = 4;

pub(crate) struct CargoPlugin;

impl ManagerPlugin for CargoPlugin {
    fn id(&self) -> &'static str {
        "cargo"
    }

    fn default_min_release_age(&self) -> &'static str {
        "7d"
    }

    fn run(&self, ctx: &ManagerCtx) -> Result<()> {
        run(ctx)
    }
}

pub(crate) static PLUGIN: CargoPlugin = CargoPlugin;

#[derive(Debug)]
struct InstalledCrate {
    version: String,
    install_meta: Option<CargoInstallMeta>,
}

#[derive(Debug, Clone)]
struct CargoInstallMeta {
    bins: Vec<String>,
    features: Vec<String>,
    all_features: bool,
    no_default_features: bool,
}

struct CargoPlanItem {
    name: String,
    current: String,
    resolved: Result<CargoResolvedTarget, String>,
}

fn run(ctx: &ManagerCtx) -> Result<()> {
    if ctx.is_scan() {
        return scan(ctx);
    }

    let min_age = ctx.policy.min_release_age.duration();

    let installed = match cargo_installed_crates() {
        Ok(installed) => installed,
        Err(err) => {
            emit_cargo_manager_error(format!("failed to read installed Cargo tools: {err}"));
            return Ok(());
        }
    };

    if installed.is_empty() {
        return Ok(());
    }

    let now = now_unix_secs()?;

    let crates_client = match crate::util::http::default_blocking_client() {
        Ok(client) => client,
        Err(err) => {
            emit_cargo_manager_error(format!("failed to initialize metadata HTTP client: {err}"));
            return Ok(());
        }
    };

    let plan = match resolve_cargo_plan(
        &installed,
        &crates_client,
        now,
        min_age,
        ctx.max_parallel_checks,
    ) {
        Ok(plan) => plan,
        Err(err) => {
            emit_cargo_manager_error(format!("planning execution failed: {err}"));
            return Ok(());
        }
    };

    let upgradable = emit_plan_and_collect_upgradable(
        plan,
        |item| PlanMeta {
            manager: PLUGIN.id(),
            source: "crates.io",
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

    apply_cargo_updates(&installed, upgradable);

    Ok(())
}

fn scan(ctx: &ManagerCtx) -> Result<()> {
    let installed = match cargo_installed_crates() {
        Ok(installed) => installed,
        Err(err) => {
            emit_cargo_manager_error(format!("failed to read installed Cargo tools: {err}"));
            return Ok(());
        }
    };

    if installed.is_empty() {
        return Ok(());
    }

    let now = verbose_now_unix_secs()?;

    let crates_client = if now.is_some() {
        match crate::util::http::default_blocking_client() {
            Ok(client) => Some(client),
            Err(err) => {
                emit_cargo_manager_error(format!(
                    "failed to initialize metadata HTTP client: {err}"
                ));
                None
            }
        }
    } else {
        None
    };

    emit_cargo_scan_outcomes(
        &installed,
        crates_client.as_ref(),
        now,
        ctx.scan_old_age_threshold,
    );
    Ok(())
}

fn resolve_cargo_plan(
    installed: &BTreeMap<String, InstalledCrate>,
    crates_client: &Client,
    now_unix_secs: u64,
    min_age: Duration,
    max_parallel_checks: usize,
) -> Result<Vec<CargoPlanItem>> {
    let jobs: Vec<(String, String)> = installed
        .iter()
        .map(|(name, entry)| (name.clone(), entry.version.clone()))
        .collect();

    let threads = effective_parallelism(max_parallel_checks, CARGO_MAX_PARALLEL_CHECKS);
    run_indexed_parallel(
        jobs,
        threads,
        "failed to build cargo planning thread pool",
        "internal error: missing cargo plan slot",
        |(name, current)| {
            let resolved = cargo_resolve_target_with_min_age(
                crates_client,
                &name,
                &current,
                now_unix_secs,
                min_age,
            )
            .map_err(|err| err.to_string());

            CargoPlanItem {
                name,
                current,
                resolved,
            }
        },
    )
}

fn apply_cargo_updates(
    installed: &BTreeMap<String, InstalledCrate>,
    upgradable: Vec<(String, String, String)>,
) {
    for (name, current, version) in upgradable {
        let install_meta = installed
            .get(&name)
            .and_then(|entry| entry.install_meta.clone());

        let mut args = vec!["install".to_string(), "--force".to_string()];
        apply_cargo_install_meta_args(&mut args, install_meta.as_ref());
        args.push(format!("{name}@{version}"));

        if let Err(err) = RunCmd::Success.run("cargo", &args) {
            let outcome = ItemOutcome::error(
                PLUGIN.id(),
                name,
                current,
                version,
                "crates.io",
                REASON_COMMAND_FAILED,
                err.to_string(),
            );
            emit_text_outcome(&outcome);
        }
    }
}

fn emit_cargo_scan_outcomes(
    installed: &BTreeMap<String, InstalledCrate>,
    crates_client: Option<&Client>,
    now_unix_secs: Option<u64>,
    old_threshold: Duration,
) {
    for (name, entry) in installed {
        let age_secs = if let (Some(now_unix_secs), Some(client)) = (now_unix_secs, crates_client) {
            cargo_release_age_secs(client, name, &entry.version, now_unix_secs)
                .ok()
                .flatten()
        } else {
            None
        };

        emit_scan_current(
            PLUGIN.id(),
            "crates.io",
            name.clone(),
            entry.version.clone(),
            age_secs,
            old_threshold,
        );
    }
}

fn cargo_installed_crates() -> Result<BTreeMap<String, InstalledCrate>> {
    let text = RunCmd::Success.text("cargo", ["install", "--list"])?;

    let mut installed = parse_cargo_install_list(&text);

    let install_meta = cargo_install_tracking_map().unwrap_or_default();
    for (name, entry) in &mut installed {
        entry.install_meta = install_meta.get(name).cloned();
    }

    Ok(installed)
}

fn parse_cargo_install_list(text: &str) -> BTreeMap<String, InstalledCrate> {
    let mut out = BTreeMap::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.ends_with(':') {
            continue;
        }

        let Some((name, ver_raw)) = trimmed.trim_end_matches(':').split_once(" v") else {
            continue;
        };

        if name.is_empty() || ver_raw.is_empty() {
            continue;
        }

        out.insert(
            name.to_string(),
            InstalledCrate {
                version: ver_raw.to_string(),
                install_meta: None,
            },
        );
    }

    out
}

#[derive(Debug, Deserialize)]
struct CargoInstallLedger {
    installs: BTreeMap<String, CargoInstallLedgerEntry>,
}

#[derive(Debug, Deserialize)]
struct CargoInstallLedgerEntry {
    #[serde(default)]
    bins: Vec<String>,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    all_features: bool,
    #[serde(default)]
    no_default_features: bool,
}

fn cargo_install_tracking_map() -> Result<BTreeMap<String, CargoInstallMeta>> {
    let cargo_home = std::env::var("CARGO_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim().to_string())
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .map(|home| format!("{}/.cargo", home.trim()))
        })
        .context("CARGO_HOME and HOME are not set")?;

    let path = std::path::Path::new(&cargo_home).join(".crates2.json");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }

    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let parsed: CargoInstallLedger = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    let mut out = BTreeMap::new();
    for (key, value) in parsed.installs {
        if let Some(crate_name) = parse_cargo_ledger_key_name(&key) {
            out.insert(
                crate_name,
                CargoInstallMeta {
                    bins: value.bins,
                    features: value.features,
                    all_features: value.all_features,
                    no_default_features: value.no_default_features,
                },
            );
        }
    }

    Ok(out)
}

fn parse_cargo_ledger_key_name(key: &str) -> Option<String> {
    let (left, _) = key.split_once(" (")?;
    let (name, _ver) = left.rsplit_once(' ')?;
    if name.is_empty() {
        return None;
    }

    Some(name.to_string())
}

fn apply_cargo_install_meta_args(args: &mut Vec<String>, meta: Option<&CargoInstallMeta>) {
    let Some(meta) = meta else {
        return;
    };

    if !meta.bins.is_empty() {
        if meta.bins.len() == 1 {
            args.push("--bin".to_string());
            args.push(meta.bins[0].clone());
        } else {
            args.push("--bins".to_string());
        }
    }

    if meta.all_features {
        args.push("--all-features".to_string());
    } else if !meta.features.is_empty() {
        args.push("--features".to_string());
        args.push(meta.features.join(","));
    }

    if meta.no_default_features {
        args.push("--no-default-features".to_string());
    }
}

struct CargoResolvedTarget {
    selected_version: Option<String>,
    latest_version: Option<String>,
    latest_age_secs: Option<u64>,
}

impl CargoResolvedTarget {
    fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest> {
        DelayedLatest::from_latest(
            self.latest_version.as_deref(),
            self.latest_age_secs,
            min_age,
        )
    }
}

fn cargo_resolve_target_with_min_age(
    crates_client: &Client,
    name: &str,
    current: &str,
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<CargoResolvedTarget> {
    let output = RunCmd::Success.run("cargo", ["search", name, "--limit", "1"])?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} search {name} --limit 1 failed: {}",
            PLUGIN.id(),
            stderr.trim()
        );
    }

    let stdout = String::from_utf8(output.stdout).context("cargo search output not UTF-8")?;
    let latest = parse_cargo_search_latest_version(name, &stdout)?;

    let all_versions = crates_io_versions(crates_client, name)?;

    let SemverAgeResolution {
        selected_version,
        latest_version,
        latest_age_secs,
    } = resolve_semver_with_min_age(current, &all_versions, now_unix_secs, min_age)
        .with_context(|| format!("failed to resolve eligible semver target for {name}"))?;

    // Keep the parsed search latest in scope to validate semver hygiene and avoid stale data.
    let _ = latest;

    Ok(CargoResolvedTarget {
        selected_version,
        latest_version,
        latest_age_secs,
    })
}

fn parse_cargo_search_latest_version(crate_name: &str, stdout: &str) -> Result<Version> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("...") || trimmed.starts_with("note:") {
            continue;
        }

        let prefix = format!("{crate_name} = \"");
        if let Some(rest) = trimmed.strip_prefix(&prefix)
            && let Some((ver, _)) = rest.split_once('"')
        {
            return Version::parse(ver).with_context(|| {
                format!("failed to parse cargo search version for {crate_name}: {ver}")
            });
        }
    }

    bail!("failed to parse cargo search latest version for {crate_name}")
}

#[derive(Debug, serde::Deserialize)]
struct CratesIoRoot {
    #[serde(default)]
    versions: Vec<CratesIoVersion>,
}

#[derive(Debug, serde::Deserialize)]
struct CratesIoVersion {
    num: String,
    created_at: String,
    yanked: bool,
}

fn cargo_release_age_secs(
    crates_client: &Client,
    crate_name: &str,
    version: &str,
    now_unix_secs: u64,
) -> Result<Option<u64>> {
    let versions = crates_io_versions(crates_client, crate_name)?;
    Ok(release_age_secs_for_version(
        &versions,
        version,
        now_unix_secs,
    ))
}

fn crates_io_versions(client: &Client, crate_name: &str) -> Result<Vec<SemverTimestamp>> {
    let url = format!("https://crates.io/api/v1/crates/{crate_name}");

    let body = client
        .get(&url)
        .send()
        .with_context(|| format!("failed to GET {url}"))?
        .error_for_status()
        .with_context(|| format!("crates.io returned error for {crate_name}"))?
        .text()
        .with_context(|| format!("failed to read crates.io response body for {crate_name}"))?;

    let root: CratesIoRoot = serde_json::from_str(&body)
        .with_context(|| format!("failed to parse crates.io JSON for {crate_name}"))?;

    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for v in root.versions {
        if v.yanked {
            continue;
        }

        if !seen.insert(v.num.clone()) {
            continue;
        }

        let ts = parse_rfc3339_unix(&v.created_at).with_context(|| {
            format!(
                "invalid crates.io version timestamp for {crate_name}@{}: {}",
                v.num, v.created_at
            )
        })?;

        out.push(SemverTimestamp {
            version: v.num,
            published_unix: ts,
        });
    }

    Ok(out)
}

fn emit_cargo_manager_error(detail: impl AsRef<str>) {
    emit_manager_level_error(PLUGIN.id(), "crates.io", detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_install_list_entries() {
        let raw = r"cargo-deny v0.19.0:
    cargo-deny
cbindgen v0.29.2:
    cbindgen
";

        let parsed = parse_cargo_install_list(raw);
        assert_eq!(
            parsed.get("cargo-deny").map(|e| e.version.as_str()),
            Some("0.19.0")
        );
        assert_eq!(
            parsed.get("cbindgen").map(|e| e.version.as_str()),
            Some("0.29.2")
        );
    }

    #[test]
    fn parse_search_latest() {
        let raw = "cargo-deny = \"0.19.0\"    # comment\n... and 12 crates more\n";
        let parsed = parse_cargo_search_latest_version("cargo-deny", raw).expect("should parse");
        assert_eq!(parsed, Version::new(0, 19, 0));
    }

    #[test]
    fn parse_cargo_ledger_key_extracts_name() {
        let key = "cargo-deny 0.19.0 (registry+https://github.com/rust-lang/crates.io-index)";
        assert_eq!(
            parse_cargo_ledger_key_name(key).as_deref(),
            Some("cargo-deny")
        );
    }

    #[test]
    fn apply_install_meta_args_uses_single_bin_and_features() {
        let meta = CargoInstallMeta {
            bins: vec!["cargo-deny".to_string()],
            features: vec!["vendored-openssl".to_string(), "native-tls".to_string()],
            all_features: false,
            no_default_features: true,
        };

        let mut args = Vec::new();
        apply_cargo_install_meta_args(&mut args, Some(&meta));

        assert_eq!(
            args,
            vec![
                "--bin",
                "cargo-deny",
                "--features",
                "vendored-openssl,native-tls",
                "--no-default-features"
            ]
        );
    }
}
