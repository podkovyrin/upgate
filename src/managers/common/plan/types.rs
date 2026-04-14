use crate::outcome::ItemOutcome;
use crate::util::time::human_age;
use std::time::Duration;

pub struct PlanMeta {
    pub manager: &'static str,
    pub source: &'static str,
    pub name: String,
    pub current: String,
}

#[derive(Debug, Clone)]
pub struct DelayedLatest {
    pub latest_version: String,
    pub latest_age: String,
    pub required_age: String,
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
    NoChange,
    Update {
        target: String,
        delayed_latest: Option<DelayedLatest>,
    },
}

#[derive(Debug, Clone)]
pub struct PlannedUpdate {
    pub manager: &'static str,
    pub source: &'static str,
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
