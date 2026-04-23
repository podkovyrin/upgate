use super::types::{OutcomeStatus, ReasonCode};

#[derive(Debug, Clone)]
pub struct ItemOutcome {
    pub manager: &'static str,
    pub name: String,
    pub from_version: String,
    pub to_version: String,
    pub status: OutcomeStatus,
    pub reason_code: Option<ReasonCode>,
    pub reason_detail: Option<String>,
    pub age: Option<String>,
    pub required_age: Option<String>,
    pub latest_version: Option<String>,
    pub latest_age: Option<String>,
    pub version_policy: Option<String>,
    pub latest_blocked_by_policy_version: Option<String>,
    pub version_policy_warning: Option<String>,
    pub scan_age: Option<String>,
    pub scan_is_old: bool,
}

impl ItemOutcome {
    fn base(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        status: OutcomeStatus,
    ) -> Self {
        Self {
            manager,
            name: name.into(),
            from_version: from_version.into(),
            to_version: to_version.into(),
            status,
            reason_code: None,
            reason_detail: None,
            age: None,
            required_age: None,
            latest_version: None,
            latest_age: None,
            version_policy: None,
            latest_blocked_by_policy_version: None,
            version_policy_warning: None,
            scan_age: None,
            scan_is_old: false,
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
        );
        outcome.scan_age = Some(age.into());
        outcome.scan_is_old = is_old;
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
        );
        outcome.reason_code = Some(ReasonCode::TooFresh);
        outcome.required_age = Some(required_age.into());
        outcome.latest_version = Some(latest_version.into());
        outcome.latest_age = Some(latest_age.into());
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
        );
        outcome.reason_code = Some(ReasonCode::NoEligibleRelease);
        outcome.required_age = Some(required_age.into());
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
        let mut outcome = Self::base(
            manager,
            name,
            current_version,
            latest_version,
            OutcomeStatus::Delayed,
        );
        outcome.reason_code = Some(ReasonCode::NoEligibleRelease);
        outcome.required_age = Some(required_age.into());
        outcome.latest_age = Some(latest_age.into());
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
        let mut outcome = Self::base(
            manager,
            name,
            from_version,
            to_version,
            OutcomeStatus::Delayed,
        );
        outcome.reason_code = Some(ReasonCode::TooFresh);
        outcome.age = Some(age.into());
        outcome.required_age = Some(required_age.into());
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
        let mut outcome = Self::base(
            manager,
            name,
            from_version,
            to_version,
            OutcomeStatus::Skipped,
        );
        outcome.reason_code = Some(reason_code);
        outcome.reason_detail = Some(reason_detail.into());
        outcome
    }

    pub fn skipped_no_change(
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
            OutcomeStatus::Skipped,
        );
        outcome.reason_code = Some(ReasonCode::NoChange);
        outcome.reason_detail = Some("already at selected target".to_string());
        outcome
    }

    pub fn error(
        manager: &'static str,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        reason_code: ReasonCode,
        reason_detail: impl Into<String>,
    ) -> Self {
        let mut outcome = Self::base(
            manager,
            name,
            from_version,
            to_version,
            OutcomeStatus::Error,
        );
        outcome.reason_code = Some(reason_code);
        outcome.reason_detail = Some(reason_detail.into());
        outcome
    }
}
