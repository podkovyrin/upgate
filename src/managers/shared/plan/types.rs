use std::time::Duration;

use crate::managers::shared::versioning::policy::{
    GateBypass, PolicyBlockReason, PolicyWarning, VersionPolicy,
};
use crate::outcome::DelayedReason;
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

    pub fn new_if_fresh(
        latest_version: impl Into<String>,
        latest_age_secs: u64,
        min_age: Duration,
    ) -> Option<Self> {
        if latest_age_secs >= min_age.as_secs() {
            return None;
        }

        Some(Self::new(latest_version, latest_age_secs, min_age))
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
        if self.policy == VersionPolicy::Disabled {
            return;
        }

        outcome.set_version_policy(
            self.policy.as_str(),
            self.latest_blocked_version.clone(),
            self.warning.map(PolicyWarning::as_note).map(str::to_string),
        );
    }
}

pub enum PlanDecision {
    Error(String),
    DelayedNoEligible {
        required_age: String,
        delayed_latest: Option<DelayedLatest>,
        delayed_reason: DelayedReason,
        version_policy: Option<VersionPolicyMeta>,
        force_target: Option<String>,
        candidate_versions: Vec<CandidateVersionMeta>,
    },
    CurrentBlockedByPolicy {
        version_policy: VersionPolicyMeta,
        force_target: Option<String>,
        candidate_versions: Vec<CandidateVersionMeta>,
    },
    NoChange,
    Update {
        target: String,
        delayed_latest: Option<DelayedLatest>,
        version_policy: Option<VersionPolicyMeta>,
        candidate_versions: Vec<CandidateVersionMeta>,
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
    pub gate_bypass: GateBypass,
}

#[derive(Debug, Clone)]
pub struct CandidateVersionMeta {
    pub version: String,
    pub age: String,
    pub policy_allowed: bool,
    pub age_allowed: bool,
    pub policy_block_reason: Option<PolicyBlockReason>,
    pub policy_warning: Option<PolicyWarning>,
}

#[derive(Debug, Clone)]
pub struct ApplyCandidateVersion {
    update: PlannedUpdate,
    note: String,
    force: bool,
}

impl ApplyCandidateVersion {
    pub fn new(update: PlannedUpdate, note: String, force: bool) -> Self {
        Self {
            update,
            note,
            force,
        }
    }

    pub fn update(&self) -> &PlannedUpdate {
        &self.update
    }

    pub fn note(&self) -> &str {
        &self.note
    }

    pub const fn is_force(&self) -> bool {
        self.force
    }

    pub fn clone_update(&self) -> PlannedUpdate {
        self.update.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyCandidateKind {
    Recommended,
    ForceCandidate,
}

impl ApplyCandidateKind {
    pub const fn is_visible_by_default(self) -> bool {
        matches!(self, Self::Recommended)
    }

    pub const fn is_selected_by_default(self) -> bool {
        matches!(self, Self::Recommended)
    }

    pub const fn is_force_candidate(self) -> bool {
        matches!(self, Self::ForceCandidate)
    }
}

#[derive(Debug, Clone)]
pub struct ApplyCandidate {
    update: PlannedUpdate,
    kind: ApplyCandidateKind,
    note: String,
    versions: Vec<ApplyCandidateVersion>,
}

impl ApplyCandidate {
    pub fn recommended(update: PlannedUpdate) -> Self {
        Self {
            update,
            kind: ApplyCandidateKind::Recommended,
            note: String::new(),
            versions: Vec::new(),
        }
    }

    pub fn force_candidate(update: PlannedUpdate) -> Self {
        Self {
            update,
            kind: ApplyCandidateKind::ForceCandidate,
            note: String::new(),
            versions: Vec::new(),
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = note.into();
        self
    }

    pub fn with_versions(mut self, versions: Vec<ApplyCandidateVersion>) -> Self {
        self.versions = versions;
        self
    }

    pub const fn is_visible_by_default(&self) -> bool {
        self.kind.is_visible_by_default()
    }

    pub const fn is_selected_by_default(&self) -> bool {
        self.kind.is_selected_by_default()
    }

    pub const fn is_force_candidate(&self) -> bool {
        self.kind.is_force_candidate()
    }

    pub fn update(&self) -> &PlannedUpdate {
        &self.update
    }

    pub fn update_tree_mut(&mut self, mut f: impl FnMut(&mut PlannedUpdate)) {
        f(&mut self.update);
        for version in &mut self.versions {
            f(&mut version.update);
        }
    }

    pub fn into_update(self) -> PlannedUpdate {
        self.update
    }

    pub fn note(&self) -> &str {
        &self.note
    }

    pub fn versions(&self) -> &[ApplyCandidateVersion] {
        &self.versions
    }

    pub fn has_selectable_versions(&self) -> bool {
        self.versions.len() > 1
    }

    pub fn clone_selected_update(&self, version_idx: usize) -> PlannedUpdate {
        self.versions
            .get(version_idx)
            .map_or_else(|| self.update.clone(), ApplyCandidateVersion::clone_update)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn planned_update(target: &str) -> PlannedUpdate {
        PlannedUpdate {
            manager: "test",
            name: "tool".to_string(),
            current: "1.0.0".to_string(),
            target: target.to_string(),
            delayed_latest: None,
            version_policy: None,
            apply_spec_base: None,
            gate_bypass: GateBypass::NONE,
        }
    }

    #[test]
    fn selected_version_preserves_gate_bypass() {
        let mut forced = planned_update("2.0.0");
        forced.gate_bypass = GateBypass {
            version_policy: true,
            min_release_age: true,
        };
        let candidate = ApplyCandidate::recommended(planned_update("1.1.0")).with_versions(vec![
            ApplyCandidateVersion::new(planned_update("1.1.0"), String::new(), false),
            ApplyCandidateVersion::new(forced, String::new(), true),
        ]);

        let selected = candidate.clone_selected_update(1);

        assert_eq!(selected.target, "2.0.0");
        assert_eq!(
            selected.gate_bypass,
            GateBypass {
                version_policy: true,
                min_release_age: true,
            }
        );
    }

    #[test]
    fn update_tree_mut_applies_to_selectable_versions() {
        let mut candidate =
            ApplyCandidate::recommended(planned_update("1.1.0")).with_versions(vec![
                ApplyCandidateVersion::new(planned_update("1.1.0"), String::new(), false),
                ApplyCandidateVersion::new(planned_update("2.0.0"), String::new(), false),
            ]);

        candidate.update_tree_mut(|update| {
            update.apply_spec_base = Some("example.com/tool/cmd/tool".to_string());
        });

        let selected = candidate.clone_selected_update(1);

        assert_eq!(
            selected.apply_spec_base.as_deref(),
            Some("example.com/tool/cmd/tool")
        );
    }
}
