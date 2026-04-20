use std::time::Duration;

use anyhow::Result;

use crate::outcome::{ItemOutcome, ReasonCode, emit_text_outcome};
use crate::util::time::{human_age, now_unix_secs};

pub fn emit_manager_level_error(manager: &'static str, detail: impl AsRef<str>) {
    let outcome = ItemOutcome::error(
        manager,
        "*",
        "*",
        "*",
        ReasonCode::CommandFailed,
        format!("manager-level fallback: {}", detail.as_ref()),
    );
    emit_text_outcome(&outcome);
}

pub fn emit_manager_level_error_with(
    manager: &'static str,
    context: impl AsRef<str>,
    err: impl std::fmt::Display,
) {
    emit_manager_level_error(manager, format!("{}: {err}", context.as_ref()));
}

pub fn soft_fail<T, E>(
    result: std::result::Result<T, E>,
    manager: &'static str,
    context: impl AsRef<str>,
) -> Option<T>
where
    E: std::fmt::Display,
{
    match result {
        Ok(value) => Some(value),
        Err(err) => {
            emit_manager_level_error_with(manager, context, err);
            None
        }
    }
}

pub fn soft_fail_or<T, E, F>(
    result: std::result::Result<T, E>,
    fallback: F,
    manager: &'static str,
    context: impl AsRef<str>,
) -> T
where
    E: std::fmt::Display,
    F: FnOnce() -> T,
{
    match result {
        Ok(value) => value,
        Err(err) => {
            emit_manager_level_error_with(manager, context, err);
            fallback()
        }
    }
}

pub fn emit_scan_current(
    manager: &'static str,
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
            human_age(age_secs),
            age_secs >= old_threshold.as_secs(),
        )
    } else {
        ItemOutcome::current(manager, name, version)
    };

    emit_text_outcome(&outcome);
}

pub fn verbose_now_unix_secs() -> Result<Option<u64>> {
    crate::ui::output_theme()
        .verbose
        .then(now_unix_secs)
        .transpose()
}

pub fn emit_version_scan_outcomes<I, F>(
    manager: &'static str,
    items: I,
    now_unix_secs: Option<u64>,
    old_threshold: Duration,
    mut release_age_secs: F,
) where
    I: IntoIterator<Item = (String, String)>,
    F: FnMut(&str, &str, u64) -> Result<Option<u64>>,
{
    for (name, version) in items {
        let age_secs = now_unix_secs.and_then(|now_unix_secs| {
            match release_age_secs(&name, &version, now_unix_secs) {
                Ok(age_secs) => age_secs,
                Err(err) => {
                    crate::util::logging::log_warning(format!(
                        "[{manager}] failed to resolve release age for {name}@{version}: {err}"
                    ));
                    None
                }
            }
        });

        emit_scan_current(manager, name, version, age_secs, old_threshold);
    }
}
