use crate::Cli;
use crate::manager::Manager;
use crate::outcome::{
    ItemOutcome, REASON_COMMAND_FAILED, REASON_MISSING_METADATA, REASON_PINNED, emit_text_outcome,
};
use crate::process::run_command_checked_stdout;
use crate::timefmt::human_age;
use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Deserialize)]
struct OutdatedRoot {
    formulae: Vec<OutdatedFormula>,
    casks: Vec<OutdatedCask>,
}

#[derive(Debug, Deserialize)]
struct OutdatedFormula {
    name: String,
    installed_versions: Vec<String>,
    current_version: String,
    pinned: bool,
    pinned_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OutdatedCask {
    name: String,
    installed_versions: Vec<String>,
    current_version: String,
}

#[derive(Debug, Deserialize)]
struct InfoRoot {
    #[serde(default)]
    formulae: Vec<FormulaInfo>,
    #[serde(default)]
    casks: Vec<CaskInfo>,
}

#[derive(Debug, Deserialize)]
struct FormulaInfo {
    full_name: String,
    tap: Option<String>,
    ruby_source_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CaskInfo {
    token: String,
    tap: Option<String>,
    ruby_source_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TapInfo {
    name: String,
    path: String,
    remote: Option<String>,
    branch: Option<String>,
}

#[derive(Debug)]
struct TapMeta {
    path: String,
    remote: Option<String>,
    branch: Option<String>,
}

#[derive(Clone, Copy)]
enum DataSource {
    Git,
    Api,
    None,
}

impl DataSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Api => "api",
            Self::None => "n/a",
        }
    }
}

enum PlanAction {
    Upgrade {
        source: DataSource,
    },
    Delayed {
        age: String,
        required: String,
        source: DataSource,
    },
    Skipped {
        reason: String,
        source: DataSource,
    },
}

struct PlanItem {
    name: String,
    installed: String,
    target: String,
    action: PlanAction,
    is_formula: bool,
}

struct PackageJob {
    name: String,
    installed: String,
    target: String,
    is_formula: bool,
    initial_skip_reason: Option<String>,
    tap_and_source: Option<(Option<String>, Option<String>)>,
}

#[derive(Clone)]
struct ApiJob {
    index: usize,
    name: String,
    installed: String,
    target: String,
    is_formula: bool,
    remote: String,
    branch: Option<String>,
    source_path: String,
    local_err: String,
}

enum PhaseOneResult {
    Final(PlanItem),
    NeedsApi(ApiJob),
}

#[derive(Debug, Deserialize)]
struct GitHubCommitItem {
    commit: GitHubCommit,
}

#[derive(Debug, Deserialize)]
struct GitHubCommit {
    author: Option<GitHubCommitPerson>,
    committer: Option<GitHubCommitPerson>,
}

#[derive(Debug, Deserialize)]
struct GitHubCommitPerson {
    date: String,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn run(cli: &Cli) -> Result<()> {
    let min_age = parse_duration(&cli.min_release_age)?;

    if !cli.no_update {
        run_brew(&["update", "--quiet"])?;
    }

    let outdated: OutdatedRoot = brew_json(&["outdated", "--json=v2"])?;
    if outdated.formulae.is_empty() && outdated.casks.is_empty() {
        return Ok(());
    }

    let formula_names: Vec<String> = outdated.formulae.iter().map(|f| f.name.clone()).collect();
    let cask_names: Vec<String> = outdated.casks.iter().map(|c| c.name.clone()).collect();

    let info = brew_info_for_names(&formula_names, &cask_names)?;
    let tap_meta = brew_tap_meta()?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX_EPOCH")?
        .as_secs();

    let github_client = github_client()?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cli.max_parallel_checks.max(1))
        .build()
        .context("failed to build rayon thread pool")?;

    let mut formula_info_by_name: HashMap<String, FormulaInfo> = HashMap::new();
    for formula in info.formulae {
        formula_info_by_name.insert(formula.full_name.clone(), formula);
    }

    let mut cask_info_by_name: HashMap<String, CaskInfo> = HashMap::new();
    for cask in info.casks {
        cask_info_by_name.insert(cask.token.clone(), cask);
    }

    let mut jobs = Vec::with_capacity(outdated.formulae.len() + outdated.casks.len());

