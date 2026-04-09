use crate::manager::Manager;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutcomeStatus {
    Update,
    Delayed,
    Skipped,
    Error,
}

pub(crate) const REASON_TOO_FRESH: &str = "too_fresh";
pub(crate) const REASON_NO_ELIGIBLE_RELEASE: &str = "no_eligible_release";
pub(crate) const REASON_NO_CHANGE: &str = "no_change";
pub(crate) const REASON_PINNED: &str = "pinned";
pub(crate) const REASON_MISSING_METADATA: &str = "missing_metadata";
pub(crate) const REASON_COMMAND_FAILED: &str = "command_failed";

#[derive(Debug, Clone)]
pub(crate) struct ItemOutcome {
    pub(crate) manager: Manager,
    pub(crate) name: String,
    pub(crate) from_version: String,
    pub(crate) to_version: String,
    pub(crate) status: OutcomeStatus,
    pub(crate) source: String,
    pub(crate) reason_code: Option<&'static str>,
    pub(crate) reason_detail: Option<String>,
    pub(crate) age: Option<String>,
    pub(crate) required_age: Option<String>,
    pub(crate) latest_version: Option<String>,
    pub(crate) latest_age: Option<String>,
}

impl ItemOutcome {
    pub(crate) fn update(
        manager: Manager,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            name: name.into(),
            from_version: from_version.into(),
            to_version: to_version.into(),
            status: OutcomeStatus::Update,
            source: source.into(),
            reason_code: None,
            reason_detail: None,
            age: None,
            required_age: None,
            latest_version: None,
            latest_age: None,
        }
    }

    pub(crate) fn update_with_delayed_latest(
        manager: Manager,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        source: impl Into<String>,
        latest_version: impl Into<String>,
        latest_age: impl Into<String>,
        required_age: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            name: name.into(),
            from_version: from_version.into(),
            to_version: to_version.into(),
            status: OutcomeStatus::Update,
            source: source.into(),
            reason_code: Some(REASON_TOO_FRESH),
            reason_detail: None,
            age: None,
            required_age: Some(required_age.into()),
            latest_version: Some(latest_version.into()),
            latest_age: Some(latest_age.into()),
        }
    }

    pub(crate) fn delayed_no_eligible(
        manager: Manager,
        name: impl Into<String>,
        version: impl Into<String>,
        source: impl Into<String>,
        required_age: impl Into<String>,
    ) -> Self {
        let version = version.into();
        Self {
            manager,
            name: name.into(),
            from_version: version.clone(),
            to_version: version,
            status: OutcomeStatus::Delayed,
            source: source.into(),
            reason_code: Some(REASON_NO_ELIGIBLE_RELEASE),
            reason_detail: None,
            age: None,
            required_age: Some(required_age.into()),
            latest_version: None,
            latest_age: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn delayed_too_fresh(
        manager: Manager,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        source: impl Into<String>,
        age: impl Into<String>,
        required_age: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            name: name.into(),
            from_version: from_version.into(),
            to_version: to_version.into(),
            status: OutcomeStatus::Delayed,
            source: source.into(),
            reason_code: Some(REASON_TOO_FRESH),
            reason_detail: None,
            age: Some(age.into()),
            required_age: Some(required_age.into()),
            latest_version: None,
            latest_age: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn skipped(
        manager: Manager,
        name: impl Into<String>,
        from_version: impl Into<String>,
        to_version: impl Into<String>,
        source: impl Into<String>,
        reason_code: &'static str,
        reason_detail: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            name: name.into(),
            from_version: from_version.into(),
            to_version: to_version.into(),
            status: OutcomeStatus::Skipped,
            source: source.into(),
            reason_code: Some(reason_code),
            reason_detail: Some(reason_detail.into()),
            age: None,
            required_age: None,
            latest_version: None,
            latest_age: None,
        }
    }

    pub(crate) fn skipped_no_change(
        manager: Manager,
        name: impl Into<String>,
        version: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        let version = version.into();
        Self {
            manager,
            name: name.into(),
            from_version: version.clone(),
            to_version: version,
            status: OutcomeStatus::Skipped,
            source: source.into(),
            reason_code: Some(REASON_NO_CHANGE),
            reason_detail: Some("already at selected target".to_string()),
            age: None,
            required_age: None,
            latest_version: None,
            latest_age: None,
        }
    }

    pub(crate) fn to_text_line(&self) -> Option<String> {
        // Manager enum is the source of truth for the manager prefix.
        if self.status == OutcomeStatus::Skipped && self.reason_code == Some(REASON_NO_CHANGE) {
            return None;
        }

        let manager = self.manager.as_str();
        let from = version_label(&self.from_version);
        let to = version_label(&self.to_version);

        match self.status {
            OutcomeStatus::Update => {
                if let (Some(latest), Some(latest_age), Some(required_age)) = (
                    self.latest_version.as_deref(),
                    self.latest_age.as_deref(),
                    self.required_age.as_deref(),
                ) {
                    Some(format!(
                        "{manager}: {} {} -> {} (source: {}; latest {} delayed: {} < {})",
                        self.name,
                        from,
                        to,
                        self.source,
                        version_label(latest),
                        latest_age,
                        required_age
                    ))
                } else {
                    Some(format!(
                        "{manager}: {} {} -> {} (source: {})",
                        self.name, from, to, self.source
                    ))
                }
            }
            OutcomeStatus::Delayed => {
                if self.reason_code == Some(REASON_NO_ELIGIBLE_RELEASE) {
                    let required_age = self.required_age.as_deref().unwrap_or("unknown");
                    return Some(format!(
                        "{manager}: {} {} -> {} (delayed, no eligible release >= current within {} window, source: {})",
                        self.name, from, to, required_age, self.source
                    ));
                }

                if let (Some(age), Some(required_age)) =
                    (self.age.as_deref(), self.required_age.as_deref())
                {
                    return Some(format!(
                        "{manager}: {} {} -> {} (delayed, {} < {}, source: {})",
                        self.name, from, to, age, required_age, self.source
                    ));
                }

                Some(format!(
                    "{manager}: {} {} -> {} (delayed, source: {})",
                    self.name, from, to, self.source
                ))
            }
            OutcomeStatus::Skipped => {
                if let Some(reason) = &self.reason_detail {
                    Some(format!(
                        "{manager}: {} {} -> {} (skipped, {}, source: {})",
                        self.name, from, to, reason, self.source
                    ))
                } else {
                    Some(format!(
                        "{manager}: {} {} -> {} (skipped, source: {})",
                        self.name, from, to, self.source
                    ))
                }
            }
            OutcomeStatus::Error => {
                if let Some(reason) = &self.reason_detail {
                    Some(format!(
                        "{manager}: {} {} -> {} (error, {}, source: {})",
                        self.name, from, to, reason, self.source
                    ))
                } else {
                    Some(format!(
                        "{manager}: {} {} -> {} (error, source: {})",
                        self.name, from, to, self.source
                    ))
                }
            }
        }
    }
}

pub(crate) fn emit_text_outcome(outcome: &ItemOutcome) {
    if let Some(line) = outcome.to_text_line() {
        println!("{line}");
    }
}

#[allow(dead_code)]
pub(crate) fn emit_text_outcomes(outcomes: &[ItemOutcome]) {
    for outcome in outcomes {
        emit_text_outcome(outcome);
    }
}

pub(crate) fn version_label(version: &str) -> String {
    if version.starts_with('v') {
        return version.to_string();
    }

    match version.chars().next() {
        Some(c) if c.is_ascii_digit() => format!("v{version}"),
        _ => version.to_string(),
    }
}
