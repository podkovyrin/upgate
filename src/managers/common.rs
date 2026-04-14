use crate::config::PIN_ALL;
use crate::manager::ManagerCtx;
use crate::outcome::{ItemOutcome, REASON_COMMAND_FAILED, REASON_PINNED, emit_text_outcome};
use crate::util::time::now_unix_secs;
use crate::util::timefmt::human_age;
use crate::util::timeparse::parse_rfc3339_unix;
use anyhow::{Context, Result};
use pep440::Version as Pep440Version;
use semver::Version;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

pub(crate) struct PlanMeta {
    pub(crate) manager: &'static str,
    pub(crate) source: &'static str,
    pub(crate) name: String,
    pub(crate) current: String,
}

#[derive(Debug, Clone)]
pub(crate) struct DelayedLatest {
    pub(crate) latest_version: String,
    pub(crate) latest_age: String,
    pub(crate) required_age: String,
}

impl DelayedLatest {
    pub(crate) fn new(
        latest_version: impl Into<String>,
        latest_age_secs: u64,
        min_age: Duration,
    ) -> Self {
        Self {
            latest_version: latest_version.into(),
            latest_age: human_age(latest_age_secs),
            required_age: human_age(min_age.as_secs()),
        }
    }

    pub(crate) fn from_too_fresh_latest(
        selected_version: Option<&str>,
        latest_version: Option<&str>,
        latest_age_secs: Option<u64>,
        min_age: Duration,
    ) -> Option<Self> {
        let latest_version = latest_version?;
        let latest_age_secs = latest_age_secs?;

        if latest_age_secs >= min_age.as_secs() || selected_version == Some(latest_version) {
            return None;
        }

        Some(Self::new(latest_version, latest_age_secs, min_age))
    }
}