    for item in outdated.formulae {
        let installed = item
            .installed_versions
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let initial_skip_reason = if item.pinned {
            Some(match item.pinned_version {
                Some(v) => format!("pinned at {v}"),
                None => "pinned".to_string(),
            })
        } else {
            None
        };

        let tap_and_source = formula_info_by_name
            .get(&item.name)
            .map(|f| (f.tap.clone(), f.ruby_source_path.clone()));

        jobs.push(PackageJob {
            name: item.name,
            installed,
            target: item.current_version,
            is_formula: true,
            initial_skip_reason,
            tap_and_source,
        });
    }

    for item in outdated.casks {
        let installed = item
            .installed_versions
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());

        let tap_and_source = cask_info_by_name
            .get(&item.name)
            .map(|c| (c.tap.clone(), c.ruby_source_path.clone()));

        jobs.push(PackageJob {
            name: item.name,
            installed,
            target: item.current_version,
            is_formula: false,
            initial_skip_reason: None,
            tap_and_source,
        });
    }

    let phase_one_results: Vec<PhaseOneResult> = pool.install(|| {
        jobs.into_par_iter()
            .enumerate()
            .map(|(index, job)| phase_one_local_check(index, job, min_age, now, &tap_meta))
            .collect()
    });

    let mut plan_slots: Vec<Option<PlanItem>> =
        (0..phase_one_results.len()).map(|_| None).collect();
    let mut api_jobs = Vec::new();

    for (index, result) in phase_one_results.into_iter().enumerate() {
        match result {
            PhaseOneResult::Final(item) => plan_slots[index] = Some(item),
            PhaseOneResult::NeedsApi(job) => api_jobs.push(job),
        }
    }

    if !api_jobs.is_empty() {
        let api_parallelism = cli.max_parallel_checks.clamp(1, 4);
        let api_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(api_parallelism)
            .build()
            .context("failed to build API fallback thread pool")?;

        let api_results: Vec<(usize, PlanItem)> = api_pool.install(|| {
            api_jobs
                .into_par_iter()
                .map(|job| {
                    let action = match github_last_commit_unix_seconds(
                        &github_client,
                        &job.remote,
                        job.branch.as_deref(),
                        &job.source_path,
                    ) {
                        Ok(ts) => action_from_commit_age(min_age, now, ts, DataSource::Api),
                        Err(github_err) => PlanAction::Skipped {
                            reason: format!(
                                "failed age check: local git failed ({}); GitHub fallback failed ({})",
                                job.local_err, github_err
                            ),
                            source: DataSource::Api,
                        },
                    };

                    (
                        job.index,
                        PlanItem {
                            name: job.name,
                            installed: job.installed,
                            target: job.target,
                            action,
                            is_formula: job.is_formula,
                        },
                    )
                })
                .collect()
        });

        for (index, item) in api_results {
            plan_slots[index] = Some(item);
        }
    }

    let plan: Vec<PlanItem> = plan_slots
        .into_iter()
        .map(|item| item.context("internal error: missing plan slot"))
        .collect::<Result<Vec<_>>>()?;

    for item in &plan {
        let outcome = item_to_outcome(item);
        emit_text_outcome(&outcome);
    }

    if cli.dry_run {
        return Ok(());
    }

    let formula_to_upgrade: Vec<String> = plan
        .iter()
        .filter(|i| i.is_formula)
        .filter_map(|i| match i.action {
            PlanAction::Upgrade { .. } => Some(i.name.clone()),
            PlanAction::Delayed { .. } | PlanAction::Skipped { .. } => None,
        })
        .collect();

    let casks_to_upgrade: Vec<String> = plan
        .iter()
        .filter(|i| !i.is_formula)
        .filter_map(|i| match i.action {
            PlanAction::Upgrade { .. } => Some(i.name.clone()),
            PlanAction::Delayed { .. } | PlanAction::Skipped { .. } => None,
        })
        .collect();

    if !formula_to_upgrade.is_empty() {
        let mut args = vec!["upgrade".to_string(), "--formula".to_string()];
        args.extend(formula_to_upgrade);
        run_brew_owned(&args)?;
    }

    if !casks_to_upgrade.is_empty() {
        let mut args = vec!["upgrade".to_string(), "--cask".to_string()];
        args.extend(casks_to_upgrade);
        run_brew_owned(&args)?;
    }

    Ok(())
}

