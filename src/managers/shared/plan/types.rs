use std::time::Duration;

use crate::managers::shared::versioning::{Pep440AgeResolution, SemverAgeResolution};
use crate::outcome::ItemOutcome;
use crate::util::time::human_age;

pub struct PlanMeta {
    pub manager: &'static str,
    pub name: String,
    pub current: String,
}

#[derive(Debug, Clone)]
pub struct DelayedLatest {
    pub latest_version: String,
    pub latest_age: String,
    pub required_age: String,
}

#[derive(Debug, Clone)]
pub struct AgeResolvedTarget {
    pub selected_version: Option<String>,
    pub latest_version: Option<String>,
    pub latest_age_secs: Option<u64>,
    pub current_blocked_by_policy: bool,
}

impl AgeResolvedTarget {
    pub const fn new(
        selected_version: Option<String>,
        latest_version: Option<String>,
        latest_age_secs: Option<u64>,
    ) -> Self {
        Self::new_with_policy_flag(selected_version, latest_version, latest_age_secs, false)
    }

    pub const fn new_with_policy_flag(
        selected_version: Option<String>,
        latest_version: Option<String>,
        latest_age_secs: Option<u64>,
        current_blocked_by_policy: bool,
    ) -> Self {
        Self {
            selected_version,
            latest_version,
            latest_age_secs,
            current_blocked_by_policy,
        }
    }

    pub fn delayed_latest(&self, min_age: Duration) -> Option<DelayedLatest> {
        DelayedLatest::from_too_fresh_latest(
            self.selected_version.as_deref(),
            self.latest_version.as_deref(),
            self.latest_age_secs,
            min_age,
        )
    }
}

impl From<SemverAgeResolution> for AgeResolvedTarget {
    fn from(value: SemverAgeResolution) -> Self {
        Self::new_with_policy_flag(
            value.selected_version,
            value.latest_version,
            value.latest_age_secs,
            value.current_blocked_by_policy,
        )
    }
}

impl From<Pep440AgeResolution> for AgeResolvedTarget {
    fn from(value: Pep440AgeResolution) -> Self {
        Self::new_with_policy_flag(
            value.selected_version,
            value.latest_version,
            value.latest_age_secs,
            value.current_blocked_by_policy,
        )
    }
}

impl DelayedLatest {
    pub fn new(latest_version: impl Into<String>, latest_age_secs: u64, min_age: Duration) -> Self {
        Self {
            latest_version: latest_version.into(),
            latest_age: human_age(latest_age_secs),
            required_age: human_age(min_age.as_secs()),
        }
    }

    pub fn from_too_fresh_latest(
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

pub enum PlanDecision {
    Error(String),
    DelayedNoEligible {
        required_age: String,
        delayed_latest: Option<DelayedLatest>,
    },
    CurrentBlockedByPolicy,
    NoChange,
    Update {
        target: String,
        delayed_latest: Option<DelayedLatest>,
    },
}

#[derive(Debug, Clone)]
pub struct PlannedUpdate {
    pub manager: &'static str,
    pub name: String,
    pub current: String,
    pub target: String,
    pub delayed_latest: Option<DelayedLatest>,
    pub apply_spec_base: Option<String>,
}

impl PlannedUpdate {
    pub fn to_update_outcome(&self) -> ItemOutcome {
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
        )
    }
}
