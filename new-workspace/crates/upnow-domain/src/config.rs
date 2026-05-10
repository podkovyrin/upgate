use std::fmt::{self, Display};
use std::time::Duration;

use crate::{ManagerId, UpdateSelectionPolicy, VersionPolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerMode {
    Off,
    Plan,
    Apply,
}

impl ManagerMode {
    #[must_use]
    pub const fn allows_run(self, applying: bool) -> bool {
        match self {
            Self::Off => false,
            Self::Plan => !applying,
            Self::Apply => true,
        }
    }
}

impl Display for ManagerMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Off => "off",
            Self::Plan => "plan",
            Self::Apply => "apply",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerConfig {
    pub manager_id: ManagerId,
    pub mode: ManagerMode,
    pub min_release_age: Duration,
    pub version_policy: VersionPolicy,
    pub no_update: bool,
    pub selection: UpdateSelectionPolicy,
}