fn phase_one_local_check(
    index: usize,
    job: PackageJob,
    min_age: Duration,
    now_unix_secs: u64,
    tap_meta: &HashMap<String, TapMeta>,
) -> PhaseOneResult {
    if let Some(reason) = job.initial_skip_reason {
        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped {
                reason,
                source: DataSource::None,
            },
            is_formula: job.is_formula,
        });
    }

    let Some((tap, source_path)) = job.tap_and_source.as_ref() else {
        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped {
                reason: "unable to resolve package metadata from brew info".to_string(),
                source: DataSource::None,
            },
            is_formula: job.is_formula,
        });
    };

    let Some(tap) = tap.as_deref() else {
        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped {
                reason: "missing tap".to_string(),
                source: DataSource::None,
            },
            is_formula: job.is_formula,
        });
    };

    let Some(source_path) = source_path.as_deref() else {
        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped {
                reason: "missing ruby_source_path".to_string(),
                source: DataSource::None,
            },
            is_formula: job.is_formula,
        });
    };

    let Some(tap_meta) = tap_meta.get(tap) else {
        if let Some((remote, branch)) = resolve_api_fallback_remote_branch(tap, None) {
            return PhaseOneResult::NeedsApi(ApiJob {
                index,
                name: job.name,
                installed: job.installed,
                target: job.target,
                is_formula: job.is_formula,
                remote,
                branch,
                source_path: source_path.to_string(),
                local_err: format!("tap '{tap}' is not installed locally"),
            });
        }

        return PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: PlanAction::Skipped {
                reason: format!("tap '{tap}' is not installed locally"),
                source: DataSource::None,
            },
            is_formula: job.is_formula,
        });
    };

    match git_last_commit_unix_seconds(&tap_meta.path, tap_meta.branch.as_deref(), source_path) {
        Ok(ts) => PhaseOneResult::Final(PlanItem {
            name: job.name,
            installed: job.installed,
            target: job.target,
            action: action_from_commit_age(min_age, now_unix_secs, ts, DataSource::Git),
            is_formula: job.is_formula,
        }),
        Err(local_err) => {
            if let Some((remote, branch)) = resolve_api_fallback_remote_branch(tap, Some(tap_meta))
            {
                PhaseOneResult::NeedsApi(ApiJob {
                    index,
                    name: job.name,
                    installed: job.installed,
                    target: job.target,
                    is_formula: job.is_formula,
                    remote,
                    branch,
                    source_path: source_path.to_string(),
                    local_err: local_err.to_string(),
                })
            } else {
                PhaseOneResult::Final(PlanItem {
                    name: job.name,
                    installed: job.installed,
                    target: job.target,
                    action: PlanAction::Skipped {
                        reason: format!(
                            "failed age check: local git failed ({local_err}) and no remote fallback available"
                        ),
                        source: DataSource::None,
                    },
                    is_formula: job.is_formula,
                })
            }
        }
    }
}

fn item_to_outcome(item: &PlanItem) -> ItemOutcome {
    match &item.action {
        PlanAction::Upgrade { source } => ItemOutcome::update(
            Manager::Brew,
            item.name.clone(),
            item.installed.clone(),
            item.target.clone(),
            source.as_str(),
        ),
        PlanAction::Delayed {
            age,
            required,
            source,
        } => ItemOutcome::delayed_too_fresh(
            Manager::Brew,
            item.name.clone(),
            item.installed.clone(),
            item.target.clone(),
            source.as_str(),
            age.clone(),
            required.clone(),
        ),
        PlanAction::Skipped { reason, source } => {
            if reason.contains("failed age check") {
                return ItemOutcome::error(
                    Manager::Brew,
                    item.name.clone(),
                    item.installed.clone(),
                    item.target.clone(),
                    source.as_str(),
                    REASON_COMMAND_FAILED,
                    reason.clone(),
                );
            }

            let reason_code = if reason.starts_with("pinned") {
                REASON_PINNED
            } else {
                REASON_MISSING_METADATA
            };

            ItemOutcome::skipped(
                Manager::Brew,
                item.name.clone(),
                item.installed.clone(),
                item.target.clone(),
                source.as_str(),
                reason_code,
                reason.clone(),
            )
        }
    }
}

fn action_from_commit_age(
    min_age: Duration,
    now_unix_secs: u64,
    committed_at: u64,
    source: DataSource,
) -> PlanAction {
    let age_secs = now_unix_secs.saturating_sub(committed_at);

    if age_secs >= min_age.as_secs() {
        return PlanAction::Upgrade { source };
    }

    let age = human_age(age_secs);
    let required = human_age(min_age.as_secs());
    PlanAction::Delayed {
        age,
        required,
        source,
    }
}

