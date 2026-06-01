use std::fmt::{self, Display};
use std::time::Duration;

use upnow_domain::{
    InstalledTool, ManagerId, ManagerMode, ManagerScanEvidenceInput, ManagerScanInput,
    ManagerUpdateInput, PackageName, ReleaseEvidenceSource, ReleaseLookupResult, VersionPolicy,
};
use upnow_execution::{ExecutionCommand, ResolvedExecutionPlan};
use upnow_infra::{Env, HttpClient, ProcessRunner, run_ordered_parallel};
use upnow_release::release_evidence_for_version;

pub use upnow_domain::ManagerCapabilities;

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

pub trait ManagerAdapter: Sync {
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

    fn required_executable() -> &'static str
    where
        Self: Sized;

    fn capabilities(&self) -> ManagerCapabilities;

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

    /// Discovers scan inputs with optional release evidence.
    ///
    /// Managers should override this when efficient verbose scan depends on
    /// manager-owned discovery, runtime resolution, batch metadata APIs, or
    /// native target-age evidence. The default preserves the item-by-item
    /// lookup behavior used by managers without a specialized evidence path.
    ///
    /// # Errors
    ///
    /// Returns a manager adapter error when scan discovery or lookup fails.
    fn scan_inputs_with_release_evidence(
        &self,
        process: &ProcessRunner,
        http: &HttpClient,
        env: &Env,
        max_parallel_checks_per_manager: usize,
    ) -> Result<Vec<ManagerScanEvidenceInput>, ManagerAdapterError> {
        run_ordered_parallel(
            self.scan_inputs(process, env)?,
            max_parallel_checks_per_manager.max(1),
            "verbose scan release evidence",
            |input| match input {
                ManagerScanInput::Installed(tool) => match self.release_lookup(
                    process,
                    http,
                    env,
                    ReleaseLookupSubject::Installed(&tool),
                )? {
                    ReleaseLookupResult::Known(timeline) => {
                        let release_evidence = release_evidence_for_version(
                            &timeline,
                            &tool.installed_version,
                            ReleaseEvidenceSource::ReleaseTimeline,
                        );
                        Ok(ManagerScanEvidenceInput::Installed {
                            tool,
                            release_evidence,
                        })
                    }
                    ReleaseLookupResult::MissingMetadata | ReleaseLookupResult::LookupFailed(_) => {
                        Ok(ManagerScanEvidenceInput::Installed {
                            tool,
                            release_evidence: None,
                        })
                    }
                },
                ManagerScanInput::Skipped { installed, reason } => {
                    Ok(ManagerScanEvidenceInput::Skipped { installed, reason })
                }
            },
        )
        .map_err(|err| ManagerAdapterError::Manager {
            manager_id: "adapter".to_owned(),
            kind: ManagerAdapterErrorKind::Infra,
            detail: err.to_string(),
        })?
        .into_iter()
        .collect()
    }

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
        max_parallel_checks_per_manager: usize,
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
