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
    BlockReason, DelayReason, ExecutionEligibility, PlanItem, PlanItemId, PolicyBlockReason,
    SkipReason, UpdateCandidate, UpdatePlan, UpdateSeed,
};
pub use policy::VersionPolicy;
pub use release::{
    ReleaseEntry, ReleaseLookupError, ReleaseLookupResult, ReleaseTimeline, ReleaseTimestamp,
};
pub use scan::{ManagerRuleReason, ScanIssue, ScanItem, ScanReport};
pub use selection::{PinChange, PinOperation, PlanSelection, SelectedItem};
pub use version::{VersionScheme, VersionText};