fn git_last_commit_unix_seconds(
    repo_path: &str,
    branch: Option<&str>,
    source_path: &str,
) -> Result<u64> {
    // Some Homebrew taps can have an invalid local HEAD (e.g. refs/heads/.invalid),
    // so implicit HEAD-based commands fail even though origin/* and FETCH_HEAD are valid.
    // `brew update-reset` usually repairs tap refs if needed.
    let mut refs: Vec<String> = Vec::new();

    let mut push_unique = |git_ref: String| {
        if !refs.iter().any(|existing| existing == &git_ref) {
            refs.push(git_ref);
        }
    };

    if let Some(branch) = branch.filter(|b| !b.is_empty()) {
        push_unique(format!("origin/{branch}"));
    }

    push_unique("origin/HEAD".to_string());
    push_unique("FETCH_HEAD".to_string());
    push_unique("HEAD".to_string());

    let mut last_err = String::new();
    for git_ref in refs {
        match git_log_timestamp_for_ref(repo_path, source_path, &git_ref) {
            Ok(ts) => return Ok(ts),
            Err(err) => last_err = format!("{git_ref}: {err}"),
        }
    }

    bail!("git log failed for all refs ({last_err})")
}

fn git_log_timestamp_for_ref(repo_path: &str, source_path: &str, git_ref: &str) -> Result<u64> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["log", "-1", "--format=%ct", git_ref, "--", source_path])
        .output()
        .with_context(|| format!("failed running git log {git_ref} for {source_path}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout).context("git log output was not UTF-8")?;
    let ts = stdout
        .trim()
        .parse::<u64>()
        .with_context(|| format!("invalid git timestamp for {source_path}"))?;
    Ok(ts)
}

fn github_last_commit_unix_seconds(
    client: &Client,
    remote: &str,
    branch: Option<&str>,
    source_path: &str,
) -> Result<u64> {
    let (owner, repo) = parse_github_remote(remote)
        .with_context(|| format!("unsupported non-GitHub remote '{remote}'"))?;

    let mut url = reqwest::Url::parse(&format!(
        "https://api.github.com/repos/{owner}/{repo}/commits"
    ))
    .context("failed to build GitHub commits URL")?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("path", source_path);
        if let Some(branch) = branch.filter(|b| !b.is_empty()) {
            q.append_pair("sha", branch);
        }
        q.append_pair("per_page", "1");
    }

    let commits: Vec<GitHubCommitItem> = client
        .get(url)
        .send()
        .context("GitHub API request failed")?
        .error_for_status()
        .context("GitHub API returned an error status")?
        .json()
        .context("failed to parse GitHub API response")?;

    let first = commits
        .first()
        .context("GitHub API returned no commits for this file")?;

    let date = first
        .commit
        .committer
        .as_ref()
        .map(|p| p.date.as_str())
        .or_else(|| first.commit.author.as_ref().map(|p| p.date.as_str()))
        .context("GitHub commit payload missing date")?;

    let dt = chrono::DateTime::parse_from_rfc3339(date)
        .with_context(|| format!("invalid RFC3339 date from GitHub: {date}"))?;

    u64::try_from(dt.timestamp()).context("GitHub commit timestamp is negative")
}

fn parse_github_remote(remote: &str) -> Option<(String, String)> {
    let rest = if let Some(r) = remote.strip_prefix("https://github.com/") {
        r
    } else if let Some(r) = remote.strip_prefix("http://github.com/") {
        r
    } else if let Some(r) = remote.strip_prefix("git@github.com:") {
        r
    } else if let Some(r) = remote.strip_prefix("ssh://git@github.com/") {
        r
    } else {
        return None;
    };

    let cleaned = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = cleaned.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    Some((owner, repo))
}

fn resolve_api_fallback_remote_branch(
    tap: &str,
    tap_meta: Option<&TapMeta>,
) -> Option<(String, Option<String>)> {
    if let Some(meta) = tap_meta {
        if let Some(remote) = meta.remote.clone() {
            return Some((remote, meta.branch.clone()));
        }
    }

    match tap {
        "homebrew/core" => Some((
            "https://github.com/Homebrew/homebrew-core".to_string(),
            Some("main".to_string()),
        )),
        "homebrew/cask" => Some((
            "https://github.com/Homebrew/homebrew-cask".to_string(),
            Some("main".to_string()),
        )),
        _ => None,
    }
}

fn brew_tap_meta() -> Result<HashMap<String, TapMeta>> {
    let taps: Vec<TapInfo> = brew_json(&["tap-info", "--json", "--installed"])?;
    Ok(taps
        .into_iter()
        .map(|t| {
            (
                t.name,
                TapMeta {
                    path: t.path,
                    remote: t.remote,
                    branch: t.branch,
                },
            )
        })
        .collect())
}

