use crate::{InstalledTool, ManagerId};

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
    Skipped {
        tool: InstalledTool,
        reason: ScanIssue,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanIssue {
    ManagerUnavailable { detail: String },
    DiscoveryFailed { detail: String },
    ReleaseLookupFailed { detail: String },
    MissingReleaseMetadata,
    ExcludedByManagerRule(ManagerRuleReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerRuleReason {
    Dependency,
    DefaultGem,
    ManagerDefault,
    Other { detail: String },
}
