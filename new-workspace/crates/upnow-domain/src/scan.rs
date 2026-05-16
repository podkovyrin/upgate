use std::time::Duration;

use crate::{InstalledTool, ManagerId, VersionReleaseEvidence};

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
    pub const fn new(manager_id: ManagerId, items: Vec<ScanItem>, issues: Vec<ScanIssue>) -> Self {
        Self {
            manager_id,
            items,
            issues,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerScanInput {
    Installed(InstalledTool),
    Skipped {
        installed: InstalledTool,
        reason: ScanIssue,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerScanEvidenceInput {
    Installed {
        tool: InstalledTool,
        release_evidence: Option<VersionReleaseEvidence>,
    },
    Skipped {
        installed: InstalledTool,
        reason: ScanIssue,
    },
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
    /// Preserves the scan boundary for Brew dependency filtering.
    Dependency,
    /// Preserves the scan boundary for Gem default-gem skipping.
    DefaultGem,
    Other {
        detail: String,
    },
}
