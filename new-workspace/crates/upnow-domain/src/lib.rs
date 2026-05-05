//! Domain model crate for the `upnow` rebuild.

pub mod error;
pub mod manager;
pub mod plan;
pub mod policy;
pub mod release;
pub mod scan;
pub mod selection;
pub mod version;

pub use error::DomainError;
pub use manager::{
    InstalledTool, ManagerId, ManagerMetadata, ManagerMetadataField, ManagerMetadataKey,
    ManagerMetadataValue, PackageName, ToolId, ToolName,
};
pub use plan::{
    BlockReason, DelayReason, ExecutionEligibility, ManagerSelectedTarget, ManagerUpdateInput,
    PlanIssue, PlanItem, PlanItemId, PolicyBlockReason, SkipReason, TargetSelection,
    UpdateCandidate, UpdatePlan, UpdateSeed,
};
pub use policy::{PolicyWarning, VersionPolicy};
pub use release::{
    ReleaseEntry, ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp,
    TargetAgeEvidence, TargetAgeLookupResult,
};
pub use scan::{
    ManagerRuleReason, ManagerScanInput, ScanIssue, ScanItem, ScanReport, UnsupportedReason,
};
pub use selection::{PinChange, PinOperation, PlanSelection, SelectedItem};
pub use version::{VersionScheme, VersionText};
