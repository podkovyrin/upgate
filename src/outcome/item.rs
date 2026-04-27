use super::types::{
    AgeGateDiagnostic, CurrentReason, DelayedReason, ErrorReason, OutcomeDiagnostics,
    OutcomeReason, OutcomeStatus, OutcomeSubject, OutcomeVersions, OutcomeVisibility, ReasonCode,
    ReleaseAgeDiagnostic, SkippedReason, UpdateReason, VersionPolicyDiagnostic,
};

#[derive(Debug, Clone)]
pub struct ItemOutcome {
    pub manager: &'static str,
    pub name: String,
    pub status: OutcomeStatus,
    pub subject: OutcomeSubject,
    pub versions: OutcomeVersions,
    pub reason: OutcomeReason,
    pub visibility: OutcomeVisibility,
    pub diagnostics: OutcomeDiagnostics,
}

impl ItemOutcome {
    fn base(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        status: OutcomeStatus,
        reason: OutcomeReason,
    ) -> Self {
        let name = name.into();
        let from_version = from_version.into();
        let to_version = to_version.into();
        let subject = if name == "*" && from_version == "*" && to_version == "*" {
            OutcomeSubject::Manager
        } else {
            OutcomeSubject::Item
        };
        let versions = match subject {
            OutcomeSubject::Manager => OutcomeVersions::None,
            OutcomeSubject::Item if status == OutcomeStatus::Current => OutcomeVersions::Current {
                version: from_version,
            },
            OutcomeSubject::Item => OutcomeVersions::Change {
                from: from_version,
                to: to_version,
            },
        };

        Self {
            manager,
            name,
            status,
            subject,
            versions,
            reason,
            visibility: OutcomeVisibility::Always,
            diagnostics: OutcomeDiagnostics::default(),
        }
    }

    pub fn current(
        manager: &'static str,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        let version = version.into();
        Self::base(
            manager,
            name,
            version.clone(),
            version,
            OutcomeStatus::Current,
            OutcomeReason::Current(CurrentReason::Scan),
        )
    }

    pub fn current_with_age(
        manager: &'static str,
        name: impl Into<String>,
        version: impl Into<String>,
        age: impl Into<String>,
        is_old: bool,
    ) -> Self {
        let version = version.into();
        let mut outcome = Self::base(
            manager,
            name,
            version.clone(),
            version,
            OutcomeStatus::Current,
            OutcomeReason::Current(CurrentReason::Scan),
        );
        let age = age.into();
        outcome.diagnostics.release_age = Some(ReleaseAgeDiagnostic { age, is_old });
        outcome
    }