fn brew_info_for_names(formula_names: &[String], cask_names: &[String]) -> Result<InfoRoot> {
    if formula_names.is_empty() && cask_names.is_empty() {
        return Ok(InfoRoot {
            formulae: Vec::new(),
            casks: Vec::new(),
        });
    }

    let mut args = vec!["info".to_string(), "--json=v2".to_string()];
    args.extend(formula_names.iter().cloned());
    args.extend(cask_names.iter().cloned());

    brew_json_owned(&args)
}

fn github_client() -> Result<Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("upnow/0.1"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );

    let token = std::env::var("HOMEBREW_GITHUB_API_TOKEN")
        .ok()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .or_else(|| std::env::var("GH_TOKEN").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if let Some(token) = token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .context("invalid GitHub token value in env")?;
        headers.insert(AUTHORIZATION, value);
    }

    Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(8))
        .build()
        .context("failed to build HTTP client")
}

fn brew_json<T>(args: &[&str]) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let stdout = run_brew(args)?;
    serde_json::from_slice(&stdout).with_context(|| {
        format!(
            "failed to parse brew JSON output for args: {}",
            args.join(" ")
        )
    })
}

fn brew_json_owned<T>(args: &[String]) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let stdout = run_brew_owned(args)?;
    serde_json::from_slice(&stdout).with_context(|| {
        format!(
            "failed to parse brew JSON output for args: {}",
            args.join(" ")
        )
    })
}

fn run_brew(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("brew");
    command.args(args);
    run_command_checked_stdout(command)
}

fn run_brew_owned(args: &[String]) -> Result<Vec<u8>> {
    let mut command = Command::new("brew");
    command.args(args);
    run_command_checked_stdout(command)
}

fn parse_duration(raw: &str) -> Result<Duration> {
    let trimmed = raw.trim();
    if trimmed.len() < 2 {
        bail!("invalid duration '{raw}', expected values like 12h or 7d");
    }

    let (number, unit) = trimmed.split_at(trimmed.len() - 1);
    let value = number
        .parse::<u64>()
        .with_context(|| format!("invalid duration number in '{raw}'"))?;

    let secs = match unit {
        "s" => value,
        "m" => value.saturating_mul(60),
        "h" => value.saturating_mul(60 * 60),
        "d" => value.saturating_mul(24 * 60 * 60),
        _ => bail!("invalid duration unit '{unit}', expected one of: s, m, h, d"),
    };

    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_hours() {
        let d = parse_duration("12h").expect("duration should parse");
        assert_eq!(d.as_secs(), 12 * 3600);
    }

    #[test]
    fn parse_duration_days() {
        let d = parse_duration("7d").expect("duration should parse");
        assert_eq!(d.as_secs(), 7 * 24 * 3600);
    }

    #[test]
    fn human_age_format() {
        assert_eq!(human_age(59), "59s");
        assert_eq!(human_age(61), "1m");
        assert_eq!(human_age(3600), "1h");
        assert_eq!(human_age(3660), "1h1m");
        assert_eq!(human_age(24 * 3600), "1d");
        assert_eq!(human_age(25 * 3600), "1d1h");
    }

    #[test]
    fn parse_github_https_remote() {
        let parsed = parse_github_remote("https://github.com/Homebrew/homebrew-core.git")
            .expect("should parse");
        assert_eq!(parsed.0, "Homebrew");
        assert_eq!(parsed.1, "homebrew-core");
    }

    #[test]
    fn parse_github_ssh_remote() {
        let parsed =
            parse_github_remote("git@github.com:Homebrew/homebrew-cask.git").expect("should parse");
        assert_eq!(parsed.0, "Homebrew");
        assert_eq!(parsed.1, "homebrew-cask");
    }

    #[test]
    fn fallback_remote_for_homebrew_cask() {
        let (remote, branch) =
            resolve_api_fallback_remote_branch("homebrew/cask", None).expect("fallback expected");
        assert_eq!(remote, "https://github.com/Homebrew/homebrew-cask");
        assert_eq!(branch.as_deref(), Some("main"));
    }

    #[test]
    fn fallback_remote_prefers_tap_meta_remote() {
        let meta = TapMeta {
            path: "/tmp/does-not-matter".to_string(),
            remote: Some("https://github.com/acme/custom-tap".to_string()),
            branch: Some("master".to_string()),
        };

        let (remote, branch) = resolve_api_fallback_remote_branch("homebrew/cask", Some(&meta))
            .expect("fallback expected");
        assert_eq!(remote, "https://github.com/acme/custom-tap");
        assert_eq!(branch.as_deref(), Some("master"));
    }
}
