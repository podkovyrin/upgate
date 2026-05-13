use std::fmt::{self, Display};
use std::time::Duration;

use upnow_domain::{
    InstalledTool, ManagerId, ManagerMode, ManagerScanInput, ManagerUpdateInput, PackageName,
    ReleaseLookupResult, UnsupportedReason, VersionPolicy, VersionText,
};
use upnow_execution::{ExecutionCommand, ResolvedExecutionPlan};
use upnow_infra::{Env, HttpClient, ProcessRunner};

pub use upnow_domain::ManagerCapabilities;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedManagerVersion {
    pub installed_version: VersionText,
    pub reason: UnsupportedReason,
}

#[derive(Debug, Clone, Copy)]
pub enum ReleaseLookupSubject<'a> {
    Package(&'a PackageName),
    Installed(&'a InstalledTool),
}

impl<'a> ReleaseLookupSubject<'a> {
    pub const fn package_name(self) -> &'a PackageName {
        match self {
            Self::Package(package) => package,
            Self::Installed(tool) => &tool.package_name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerAdapterErrorKind {
    Discovery,
    Parse,
    CommandConstruction,
    Interrupted,
    Infra,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerAdapterError {
    Manager {
        manager_id: String,
        kind: ManagerAdapterErrorKind,
        detail: String,
    },
    UnsupportedPolicy {
        manager_id: String,
        policy: VersionPolicy,
    },
    UnknownManager(String),
}

impl ManagerAdapterError {
    pub const fn is_interruption(&self) -> bool {
        matches!(
            self,
            Self::Manager {
                kind: ManagerAdapterErrorKind::Interrupted,
                ..
            }
        )
    }
}

impl Display for ManagerAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manager { detail, .. } => formatter.write_str(detail),
            Self::UnsupportedPolicy { manager_id, policy } => {
                write!(
                    formatter,
                    "manager `{manager_id}` does not support version policy `{policy}`"
                )
            }
            Self::UnknownManager(manager_id) => write!(formatter, "unknown manager `{manager_id}`"),
        }
    }
}

impl std::error::Error for ManagerAdapterError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagerConfigDefaults {
    pub min_release_age: Duration,
    pub mode: ManagerMode,
}

impl ManagerConfigDefaults {
    pub const fn apply_after_days(days: u64) -> Self {
        Self {
            min_release_age: Duration::from_secs(days * 24 * 60 * 60),
            mode: ManagerMode::Apply,
        }
    }

    pub const fn off_after_days(days: u64) -> Self {
        Self {
            min_release_age: Duration::from_secs(days * 24 * 60 * 60),
            mode: ManagerMode::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerConfigRuleError {
    MinReleaseAgeMustBeWholeDays,
}

pub trait ManagerAdapter {
    fn default_config() -> ManagerConfigDefaults
    where
        Self: Sized,
    {
        ManagerConfigDefaults::apply_after_days(7)
    }

    fn accepts_no_update() -> bool
    where
        Self: Sized,
    {
        false
    }

    fn supports_version_policy(policy: VersionPolicy) -> bool
    where
        Self: Sized,
    {
        let _ = policy;
        true
    }

    /// Validates manager-specific release-age config.
    ///
    /// # Errors
    ///
    /// Returns a rule error when the manager rejects the duration.
    fn validate_min_release_age_rule(
        min_release_age: Duration,
    ) -> Result<(), ManagerConfigRuleError>
    where
        Self: Sized,
    {
        let _ = min_release_age;
        Ok(())
    }

    fn capabilities(&self) -> ManagerCapabilities;

    /// Returns the installed manager version when it cannot support migrated behavior.
    ///
    /// # Errors
    ///
    /// Returns a manager adapter error when the version probe fails.
    fn unsupported_manager_version(
        &self,
        _process: &ProcessRunner,
    ) -> Result<Option<UnsupportedManagerVersion>, ManagerAdapterError> {
        Ok(None)
    }

    /// Discovers installed tools for scan output.
    ///
    /// # Errors
    ///
    /// Returns a manager adapter error when scan discovery fails.
    fn scan_inputs(
        &self,
        process: &ProcessRunner,
        env: &Env,
    ) -> Result<Vec<ManagerScanInput>, ManagerAdapterError>;

    /// Looks up release metadata for a package or installed tool.
    ///
    /// # Errors
    ///
    /// Returns a manager adapter error when the lookup cannot complete.
    fn release_lookup(
        &self,
        process: &ProcessRunner,
        http: &HttpClient,
        env: &Env,
        subject: ReleaseLookupSubject<'_>,
    ) -> Result<ReleaseLookupResult, ManagerAdapterError>;

    /// Discovers manager update inputs for shared planning.
    ///
    /// # Errors
    ///
    /// Returns a manager adapter error when update discovery fails.
    fn update_inputs(
        &self,
        process: &ProcessRunner,
        http: &HttpClient,
        env: &Env,
    ) -> Result<Vec<ManagerUpdateInput>, ManagerAdapterError>;

    /// Builds manager commands for an execution plan.
    ///
    /// # Errors
    ///
    /// Returns a manager adapter error when command construction fails.
    fn commands_for_execution_plan(
        &self,
        process: &ProcessRunner,
        env: &Env,
        plan: &ResolvedExecutionPlan,
    ) -> Result<Vec<ExecutionCommand>, ManagerAdapterError>;
}

/// Validates a manager-owned version-policy support decision.
///
/// # Errors
///
/// Returns an unsupported-policy error when the manager rule rejects the policy.
pub fn validate_version_policy(
    id: &ManagerId,
    supported: bool,
    policy: VersionPolicy,
) -> Result<(), ManagerAdapterError> {
    if supported {
        Ok(())
    } else {
        Err(ManagerAdapterError::UnsupportedPolicy {
            manager_id: id.as_str().to_owned(),
            policy,
        })
    }
}
