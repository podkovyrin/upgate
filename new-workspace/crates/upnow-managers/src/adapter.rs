use std::fmt::{self, Display};
use std::time::Duration;

use upnow_domain::{
    ManagerId, PackageName, PlanItemId, PlanSelection, UpdatePlan, UpdateSeed, VersionPolicy,
    VersionText,
};
use upnow_infra::{CommandSpec, ProcessRunner};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagerCapabilities {
    pub exact_target: bool,
    pub native_update: bool,
}

impl ManagerCapabilities {
    #[must_use]
    pub const fn new(exact_target: bool, native_update: bool) -> Self {
        Self {
            exact_target,
            native_update,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandBuildSettings {
    pub version_policy: VersionPolicy,
    pub min_release_age: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerExecutionCommand {
    pub plan_item_id: PlanItemId,
    pub package_name: PackageName,
    pub installed_version: VersionText,
    pub target_version: VersionText,
    pub command: CommandSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerAdapterErrorKind {
    Discovery,
    Parse,
    ReleaseLookup,
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

    fn capabilities(&self) -> ManagerCapabilities;

    fn supports_version_policy(&self, policy: VersionPolicy) -> bool;

    fn update_seeds(
        &self,
        process: &ProcessRunner,
        version_policy: VersionPolicy,
    ) -> Result<Vec<UpdateSeed>, ManagerAdapterError>;

    fn commands_for_selection(
        &self,
        plan: &UpdatePlan,
        selection: &PlanSelection,
        settings: CommandBuildSettings,
    ) -> Result<Vec<ManagerExecutionCommand>, ManagerAdapterError>;

    fn manager_id(&self) -> ManagerId {
        ManagerId::new(self.id()).expect("static manager id should be valid")
    }

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
