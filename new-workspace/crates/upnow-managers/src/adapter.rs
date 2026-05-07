use std::fmt::{self, Display};
use std::time::Duration;

use upnow_domain::{
    InstalledTool, ManagerId, ManagerScanInput, ManagerUpdateInput, PackageName, PlanItemId,
    ReleaseLookupResult, UnsupportedReason, VersionPolicy, VersionText,
};
use upnow_execution::ResolvedExecutionPlan;
use upnow_infra::{CommandSpec, Env, HttpClient, ProcessRunner};

pub use upnow_domain::ManagerCapabilities;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerDefaultMode {
    Off,
    Apply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagerDefaults {
    pub min_release_age: Duration,
    pub mode: ManagerDefaultMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandBuildSettings {
    pub min_release_age: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerExecutionCommandItem {
    pub plan_item_id: PlanItemId,
    pub package_name: PackageName,
    pub installed_version: VersionText,
    pub target_version: VersionText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerExecutionCommand {
    pub items: Vec<ManagerExecutionCommandItem>,
    pub command: CommandSpec,
}

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
    #[must_use]
    pub fn package_name(self) -> &'a PackageName {
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
    #[must_use]
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

pub trait ManagerAdapter {
    fn id(&self) -> &'static str;

    fn defaults(&self) -> ManagerDefaults;

    fn capabilities(&self) -> ManagerCapabilities;

    fn supports_version_policy(&self, policy: VersionPolicy) -> bool;

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
        version_policy: VersionPolicy,
        min_release_age: Duration,
        no_update: bool,
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
        settings: CommandBuildSettings,
    ) -> Result<Vec<ManagerExecutionCommand>, ManagerAdapterError>;

    fn manager_id(&self) -> ManagerId {
        ManagerId::new(self.id()).expect("static manager id should be valid")
    }

    /// Validates that a manager supports the resolved version policy.
    ///
    /// # Errors
    ///
    /// Returns an unsupported-policy error when the policy is not supported.
    fn validate_version_policy(&self, policy: VersionPolicy) -> Result<(), ManagerAdapterError> {
        if self.supports_version_policy(policy) {
            Ok(())
        } else {
            Err(ManagerAdapterError::UnsupportedPolicy {
                manager_id: self.id().to_owned(),
                policy,
            })
        }
    }
}
