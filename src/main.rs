use anyhow::{Context, Result, bail};
use clap::Parser;
use rayon::prelude::*;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Parser)]
#[command(name = "brew-delay-upgrade")]
#[command(about = "Upgrade Homebrew packages older than a minimum release age")]
struct Cli {
    /// Print the upgrade plan only.
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Minimum age of a formula/cask definition commit (e.g. 12h, 7d).
    #[arg(long, default_value = "12h")]
    min_release_age: String,

    /// Maximum concurrent age checks (git/API), to avoid API overloading.
    #[arg(long, default_value_t = 6)]
    max_parallel_checks: usize,
}

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

struct CommitAgeInfo {
    committed_at: u64,
    source: DataSource,
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

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

#[allow(clippy::too_many_lines)]
fn run() -> Result<()> {
    let cli = Cli::parse();
    let min_age = parse_duration(&cli.min_release_age)?;

    run_brew(&["update", "--quiet"])?;

    let outdated: OutdatedRoot = brew_json(&["outdated", "--json=v2"])?;
    if outdated.formulae.is_empty() && outdated.casks.is_empty() {
        return Ok(());
    }

    let formula_names: Vec<String> = outdated.formulae.iter().map(|f| f.name.clone()).collect();
    let cask_names: Vec<String> = outdated.casks.iter().map(|c| c.name.clone()).collect();

    let formula_infos = brew_formula_info(&formula_names)?;
    let cask_infos = brew_cask_info(&cask_names)?;
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
    for info in formula_infos {
        formula_info_by_name.insert(info.full_name.clone(), info);
    }

    let mut cask_info_by_name: HashMap<String, CaskInfo> = HashMap::new();
    for info in cask_infos {
        cask_info_by_name.insert(info.token.clone(), info);
    }

    let formula_plan: Vec<PlanItem> = pool.install(|| {
        outdated
            .formulae
            .into_par_iter()
            .map(|item| {
                let installed = item
                    .installed_versions
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());

                let action = if item.pinned {
                    PlanAction::Skipped {
                        reason: match item.pinned_version {
                            Some(v) => format!("pinned at {v}"),
                            None => "pinned".to_string(),
                        },
                        source: DataSource::None,
                    }
                } else {
                    let tap_and_source = formula_info_by_name
                        .get(&item.name)
                        .map(|f| (f.tap.as_deref(), f.ruby_source_path.as_deref()));

                    decide_by_age(min_age, now, tap_and_source, &tap_meta, &github_client)
                };

                PlanItem {
                    name: item.name,
                    installed,
                    target: item.current_version,
                    action,
                    is_formula: true,
                }
            })
            .collect()
    });