pub(crate) enum PlanDecision {
    Error(String),
    DelayedNoEligible {
        required_age: String,
        delayed_latest: Option<DelayedLatest>,
    },
    NoChange,
    Update {
        target: String,
        delayed_latest: Option<DelayedLatest>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct PlannedUpdate {
    pub(crate) manager: &'static str,
    pub(crate) source: &'static str,
    pub(crate) name: String,
    pub(crate) current: String,
    pub(crate) target: String,
    pub(crate) delayed_latest: Option<DelayedLatest>,
    pub(crate) apply_spec_base: Option<String>,
}

impl PlannedUpdate {
    pub(crate) fn to_update_outcome(&self) -> ItemOutcome {
        if let Some(DelayedLatest {
            latest_version,
            latest_age,
            required_age,
        }) = &self.delayed_latest
        {
            return ItemOutcome::update_with_delayed_latest(
                self.manager,
                self.name.clone(),
                self.current.clone(),
                self.target.clone(),
                self.source,
                latest_version.clone(),
                latest_age.clone(),
                required_age.clone(),
            );
        }

        ItemOutcome::update(
            self.manager,
            self.name.clone(),
            self.current.clone(),
            self.target.clone(),
            self.source,
        )
    }
}

pub(crate) fn emit_manager_level_error(
    manager: &'static str,
    source: &'static str,
    detail: impl AsRef<str>,
) {
    let outcome = ItemOutcome::error(
        manager,
        "*",
        "*",
        "*",
        source,
        REASON_COMMAND_FAILED,
        format!("manager-level fallback: {}", detail.as_ref()),
    );
    emit_text_outcome(&outcome);
}

pub(crate) fn emit_scan_current(
    manager: &'static str,
    source: &'static str,
    name: impl Into<String>,
    version: impl Into<String>,
    age_secs: Option<u64>,
    old_threshold: Duration,
) {
    let name = name.into();
    let version = version.into();
    let outcome = if let Some(age_secs) = age_secs {
        ItemOutcome::current_with_age(
            manager,
            name,
            version,
            source,
            human_age(age_secs),
            age_secs >= old_threshold.as_secs(),
        )
    } else {
        ItemOutcome::current(manager, name, version, source)
    };

    emit_text_outcome(&outcome);
}

pub(crate) fn verbose_now_unix_secs() -> Result<Option<u64>> {
    crate::ui::output_theme()
        .verbose
        .then(now_unix_secs)
        .transpose()
}

pub(crate) fn emit_version_scan_outcomes<I, F>(
    manager: &'static str,
    source: &'static str,
    items: I,
    now_unix_secs: Option<u64>,
    old_threshold: Duration,
    mut release_age_secs: F,
) where
    I: IntoIterator<Item = (String, String)>,
    F: FnMut(&str, &str, u64) -> Result<Option<u64>>,
{
    for (name, version) in items {
        let age_secs = now_unix_secs
            .and_then(|now_unix_secs| release_age_secs(&name, &version, now_unix_secs).ok())
            .flatten();

        emit_scan_current(manager, source, name, version, age_secs, old_threshold);
    }
}

pub(crate) fn emit_plan_and_collect_upgradable<T, M, D>(
    items: Vec<T>,
    mut meta_fn: M,
    mut decision_fn: D,
    suppress_update_outcomes: bool,
    pinned: Option<&BTreeSet<String>>,
) -> Vec<PlannedUpdate>
where
    M: FnMut(&T) -> PlanMeta,
    D: FnMut(&T) -> PlanDecision,
{
    let mut upgradable = Vec::new();

    for item in items {
        let PlanMeta {
            manager,
            source,
            name,
            current,
        } = meta_fn(&item);

        let decision = decision_fn(&item);

        if is_pinned(manager, &name, pinned) {
            handle_pinned_decision(
                &mut upgradable,
                manager,
                source,
                name,
                current,
                decision,
                suppress_update_outcomes,
            );
            continue;
        }

        handle_regular_decision(
            &mut upgradable,
            manager,
            source,
            name,
            current,
            decision,
            suppress_update_outcomes,
        );
    }

    upgradable
}

pub(crate) fn run_per_item_apply_flow<F>(
    ctx: &ManagerCtx,
    manager_id: &'static str,
    upgradable: Vec<PlannedUpdate>,
    apply_selected: F,
) -> Result<()>
where
    F: FnOnce(Vec<PlannedUpdate>),
{
    let selected =
        crate::util::interactive_apply::select_upgradable_items(ctx, manager_id, upgradable)?;
    if selected.is_empty() || ctx.is_dry_run() {
        return Ok(());
    }

    apply_selected(selected);
    Ok(())
}

pub(crate) fn run_global_apply_flow<F, E>(
    ctx: &ManagerCtx,
    manager_id: &'static str,
    source: &'static str,
    upgradable: Vec<PlannedUpdate>,
    apply_all: F,
) -> Result<()>
where
    F: FnOnce() -> std::result::Result<(), E>,
    E: std::fmt::Display,
{
    if upgradable.is_empty() || ctx.is_dry_run() {
        return Ok(());
    }

    if ctx.is_interactive_apply() {
        if !crate::util::interactive_apply::should_apply_global_manager(
            ctx,
            manager_id,
            &upgradable,
        )? {
            return Ok(());
        }

        for item in &upgradable {
            emit_text_outcome(&item.to_update_outcome());
        }
    }

    if let Err(err) = apply_all() {
        let outcome = ItemOutcome::error(
            manager_id,
            "*",
            "*",
            "*",
            source,
            REASON_COMMAND_FAILED,
            err.to_string(),
        );
        emit_text_outcome(&outcome);
    }

    Ok(())
}

fn is_pinned(manager: &'static str, name: &str, pinned: Option<&BTreeSet<String>>) -> bool {
    pinned.is_some_and(|set| {
        (matches!(manager, "npm" | "bun" | "mise") && !set.is_empty())
            || set.contains(name)
            || set.contains(PIN_ALL)
    })
}

fn handle_pinned_decision(
    upgradable: &mut Vec<PlannedUpdate>,
    manager: &'static str,
    source: &'static str,
    name: String,
    current: String,
    decision: PlanDecision,
    suppress_update_outcomes: bool,
) {
    if suppress_update_outcomes {
        if let PlanDecision::Update {
            target,
            delayed_latest,
        } = decision
        {
            upgradable.push(PlannedUpdate {
                manager,
                source,
                name,
                current,
                target,
                delayed_latest,
                apply_spec_base: None,
            });
        }
        return;
    }

    let outcome = ItemOutcome::skipped(
        manager,
        name,
        current.clone(),
        current,
        source,
        REASON_PINNED,
        "pinned",
    );
    emit_text_outcome(&outcome);
}

fn handle_regular_decision(
    upgradable: &mut Vec<PlannedUpdate>,
    manager: &'static str,
    source: &'static str,
    name: String,
    current: String,
    decision: PlanDecision,
    suppress_update_outcomes: bool,
) {
    match decision {
        PlanDecision::Error(err) => {
            let outcome = ItemOutcome::error(
                manager,
                name,
                current.clone(),
                current,
                source,
                REASON_COMMAND_FAILED,
                err,
            );
            emit_text_outcome(&outcome);
        }
        PlanDecision::DelayedNoEligible {
            required_age,
            delayed_latest,
        } => {
            let outcome =
                delayed_outcome(manager, source, name, current, required_age, delayed_latest);
            emit_text_outcome(&outcome);
        }
        PlanDecision::NoChange => {
            let outcome = ItemOutcome::skipped_no_change(manager, name, current, source);
            emit_text_outcome(&outcome);
        }
        PlanDecision::Update {
            target,
            delayed_latest,
        } => {
            let planned = PlannedUpdate {
                manager,
                source,
                name,
                current,
                target,
                delayed_latest,
                apply_spec_base: None,
            };
            if !suppress_update_outcomes {
                emit_text_outcome(&planned.to_update_outcome());
            }
            upgradable.push(planned);
        }
    }
}

fn delayed_outcome(
    manager: &'static str,
    source: &'static str,
    name: String,
    current: String,
    required_age: String,
    delayed_latest: Option<DelayedLatest>,
) -> ItemOutcome {
    if let Some(DelayedLatest {
        latest_version,
        latest_age,
        required_age,
    }) = delayed_latest
    {
        return ItemOutcome::delayed_no_eligible_with_latest(
            manager,
            name,
            current,
            source,
            latest_version,
            latest_age,
            required_age,
        );
    }

    ItemOutcome::delayed_no_eligible(manager, name, current, source, required_age)
}

#[derive(Debug, Clone)]
pub(crate) struct SemverTimestamp {
    pub(crate) version: String,
    pub(crate) published_unix: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct SemverAgeResolution {
    pub(crate) selected_version: Option<String>,
    pub(crate) latest_version: Option<String>,
    pub(crate) latest_age_secs: Option<u64>,
}

pub(crate) fn resolve_semver_with_min_age(
    current: &str,
    releases: &[SemverTimestamp],
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<SemverAgeResolution> {
    // Semver support is intentionally shared at manager-common level for ecosystem managers
    // that represent release timelines as semver + publish timestamp. Manager-local data fetch/
    // parse remains self-contained.
    let current_ver = Version::parse(current)
        .with_context(|| format!("failed to parse current semver: {current}"))?;

    let mut eligible: Option<(Version, String, u64)> = None;
    let mut newest_any: Option<(Version, String, u64)> = None;

    for item in releases {
        let Ok(version) = Version::parse(&item.version) else {
            continue;
        };

        if newest_any
            .as_ref()
            .is_none_or(|(curr, _, _)| version > *curr)
        {
            newest_any = Some((version.clone(), item.version.clone(), item.published_unix));
        }

        if version >= current_ver {
            let age_secs = now_unix_secs.saturating_sub(item.published_unix);
            if age_secs >= min_age.as_secs()
                && eligible.as_ref().is_none_or(|(curr, _, _)| version > *curr)
            {
                eligible = Some((version, item.version.clone(), item.published_unix));
            }
        }
    }

    let selected_version = eligible.map(|(ver, _, _)| ver.to_string());
    let (latest_version, latest_age_secs) =
        if let Some((_latest_ver, latest_str, latest_ts)) = newest_any {
            (
                Some(latest_str),
                Some(now_unix_secs.saturating_sub(latest_ts)),
            )
        } else {
            (None, None)
        };

    Ok(SemverAgeResolution {
        selected_version,
        latest_version,
        latest_age_secs,
    })
}

pub(crate) fn release_age_secs_for_version(
    releases: &[SemverTimestamp],
    version: &str,
    now_unix_secs: u64,
) -> Option<u64> {
    releases
        .iter()
        .find(|item| item.version == version)
        .map(|item| now_unix_secs.saturating_sub(item.published_unix))
}

pub(crate) fn parse_semver_time_releases(
    source: &str,
    package: &str,
    timestamps_by_version: &BTreeMap<String, String>,
) -> Result<Vec<SemverTimestamp>> {
    let mut releases = Vec::new();

    for (ver_str, ts_raw) in timestamps_by_version {
        if ver_str == "created" || ver_str == "modified" {
            continue;
        }

        let ts = parse_rfc3339_unix(ts_raw).with_context(|| {
            format!("invalid {source} timestamp for {package}@{ver_str}: {ts_raw}")
        })?;

        releases.push(SemverTimestamp {
            version: ver_str.clone(),
            published_unix: ts,
        });
    }

    Ok(releases)
}

#[derive(Debug, Clone)]
pub(crate) struct Pep440Timestamp {
    pub(crate) version: String,
    pub(crate) published_unix: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct Pep440AgeResolution {
    pub(crate) selected_version: Option<String>,
    pub(crate) latest_version: Option<String>,
    pub(crate) latest_age_secs: Option<u64>,
}

pub(crate) fn resolve_pep440_with_min_age(
    current: &str,
    releases: &[Pep440Timestamp],
    now_unix_secs: u64,
    min_age: Duration,
) -> Result<Pep440AgeResolution> {
    let current_ver = Pep440Version::parse(current)
        .with_context(|| format!("failed to parse current PEP440 version: {current}"))?;

    let mut eligible: Option<(Pep440Version, String, u64)> = None;
    let mut newest_any: Option<(Pep440Version, String, u64)> = None;

    for item in releases {
        let Some(version) = Pep440Version::parse(&item.version) else {
            continue;
        };

        if newest_any
            .as_ref()
            .is_none_or(|(curr, _, _)| version > *curr)
        {
            newest_any = Some((version.clone(), item.version.clone(), item.published_unix));
        }

        if version >= current_ver {
            let age_secs = now_unix_secs.saturating_sub(item.published_unix);
            if age_secs >= min_age.as_secs()
                && eligible.as_ref().is_none_or(|(curr, _, _)| version > *curr)
            {
                eligible = Some((version, item.version.clone(), item.published_unix));
            }
        }
    }

    let selected_version = eligible.map(|(_, version, _)| version);
    let (latest_version, latest_age_secs) =
        if let Some((_latest_ver, latest_str, latest_ts)) = newest_any {
            (
                Some(latest_str),
                Some(now_unix_secs.saturating_sub(latest_ts)),
            )
        } else {
            (None, None)
        };

    Ok(Pep440AgeResolution {
        selected_version,
        latest_version,
        latest_age_secs,
    })
}

pub(crate) fn release_age_secs_for_pep440_version(
    releases: &[Pep440Timestamp],
    version: &str,
    now_unix_secs: u64,
) -> Option<u64> {
    releases
        .iter()
        .find(|item| item.version == version)
        .map(|item| now_unix_secs.saturating_sub(item.published_unix))
}

pub(crate) fn parse_pep440_release_timestamps<T, F1, F2>(
    package: &str,
    releases_by_version: &BTreeMap<String, Vec<T>>,
    upload_time_iso_8601: F1,
    upload_time: F2,
) -> Result<Vec<Pep440Timestamp>>
where
    F1: Fn(&T) -> Option<&str>,
    F2: Fn(&T) -> Option<&str>,
{
    let mut releases = Vec::new();

    for (ver_str, files) in releases_by_version {
        let mut newest_ts = None::<u64>;
        for file in files {
            let raw = upload_time_iso_8601(file).or_else(|| upload_time(file));

            if let Some(raw) = raw {
                let ts = parse_rfc3339_unix(raw).with_context(|| {
                    format!("invalid upload timestamp for {package}@{ver_str}: {raw}")
                })?;
                newest_ts = Some(newest_ts.map_or(ts, |curr| curr.max(ts)));
            }
        }

        let Some(published_unix) = newest_ts else {
            continue;
        };

        releases.push(Pep440Timestamp {
            version: ver_str.clone(),
            published_unix,
        });
    }

    Ok(releases)
}
