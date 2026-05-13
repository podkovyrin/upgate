use std::time::Duration;

use crate::{
    DomainError, InstalledTool, ManagerId, PackageName, PolicyWarning, ReleaseLookupError,
    ReleaseLookupResult, TargetAgeLookupResult, ToolId, UnsupportedReason, VersionScheme,
    VersionText,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlanItemId(String);

impl PlanItemId {
    /// Creates a plan item id.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::EmptyPlanItemId`] when the value is blank.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DomainError::EmptyPlanItemId);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSeed {
    pub installed: InstalledTool,
    pub version_scheme: VersionScheme,
    pub target_selection: TargetSelection,
    pub execution_eligibility: ExecutionEligibility,
    pub execution_target_kind: ExecutionTargetKind,
}

impl UpdateSeed {
    pub const fn new(
        installed: InstalledTool,
        discovered_target: VersionText,
        version_scheme: VersionScheme,
        release_lookup: ReleaseLookupResult,
        execution_eligibility: ExecutionEligibility,
    ) -> Self {
        Self::planner_selectable(
            installed,
            discovered_target,
            version_scheme,
            release_lookup,
            execution_eligibility,
        )
    }
    pub const fn planner_selectable(
        installed: InstalledTool,
        discovered_target: VersionText,
        version_scheme: VersionScheme,
        release_lookup: ReleaseLookupResult,
        execution_eligibility: ExecutionEligibility,
    ) -> Self {
        Self {
            installed,
            version_scheme,
            target_selection: TargetSelection::PlannerSelectable {
                discovered_target,
                release_lookup,
            },
            execution_eligibility,
            execution_target_kind: ExecutionTargetKind::Standard,
        }
    }
    pub const fn manager_selected(
        installed: InstalledTool,
        selected_target: ManagerSelectedTarget,
        version_scheme: VersionScheme,
        execution_eligibility: ExecutionEligibility,
    ) -> Self {
        Self {
            installed,
            version_scheme,
            target_selection: TargetSelection::ManagerSelected(selected_target),
            execution_eligibility,
            execution_target_kind: ExecutionTargetKind::Standard,
        }
    }
    pub const fn with_execution_target_kind(
        mut self,
        execution_target_kind: ExecutionTargetKind,
    ) -> Self {
        self.execution_target_kind = execution_target_kind;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetSelection {
    PlannerSelectable {
        discovered_target: VersionText,
        release_lookup: ReleaseLookupResult,
    },
    ManagerSelected(ManagerSelectedTarget),
}

impl TargetSelection {
    pub const fn target_version(&self) -> &VersionText {
        match self {
            Self::PlannerSelectable {
                discovered_target, ..
            } => discovered_target,
            Self::ManagerSelected(target) => &target.target_version,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerSelectedTarget {
    pub target_version: VersionText,
    pub target_age: TargetAgeLookupResult,
    pub advisory_release_lookup: Option<AdvisoryReleaseLookup>,
}

impl ManagerSelectedTarget {
    pub const fn new(target_version: VersionText, target_age: TargetAgeLookupResult) -> Self {
        Self {
            target_version,
            target_age,
            advisory_release_lookup: None,
        }
    }
    pub fn with_advisory_release_lookup(
        mut self,
        latest_version: VersionText,
        advisory_release_lookup: ReleaseLookupResult,
    ) -> Self {
        self.advisory_release_lookup = Some(AdvisoryReleaseLookup {
            latest_version,
            release_lookup: advisory_release_lookup,
        });
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisoryReleaseLookup {
    pub latest_version: VersionText,
    pub release_lookup: ReleaseLookupResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerUpdateInput {
    Seed(UpdateSeed),
    Skipped {
        installed: InstalledTool,
        reason: SkipReason,
    },
    ResolverError {
        installed: InstalledTool,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCandidate {
    pub tool_id: ToolId,
    pub package_name: PackageName,
    pub installed_version: VersionText,
    pub target_version: VersionText,
    pub version_scheme: VersionScheme,
    pub execution_eligibility: ExecutionEligibility,
    pub execution_target_kind: ExecutionTargetKind,
    pub policy_warnings: Vec<PolicyWarning>,
    pub diagnostics: PlanDiagnostics,
}

impl UpdateCandidate {
    pub fn new(
        tool_id: ToolId,
        package_name: PackageName,
        installed_version: VersionText,
        target_version: VersionText,
        version_scheme: VersionScheme,
        execution_eligibility: ExecutionEligibility,
    ) -> Self {
        Self {
            tool_id,
            package_name,
            installed_version,
            target_version,
            version_scheme,
            execution_eligibility,
            execution_target_kind: ExecutionTargetKind::Standard,
            policy_warnings: Vec::new(),
            diagnostics: PlanDiagnostics::default(),
        }
    }
    pub const fn with_execution_target_kind(
        mut self,
        execution_target_kind: ExecutionTargetKind,
    ) -> Self {
        self.execution_target_kind = execution_target_kind;
        self
    }
    pub fn with_policy_warnings(mut self, policy_warnings: Vec<PolicyWarning>) -> Self {
        self.policy_warnings = policy_warnings;
        self
    }
    pub fn with_diagnostics(mut self, diagnostics: PlanDiagnostics) -> Self {
        self.diagnostics = diagnostics;
        self
    }
    pub const fn tool_id(&self) -> &ToolId {
        &self.tool_id
    }
    pub const fn package_name(&self) -> &PackageName {
        &self.package_name
    }
    pub const fn installed_version(&self) -> &VersionText {
        &self.installed_version
    }
    pub const fn target_version(&self) -> &VersionText {
        &self.target_version
    }
    pub const fn version_scheme(&self) -> VersionScheme {
        self.version_scheme
    }
    pub const fn execution_eligibility(&self) -> ExecutionEligibility {
        self.execution_eligibility
    }
    pub const fn execution_target_kind(&self) -> ExecutionTargetKind {
        self.execution_target_kind
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionEligibility {
    NativeOrExact,
    ExactOnly,
    ExactOrNativeGlobal,
    NativeOnly,
    ResolverNativeOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionTargetKind {
    Standard,
    BrewFormula,
    BrewCask,
}

impl ExecutionEligibility {
    pub const fn supports_exact_target(self) -> bool {
        matches!(
            self,
            Self::NativeOrExact | Self::ExactOnly | Self::ExactOrNativeGlobal
        )
    }
    pub const fn supports_native_target(self) -> bool {
        matches!(self, Self::NativeOrExact | Self::NativeOnly)
    }
    pub const fn supports_native_global(self) -> bool {
        matches!(
            self,
            Self::NativeOrExact | Self::NativeOnly | Self::ExactOrNativeGlobal
        )
    }
    pub const fn supports_resolver_native(self) -> bool {
        matches!(self, Self::ResolverNativeOnly)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanItem {
    Update {
        id: PlanItemId,
        candidate: UpdateCandidate,
    },
    Current {
        id: PlanItemId,
        installed: InstalledTool,
    },
    Delayed {
        id: PlanItemId,
        candidate: UpdateCandidate,
        reason: DelayReason,
    },
    Blocked {
        id: PlanItemId,
        seed: UpdateSeed,
        reason: BlockReason,
        policy_warnings: Vec<PolicyWarning>,
        diagnostics: PlanDiagnostics,
    },
    Skipped {
        id: PlanItemId,
        installed: InstalledTool,
        reason: SkipReason,
    },
    ResolverError {
        id: PlanItemId,
        installed: InstalledTool,
        message: String,
    },
}

impl PlanItem {
    pub const fn id(&self) -> &PlanItemId {
        match self {
            Self::Update { id, .. }
            | Self::Current { id, .. }
            | Self::Delayed { id, .. }
            | Self::Blocked { id, .. }
            | Self::Skipped { id, .. }
            | Self::ResolverError { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePlan {
    pub manager_id: ManagerId,
    pub items: Vec<PlanItem>,
    pub issues: Vec<PlanIssue>,
}

impl UpdatePlan {
    /// Creates an update plan for a manager.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::DuplicatePlanItemId`] when two items share an id.
    pub fn new(manager_id: ManagerId, items: Vec<PlanItem>) -> Result<Self, DomainError> {
        Self::with_issues(manager_id, items, Vec::new())
    }

    /// Creates an update plan with manager-level issues.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::DuplicatePlanItemId`] when two items share an id.
    pub fn with_issues(
        manager_id: ManagerId,
        items: Vec<PlanItem>,
        issues: Vec<PlanIssue>,
    ) -> Result<Self, DomainError> {
        let mut seen = Vec::new();
        for item in &items {
            if seen.contains(item.id()) {
                return Err(DomainError::DuplicatePlanItemId(
                    item.id().as_str().to_owned(),
                ));
            }
            seen.push(item.id().clone());
        }

        Ok(Self {
            manager_id,
            items,
            issues,
        })
    }
    pub fn contains_item(&self, id: &PlanItemId) -> bool {
        self.items.iter().any(|item| item.id() == id)
    }
    pub fn item(&self, id: &PlanItemId) -> Option<&PlanItem> {
        self.items.iter().find(|item| item.id() == id)
    }
    pub fn contains_package(&self, package_name: &PackageName) -> bool {
        self.items
            .iter()
            .any(|item| item.package_name() == package_name)
    }
}

impl PlanItem {
    pub const fn package_name(&self) -> &PackageName {
        match self {
            Self::Update { candidate, .. } | Self::Delayed { candidate, .. } => {
                candidate.package_name()
            }
            Self::Current { installed, .. }
            | Self::Skipped { installed, .. }
            | Self::ResolverError { installed, .. } => &installed.package_name,
            Self::Blocked { seed, .. } => &seed.installed.package_name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelayReason {
    ReleaseTooFresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockReason {
    MissingReleaseMetadata,
    ReleaseLookupFailed,
    VersionPolicy(PolicyBlockReason),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlanDiagnostics {
    pub required_age: Duration,
    pub candidates: Vec<CandidateEvaluationFact>,
    pub selected_target: Option<CandidateAgeFact>,
    pub latest_overall: Option<CandidateAgeFact>,
    pub latest_policy_eligible: Option<CandidateAgeFact>,
    pub latest_age_eligible: Option<CandidateAgeFact>,
    pub missing_metadata: Option<MissingMetadataKind>,
    pub lookup_failure: Option<ReleaseLookupError>,
    pub advisory_latest: Option<AdvisoryLatestFact>,
}

impl PlanDiagnostics {
    pub fn new(required_age: Duration) -> Self {
        Self {
            required_age,
            ..Self::default()
        }
    }
    pub const fn with_missing_metadata(mut self, missing_metadata: MissingMetadataKind) -> Self {
        self.missing_metadata = Some(missing_metadata);
        self
    }
    pub fn with_lookup_failure(mut self, lookup_failure: ReleaseLookupError) -> Self {
        self.lookup_failure = Some(lookup_failure);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateAgeFact {
    pub version: VersionText,
    pub age: Duration,
    pub age_source: CandidateAgeSource,
}

impl CandidateAgeFact {
    pub const fn new(version: VersionText, age: Duration, age_source: CandidateAgeSource) -> Self {
        Self {
            version,
            age,
            age_source,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateAgeSource {
    ReleaseTimeline,
    PublishedAt,
    ManagerNativeTimestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateEvaluationFact {
    pub version: VersionText,
    pub age: Option<Duration>,
    pub policy_allowed: bool,
    pub age_allowed: bool,
    pub policy_block_reason: Option<PolicyBlockReason>,
    pub policy_warning: Option<PolicyWarning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingMetadataKind {
    ReleaseTimeline,
    DiscoveredTarget,
    SelectedTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvisoryLatestFact {
    Known {
        latest_version: VersionText,
        candidates: Vec<CandidateAgeFact>,
    },
    MissingMetadata {
        latest_version: VersionText,
    },
    LookupFailed {
        latest_version: VersionText,
        error: ReleaseLookupError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanIssue {
    DiscoveryFailed {
        detail: String,
    },
    UnsupportedManagerVersion {
        installed_version: VersionText,
        reason: UnsupportedReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyBlockReason {
    PreReleaseBlocked,
    TrackRegression,
    UnknownStability,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    Pinned,
    ManagerRule(String),
}
