//! Domain model crate for the `upnow` rebuild.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

pub mod config;
pub mod error;
pub mod manager;
pub mod plan;
pub mod policy;
pub mod release;
pub mod scan;
pub mod selection;
pub mod version;

pub use config::{ManagerConfig, ManagerMode};
pub use error::DomainError;
pub use manager::{
    InstalledTool, ManagerCapabilities, ManagerId, ManagerMetadata, ManagerMetadataField,
    ManagerMetadataKey, ManagerMetadataValue, PackageName, ToolId, ToolName,
};
pub use plan::{
    AdvisoryLatestFact, AdvisoryReleaseLookup, BlockReason, CandidateAgeFact, CandidateAgeSource,
    CandidateEvaluationFact, DelayReason, ExecutionEligibility, ExecutionTargetKind,
    ManagerSelectedTarget, ManagerUpdateInput, MissingMetadataKind, PlanDiagnostics, PlanIssue,
    PlanItem, PlanItemId, PolicyBlockReason, SkipReason, TargetSelection, UpdateCandidate,
    UpdatePlan, UpdateSeed,
};
pub use policy::{PolicyWarning, VersionPolicy};
pub use release::{
    ReleaseEntry, ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp,
    TargetAgeEvidence, TargetAgeLookupResult,
};
pub use scan::{
    ManagerRuleReason, ManagerScanInput, ScanIssue, ScanItem, ScanReport, UnsupportedReason,
};
pub use selection::{
    PlanSelection, SelectedItem, SelectedTarget, UpdateSelectionMode, UpdateSelectionPolicy,
};
pub use version::{VersionScheme, VersionText};
