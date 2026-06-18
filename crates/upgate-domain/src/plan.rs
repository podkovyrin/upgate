use std::{
    collections::BTreeSet,
    fmt::{self, Display},
    time::Duration,
};

use crate::{
    DomainError, InstalledTool, ManagerId, PackageName, PolicyWarning, ReleaseLookupError,
    ReleaseLookupResult, TargetAgeLookupResult, ToolId, VersionScheme, VersionText,
    audit::AuditLookupResult,
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

impl Display for PlanItemId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Manager-discovered input before shared planning applies policy and age
/// gates.
///
/// Seeds preserve whether the manager supplied a release timeline that planning
/// may choose from, or a manager-selected target that planning may only gate.
pub struct UpdateSeed {
    pub installed: InstalledTool,
    pub version_scheme: VersionScheme,
    pub target_selection: TargetSelection,
    pub execution_support: ExecutionSupport,
    pub execution_target_kind: ExecutionTargetKind,
}

impl UpdateSeed {
    pub const fn new(
        installed: InstalledTool,
        discovered_target: VersionText,
        version_scheme: VersionScheme,
        release_lookup: ReleaseLookupResult,
        execution_support: ExecutionSupport,
    ) -> Self {
        Self {
            installed,
            version_scheme,
            target_selection: TargetSelection::PlannerSelectable {
                discovered_target,
                release_lookup,
            },
            execution_support,
            execution_target_kind: ExecutionTargetKind::Standard,
        }
    }
    pub const fn manager_selected(
        installed: InstalledTool,
        selected_target: ManagerSelectedTarget,
        version_scheme: VersionScheme,
        execution_support: ExecutionSupport,
    ) -> Self {
        Self {
            installed,
            version_scheme,
            target_selection: TargetSelection::ManagerSelected(selected_target),
            execution_support,
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
/// Describes who chose the target that planning evaluates.
pub enum TargetSelection {
    /// Planning may select from release metadata, using `discovered_target` as
    /// the manager-discovered newest target.
    PlannerSelectable {
        discovered_target: VersionText,
        release_lookup: ReleaseLookupResult,
    },
    /// The manager resolver already chose the target; planning may accept,
    /// delay, or block it, but must not replace it with another version.
    ManagerSelected(ManagerSelectedTarget),
}

impl TargetSelection {
    pub const fn target(&self) -> PlannedTargetRef<'_> {
        match self {
            Self::PlannerSelectable {
                discovered_target, ..
            } => PlannedTargetRef::Known(discovered_target),
            Self::ManagerSelected(target) => target.target.as_ref(),
        }
    }
    pub const fn target_version(&self) -> Option<&VersionText> {
        match self.target() {
            PlannedTargetRef::Known(version) => Some(version),
            PlannedTargetRef::ManagerResolved => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Target chosen by a manager-native resolver or outdated snapshot.
///
/// This is authoritative for manager-selected managers such as uv and mise.
/// Advisory metadata may annotate output, but it must not replace this target.
pub struct ManagerSelectedTarget {
    pub target: PlannedTarget,
    pub target_age: TargetAgeLookupResult,
    pub advisory_release_lookup: Option<AdvisoryReleaseLookup>,
    pub advisory_lookup_failure: Option<ReleaseLookupError>,
}

impl ManagerSelectedTarget {
    pub const fn new(target_version: VersionText, target_age: TargetAgeLookupResult) -> Self {
        Self {
            target: PlannedTarget::Known(target_version),
            target_age,
            advisory_release_lookup: None,
            advisory_lookup_failure: None,
        }
    }
    pub const fn target_version(&self) -> Option<&VersionText> {
        match self.target.as_ref() {
            PlannedTargetRef::Known(version) => Some(version),
            PlannedTargetRef::ManagerResolved => None,
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
    pub fn with_advisory_lookup_failure(mut self, failure: ReleaseLookupError) -> Self {
        self.advisory_lookup_failure = Some(failure);
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
/// Candidate facts after a seed has been evaluated into a concrete plan row.
///
/// A candidate describes the planned target and execution support, but the
/// surrounding `PlanItem` determines whether it is currently eligible,
/// delayed, blocked, or selectable only as a forced action.
pub struct UpdateCandidate {
    pub tool_id: ToolId,
    pub package_name: PackageName,
    pub installed_version: VersionText,
    pub target: PlannedTarget,
    pub version_scheme: VersionScheme,
    pub execution_support: ExecutionSupport,
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
        execution_support: ExecutionSupport,
    ) -> Self {
        Self {
            tool_id,
            package_name,
            installed_version,
            target: PlannedTarget::Known(target_version),
            version_scheme,
            execution_support,
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
    pub const fn target(&self) -> PlannedTargetRef<'_> {
        self.target.as_ref()
    }
    pub const fn target_version(&self) -> Option<&VersionText> {
        match self.target.as_ref() {
            PlannedTargetRef::Known(version) => Some(version),
            PlannedTargetRef::ManagerResolved => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedTarget {
    Known(VersionText),
    ManagerResolved,
}

impl PlannedTarget {
    pub const fn as_ref(&self) -> PlannedTargetRef<'_> {
        match self {
            Self::Known(version) => PlannedTargetRef::Known(version),
            Self::ManagerResolved => PlannedTargetRef::ManagerResolved,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedTargetRef<'a> {
    Known(&'a VersionText),
    ManagerResolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools)]
pub struct ExecutionSupport {
    pub exact: bool,
    pub native_selected: bool,
    pub native_global: bool,
    pub grouped_native: bool,
    pub resolver_native_selected: ResolverNativeSupport,
    pub resolver_native_global: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolverNativeSupport {
    pub selected: bool,
    pub min_age_constraint: MinAgeConstraintSupport,
    pub manager_resolved_target: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinAgeConstraintSupport {
    NotApplicable,
    Required,
    Optional,
}

impl ResolverNativeSupport {
    pub const fn none() -> Self {
        Self {
            selected: false,
            min_age_constraint: MinAgeConstraintSupport::NotApplicable,
            manager_resolved_target: false,
        }
    }
    pub const fn selected(
        min_age_constraint: MinAgeConstraintSupport,
        manager_resolved_target: bool,
    ) -> Self {
        Self {
            selected: true,
            min_age_constraint,
            manager_resolved_target,
        }
    }
}

impl ExecutionSupport {
    pub const fn exact_only() -> Self {
        Self {
            exact: true,
            native_selected: false,
            native_global: false,
            grouped_native: false,
            resolver_native_selected: ResolverNativeSupport::none(),
            resolver_native_global: false,
        }
    }
    pub const fn native_or_exact() -> Self {
        Self {
            exact: true,
            native_selected: true,
            native_global: true,
            grouped_native: false,
            resolver_native_selected: ResolverNativeSupport::none(),
            resolver_native_global: false,
        }
    }
    pub const fn exact_or_native_global() -> Self {
        Self {
            exact: true,
            native_selected: false,
            native_global: true,
            grouped_native: false,
            resolver_native_selected: ResolverNativeSupport::none(),
            resolver_native_global: false,
        }
    }
    pub const fn native_only() -> Self {
        Self {
            exact: false,
            native_selected: true,
            native_global: true,
            grouped_native: false,
            resolver_native_selected: ResolverNativeSupport::none(),
            resolver_native_global: false,
        }
    }
    pub const fn grouped_native_only() -> Self {
        Self {
            exact: false,
            native_selected: true,
            native_global: false,
            grouped_native: true,
            resolver_native_selected: ResolverNativeSupport::none(),
            resolver_native_global: false,
        }
    }
    pub const fn resolver_native(
        min_age_constraint: MinAgeConstraintSupport,
        manager_resolved_target: bool,
        resolver_native_global: bool,
    ) -> Self {
        Self {
            exact: false,
            native_selected: false,
            native_global: false,
            grouped_native: false,
            resolver_native_selected: ResolverNativeSupport::selected(
                min_age_constraint,
                manager_resolved_target,
            ),
            resolver_native_global,
        }
    }
    pub const fn supports_exact_target(self) -> bool {
        self.exact
    }
    pub const fn supports_native_target(self) -> bool {
        self.native_selected
    }
    pub const fn supports_manager_resolved_target(self) -> bool {
        self.native_selected || self.resolver_native_selected.manager_resolved_target
    }
    pub const fn supports_age_bypass(self) -> bool {
        self.exact
            || self.grouped_native
            || (self.resolver_native_selected.selected
                && matches!(
                    self.resolver_native_selected.min_age_constraint,
                    MinAgeConstraintSupport::Optional | MinAgeConstraintSupport::NotApplicable
                ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Extra command-shape discriminator for managers whose selected updates need
/// different native command forms.
pub enum ExecutionTargetKind {
    /// Standard package update command with no manager-specific grouping kind.
    Standard,
    /// Homebrew formula update command/group.
    BrewFormula,
    /// Homebrew cask update command/group.
    BrewCask,
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
    pub const fn package_name(&self) -> &PackageName {
        match self {
            Self::Update { candidate, .. } | Self::Delayed { candidate, .. } => {
                &candidate.package_name
            }
            Self::Current { installed, .. }
            | Self::Skipped { installed, .. }
            | Self::ResolverError { installed, .. } => &installed.package_name,
            Self::Blocked { seed, .. } => &seed.installed.package_name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePlan {
    pub manager_id: ManagerId,
    pub items: Vec<PlanItem>,
}

impl UpdatePlan {
    /// Creates an update plan for a manager.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::DuplicatePlanItemId`] when two items share an id.
    pub fn new(manager_id: ManagerId, items: Vec<PlanItem>) -> Result<Self, DomainError> {
        let mut seen = BTreeSet::new();
        for item in &items {
            if !seen.insert(item.id()) {
                return Err(DomainError::DuplicatePlanItemId(item.id().to_string()));
            }
        }

        Ok(Self { manager_id, items })
    }
    pub fn contains_item(&self, id: &PlanItemId) -> bool {
        self.items.iter().any(|item| item.id() == id)
    }
    pub fn item(&self, id: &PlanItemId) -> Option<&PlanItem> {
        self.items.iter().find(|item| item.id() == id)
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
    AuditVulnerable,
    AuditLookupFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// Explanation facts produced during planning.
///
/// Presentation uses these facts for notes/details, and execution uses them to
/// decide whether an exact forced target bypasses the configured age gate.
pub struct PlanDiagnostics {
    pub required_age: Duration,
    pub candidates: Vec<CandidateEvaluationFact>,
    pub selected_target: Option<CandidateAgeFact>,
    pub latest_overall: Option<CandidateAgeFact>,
    pub missing_metadata: Option<MissingMetadataKind>,
    pub lookup_failure: Option<ReleaseLookupError>,
    pub advisory_latest: Option<AdvisoryLatestFact>,
    pub advisory_lookup_failure: Option<ReleaseLookupError>,
    pub audit_blocking_target: Option<AuditLookupResult>,
    pub audit_blocking_candidate: Option<CandidateEvaluationFact>,
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
}

impl CandidateAgeFact {
    pub const fn new(version: VersionText, age: Duration) -> Self {
        Self { version, age }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Per-version evaluation fact retained for details and exact/forced selection.
pub struct CandidateEvaluationFact {
    pub version: VersionText,
    pub age: Option<Duration>,
    pub age_allowed: bool,
    pub policy_block_reason: Option<PolicyBlockReason>,
    pub policy_warning: Option<PolicyWarning>,
    pub audit: Option<AuditLookupResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingMetadataKind {
    ReleaseTimeline,
    DiscoveredTarget,
    SelectedUpdate,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
