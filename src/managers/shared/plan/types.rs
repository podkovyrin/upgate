use std::time::Duration;

use crate::managers::shared::versioning::policy::{PolicyWarning, VersionPolicy};
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

impl DelayedLatest {
    pub fn new(latest_version: impl Into<String>, latest_age_secs: u64, min_age: Duration) -> Self {
        Self {
            latest_version: latest_version.into(),
            latest_age: human_age(latest_age_secs),
            required_age: human_age(min_age.as_secs()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct VersionPolicyMeta {
    pub policy: VersionPolicy,
    pub latest_blocked_version: Option<String>,
    pub warning: Option<PolicyWarning>,
}

impl VersionPolicyMeta {
    pub fn apply_to_outcome(&self, outcome: &mut ItemOutcome) {
        if self.policy != VersionPolicy::Disabled {
            outcome.version_policy = Some(self.policy.as_str().to_string());
        }
        outcome
            .latest_blocked_by_policy_version
            .clone_from(&self.latest_blocked_version);
        outcome.version_policy_warning =
            self.warning.map(PolicyWarning::as_note).map(str::to_string);
    }
}

pub enum PlanDecision {
    Error(String),
    DelayedNoEligible {
        required_age: String,
        delayed_latest: Option<DelayedLatest>,
        version_policy: Option<VersionPolicyMeta>,
    },
    CurrentBlockedByPolicy {
        version_policy: VersionPolicyMeta,
    },
    NoChange,
    Update {
        target: String,
        delayed_latest: Option<DelayedLatest>,
        version_policy: Option<VersionPolicyMeta>,
    },
}

#[derive(Debug, Clone)]
pub struct PlannedUpdate {
    pub manager: &'static str,
    pub name: String,
    pub current: String,
    pub target: String,
    pub delayed_latest: Option<DelayedLatest>,
    pub version_policy: Option<VersionPolicyMeta>,
    pub apply_spec_base: Option<String>,
}

impl PlannedUpdate {
    pub fn to_update_outcome(&self) -> ItemOutcome {
        let mut outcome = if let Some(DelayedLatest {
            latest_version,
            latest_age,
            required_age,
        }) = &self.delayed_latest
        {
            ItemOutcome::update_with_delayed_latest(
                self.manager,
                self.name.clone(),
                self.current.clone(),
                self.target.clone(),
                latest_version.clone(),
                latest_age.clone(),
                required_age.clone(),
            )
        } else {
            ItemOutcome::update(
                self.manager,
                self.name.clone(),
                self.current.clone(),
                self.target.clone(),
            )
        };

        if let Some(policy) = &self.version_policy {
            policy.apply_to_outcome(&mut outcome);
        }

        outcome
    }
}