    pub fn update(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
    ) -> Self {
        Self::base(
            manager,
            name,
            from_version,
            to_version,
            OutcomeStatus::Update,
            OutcomeReason::Update(UpdateReason::Eligible),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_with_delayed_latest(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        latest_version: impl Into<String>,
        latest_age: impl Into<String>,
        required_age: impl Into<String>,
    ) -> Self {
        let mut outcome = Self::base(
            manager,
            name,
            from_version,
            to_version,
            OutcomeStatus::Update,
            OutcomeReason::Update(UpdateReason::LatestTooFresh),
        );
        let latest_version = latest_version.into();
        let latest_age = latest_age.into();
        let required_age = required_age.into();
        outcome.diagnostics.latest_too_fresh = Some(AgeGateDiagnostic {
            version: Some(latest_version),
            age: latest_age,
            required_age,
        });
        outcome
    }

    pub fn delayed_no_eligible(
        manager: &'static str,
        name: impl Into<String>,
        version: impl Into<String>,
        required_age: impl Into<String>,
    ) -> Self {
        let version = version.into();
        let mut outcome = Self::base(
            manager,
            name,
            version.clone(),
            version,
            OutcomeStatus::Delayed,
            OutcomeReason::Delayed(DelayedReason::NoAgeEligibleRelease),
        );
        let required_age = required_age.into();
        outcome.diagnostics.required_age = Some(required_age);
        outcome
    }

    pub fn delayed_no_eligible_with_latest(
        manager: &'static str,
        name: impl Into<String>,
        current_version: impl Into<String>,
        latest_version: impl Into<String>,
        latest_age: impl Into<String>,
        required_age: impl Into<String>,
    ) -> Self {
        let latest_version = latest_version.into();
        let mut outcome = Self::base(
            manager,
            name,
            current_version,
            latest_version.clone(),
            OutcomeStatus::Delayed,
            OutcomeReason::Delayed(DelayedReason::NoAgeEligibleRelease),
        );
        let latest_age = latest_age.into();
        let required_age = required_age.into();
        outcome.diagnostics.latest_too_fresh = Some(AgeGateDiagnostic {
            version: Some(latest_version),
            age: latest_age,
            required_age,
        });
        outcome
    }

    pub fn delayed_too_fresh(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        age: impl Into<String>,
        required_age: impl Into<String>,
    ) -> Self {
        let to_version = to_version.into();
        let mut outcome = Self::base(
            manager,
            name,
            from_version,
            to_version.clone(),
            OutcomeStatus::Delayed,
            OutcomeReason::Delayed(DelayedReason::TargetTooFresh),
        );
        let age = age.into();
        let required_age = required_age.into();
        outcome.diagnostics.target_too_fresh = Some(AgeGateDiagnostic {
            version: Some(to_version),
            age,
            required_age,
        });
        outcome
    }

    pub fn skipped(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        reason_code: ReasonCode,
        reason_detail: impl Into<String>,
    ) -> Self {
        let reason = match reason_code {
            ReasonCode::Pinned => OutcomeReason::Skipped(SkippedReason::Pinned),
            ReasonCode::MissingMetadata => OutcomeReason::Skipped(SkippedReason::MissingMetadata),
            ReasonCode::UnsupportedPlatform => {
                OutcomeReason::Skipped(SkippedReason::UnsupportedPlatform)
            }
            ReasonCode::MissingCommand => OutcomeReason::Skipped(SkippedReason::MissingCommand),
            ReasonCode::CommandFailed => OutcomeReason::Error(ErrorReason::CommandFailed),
        };
        let mut outcome = Self::base(
            manager,
            name,
            from_version,
            to_version,
            OutcomeStatus::Skipped,
            reason,
        );
        let reason_detail = reason_detail.into();
        outcome.diagnostics.detail = Some(reason_detail);
        outcome
    }

    fn skipped_with_reason(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        reason: SkippedReason,
        reason_detail: impl Into<String>,
    ) -> Self {
        let mut outcome = Self::base(
            manager,
            name,
            from_version,
            to_version,
            OutcomeStatus::Skipped,
            OutcomeReason::Skipped(reason),
        );
        let reason_detail = reason_detail.into();
        outcome.diagnostics.detail = Some(reason_detail);
        outcome
    }

    pub fn skipped_pinned(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
    ) -> Self {
        Self::skipped_with_reason(
            manager,
            name,
            from_version,
            to_version,
            SkippedReason::Pinned,
            "pinned",
        )
    }

    pub fn skipped_missing_metadata(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        reason_detail: impl Into<String>,
    ) -> Self {
        Self::skipped_with_reason(
            manager,
            name,
            from_version,
            to_version,
            SkippedReason::MissingMetadata,
            reason_detail,
        )
    }

    pub fn current_no_newer(
        manager: &'static str,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        let version = version.into();
        let mut outcome = Self::base(
            manager,
            name,
            version.clone(),
            version,
            OutcomeStatus::Current,
            OutcomeReason::Current(CurrentReason::NoNewerVersion),
        );
        outcome.visibility = OutcomeVisibility::VerboseOnly;
        outcome.diagnostics.detail = Some("no newer version found".to_string());
        outcome
    }

    pub fn set_delayed_reason(&mut self, reason: DelayedReason) {
        if self.status == OutcomeStatus::Delayed {
            self.reason = OutcomeReason::Delayed(reason);
        }
    }

    pub fn error(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        reason_code: ReasonCode,
        reason_detail: impl Into<String>,
    ) -> Self {
        let reason = match reason_code {
            ReasonCode::MissingMetadata => OutcomeReason::Error(ErrorReason::ResolverFailed),
            ReasonCode::CommandFailed
            | ReasonCode::UnsupportedPlatform
            | ReasonCode::MissingCommand
            | ReasonCode::Pinned => OutcomeReason::Error(ErrorReason::CommandFailed),
        };
        let mut outcome = Self::base(
            manager,
            name,
            from_version,
            to_version,
            OutcomeStatus::Error,
            reason,
        );
        let reason_detail = reason_detail.into();
        outcome.diagnostics.detail = Some(reason_detail);
        outcome
    }

    pub fn resolver_error(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        reason_detail: impl Into<String>,
    ) -> Self {
        let mut outcome = Self::base(
            manager,
            name,
            from_version,
            to_version,
            OutcomeStatus::Error,
            OutcomeReason::Error(ErrorReason::ResolverFailed),
        );
        let reason_detail = reason_detail.into();
        outcome.diagnostics.detail = Some(reason_detail);
        outcome
    }

    pub fn set_version_policy(
        &mut self,
        policy: impl Into<String>,
        latest_blocked_version: Option<String>,
        warning: Option<String>,
    ) {
        let policy = policy.into();
        let has_blocked_version = latest_blocked_version.is_some();
        self.diagnostics.version_policy = Some(VersionPolicyDiagnostic {
            policy,
            latest_blocked_version,
            warning,
        });

        match self.status {
            OutcomeStatus::Current => {
                self.reason = OutcomeReason::Current(CurrentReason::BlockedByVersionPolicy);
            }
            OutcomeStatus::Update if has_blocked_version => {
                self.reason = OutcomeReason::Update(UpdateReason::LatestBlockedByVersionPolicy);
            }
            _ => {}
        }
    }
}
