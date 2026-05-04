use std::time::Duration;

use crate::{InstalledTool, ManagerId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedReason {
    YarnModernGlobalUnsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    pub manager_id: ManagerId,
    pub items: Vec<ScanItem>,
    pub issues: Vec<ScanIssue>,
}

impl ScanReport {
    #[must_use]
    pub fn new(manager_id: ManagerId, items: Vec<ScanItem>, issues: Vec<ScanIssue>) -> Self {
        Self {
            manager_id,
            items,
            issues,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanItem {
    Installed(InstalledTool),
    InstalledWithReleaseAge {
        tool: InstalledTool,
        age: Duration,
    },
    Skipped {
        tool: InstalledTool,
        reason: ScanIssue,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanIssue {
    ManagerUnavailable {
        detail: String,
    },
    DiscoveryFailed {
        detail: String,
    },
    ReleaseLookupFailed {
        detail: String,
    },
    MissingReleaseMetadata,
    UnsupportedManagerVersion {
        installed_version: crate::VersionText,
        reason: UnsupportedReason,
    },
    ExcludedByManagerRule(ManagerRuleReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerRuleReason {
    Dependency,
    DefaultGem,
    ManagerDefault,
    Other { detail: String },
}
