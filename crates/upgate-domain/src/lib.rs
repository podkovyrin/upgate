//! Domain model crate for the `upgate` rebuild.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

mod audit;
mod config;
mod error;
mod manager;
mod plan;
mod policy;
mod release;
mod scan;
mod selection;
mod version;

pub use audit::{
    AuditFinding, AuditLookupResult, AuditPackageName, AuditQuery, AuditSubject, OsvEcosystem,
};
pub use config::{ManagerConfig, ManagerMode};
pub use error::DomainError;
pub use manager::{InstalledTool, ManagerCapabilities, ManagerId, PackageName, ToolId, ToolName};
pub use plan::{
    AdvisoryLatestFact, AdvisoryReleaseLookup, BlockReason, CandidateAgeFact,
    CandidateEvaluationFact, DelayReason, ExecutionSupport, ExecutionTargetKind,
    ManagerSelectedTarget, ManagerUpdateInput, MinAgeConstraintSupport, MissingMetadataKind,
    PlanDiagnostics, PlanItem, PlanItemId, PlannedTarget, PlannedTargetRef, PolicyBlockReason,
    ResolverNativeSupport, SkipReason, TargetSelection, UpdateCandidate, UpdatePlan, UpdateSeed,
};
pub use policy::{PolicyWarning, VersionPolicy};
pub use release::{
    ReleaseEntry, ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp,
    TargetAgeEvidence, TargetAgeLookupResult, VersionReleaseEvidence,
};
pub use scan::{
    ManagerRuleReason, ManagerScanEvidenceInput, ManagerScanInput, ScanIssue, ScanItem, ScanReport,
};
pub use selection::{
    PlanSelection, SelectedItem, SelectedUpdate, UpdateSelectionMode, UpdateSelectionPolicy,
};
pub use version::{VersionScheme, VersionText};