    let cask_plan: Vec<PlanItem> = pool.install(|| {
        outdated
            .casks
            .into_par_iter()
            .map(|item| {
                let installed = item
                    .installed_versions
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());

                let tap_and_source = cask_info_by_name
                    .get(&item.name)
                    .map(|c| (c.tap.as_deref(), c.ruby_source_path.as_deref()));

                let action = decide_by_age(min_age, now, tap_and_source, &tap_meta, &github_client);

                PlanItem {
                    name: item.name,
                    installed,
                    target: item.current_version,
                    action,
                    is_formula: false,
                }
            })
            .collect()
    });

    let mut plan = Vec::with_capacity(formula_plan.len() + cask_plan.len());
    plan.extend(formula_plan);
    plan.extend(cask_plan);

    for item in &plan {
        let installed = version_label(&item.installed);
        let target = version_label(&item.target);

        match &item.action {
            PlanAction::Upgrade { source } => {
                println!(
                    "brew: {} {} -> {} (source: {})",
                    item.name,
                    installed,
                    target,
                    source.as_str()
                );
            }
            PlanAction::Delayed {
                age,
                required,
                source,
            } => {
                println!(
                    "brew: {} {} -> {} (delayed, {} < {}, source: {})",
                    item.name,
                    installed,
                    target,
                    age,
                    required,
                    source.as_str()
                );
            }
            PlanAction::Skipped { reason, source } => {
                println!(
                    "brew: {} {} -> {} (skipped, {}, source: {})",
                    item.name,
                    installed,
                    target,
                    reason,
                    source.as_str()
                );
            }
        }
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

fn decide_by_age(
    min_age: Duration,
    now_unix_secs: u64,
    tap_and_source: Option<(Option<&str>, Option<&str>)>,
    tap_meta: &HashMap<String, TapMeta>,
    github_client: &Client,
) -> PlanAction {
    let Some((tap, source_path)) = tap_and_source else {
        return PlanAction::Skipped {
            reason: "unable to resolve package metadata from brew info".to_string(),
            source: DataSource::None,
        };
    };

    let Some(tap) = tap else {
        return PlanAction::Skipped {
            reason: "missing tap".to_string(),
            source: DataSource::None,
        };
    };

    let Some(source_path) = source_path else {
        return PlanAction::Skipped {
            reason: "missing ruby_source_path".to_string(),
            source: DataSource::None,
        };
    };

    let Some(tap_meta) = tap_meta.get(tap) else {
        return PlanAction::Skipped {
            reason: format!("tap '{tap}' is not installed locally"),
            source: DataSource::None,
        };
    };

    let commit_info = match commit_unix_seconds(tap_meta, source_path, github_client) {
        Ok(info) => info,
        Err(err) => {
            return PlanAction::Skipped {
                reason: format!("failed age check: {err}"),
                source: DataSource::None,
            };
        }
    };

    let age_secs = now_unix_secs.saturating_sub(commit_info.committed_at);

    if age_secs >= min_age.as_secs() {
        return PlanAction::Upgrade {
            source: commit_info.source,
        };
    }

    let age = human_age(age_secs);
    let required = human_age(min_age.as_secs());
    PlanAction::Delayed {
        age,
        required,
        source: commit_info.source,
    }
}

fn commit_unix_seconds(
    tap_meta: &TapMeta,
    source_path: &str,
    github_client: &Client,
) -> Result<CommitAgeInfo> {
    match git_last_commit_unix_seconds(&tap_meta.path, tap_meta.branch.as_deref(), source_path) {
        Ok(ts) => Ok(CommitAgeInfo {
            committed_at: ts,
            source: DataSource::Git,
        }),
        Err(local_err) => {
            if let (Some(remote), Some(branch)) = (&tap_meta.remote, &tap_meta.branch) {
                match github_last_commit_unix_seconds(github_client, remote, branch, source_path) {
                    Ok(ts) => Ok(CommitAgeInfo {
                        committed_at: ts,
                        source: DataSource::Api,
                    }),
                    Err(github_err) => {
                        bail!(
                            "local git failed ({local_err}); GitHub fallback failed ({github_err})"
                        )
                    }
                }
            } else {
                bail!("local git failed ({local_err}) and no remote/branch fallback available")
            }
        }
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
    branch: &str,
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
        q.append_pair("sha", branch);
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

fn brew_formula_info(names: &[String]) -> Result<Vec<FormulaInfo>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }

    let mut args = vec![
        "info".to_string(),
        "--json=v2".to_string(),
        "--formula".to_string(),
    ];
    args.extend(names.iter().cloned());

    let root: InfoRoot = brew_json_owned(&args)?;
    Ok(root.formulae)
}

fn brew_cask_info(names: &[String]) -> Result<Vec<CaskInfo>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }

    let mut args = vec![
        "info".to_string(),
        "--json=v2".to_string(),
        "--cask".to_string(),
    ];
    args.extend(names.iter().cloned());

    let root: InfoRoot = brew_json_owned(&args)?;
    Ok(root.casks)
}

fn github_client() -> Result<Client> {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("brew-delay-upgrade/0.1"),
    );
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
    let output = Command::new("brew")
        .args(args)
        .output()
        .with_context(|| format!("failed to run brew {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("brew {} failed: {}", args.join(" "), stderr.trim());
    }

    Ok(output.stdout)
}

fn run_brew_owned(args: &[String]) -> Result<Vec<u8>> {
    let output = Command::new("brew")
        .args(args)
        .output()
        .with_context(|| format!("failed to run brew {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("brew {} failed: {}", args.join(" "), stderr.trim());
    }

    Ok(output.stdout)
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

fn version_label(version: &str) -> String {
    if version.starts_with('v') {
        return version.to_string();
    }

    match version.chars().next() {
        Some(c) if c.is_ascii_digit() => format!("v{version}"),
        _ => version.to_string(),
    }
}

fn human_age(total_secs: u64) -> String {
    if total_secs < 60 {
        return format!("{total_secs}s");
    }

    if total_secs < 60 * 60 {
        return format!("{}m", total_secs / 60);
    }

    if total_secs < 24 * 60 * 60 {
        let hours = total_secs / 3600;
        let minutes = (total_secs % 3600) / 60;
        return if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{minutes}m")
        };
    }

    let days = total_secs / (24 * 60 * 60);
    let hours = (total_secs % (24 * 60 * 60)) / 3600;
    if hours == 0 {
        format!("{days}d")
    } else {
        format!("{days}d{hours}h")
    }
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
}
