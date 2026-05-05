use crate::{
    DomainError, InstalledTool, ManagerId, PackageName, PolicyWarning, ReleaseLookupResult, ToolId,
    UnsupportedReason, VersionScheme, VersionText,
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

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSeed {
    pub installed: InstalledTool,
    pub discovered_target: VersionText,
    pub version_scheme: VersionScheme,
    pub release_lookup: ReleaseLookupResult,
}

impl UpdateSeed {
    #[must_use]
    pub fn new(
        installed: InstalledTool,
        discovered_target: VersionText,
        version_scheme: VersionScheme,
        release_lookup: ReleaseLookupResult,
    ) -> Self {
        Self {
            installed,
            discovered_target,
            version_scheme,
            release_lookup,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerUpdateInput {
    Seed(UpdateSeed),
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
    pub policy_warnings: Vec<PolicyWarning>,
}

impl UpdateCandidate {
    #[must_use]
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
            policy_warnings: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_policy_warnings(mut self, policy_warnings: Vec<PolicyWarning>) -> Self {
        self.policy_warnings = policy_warnings;
        self
    }

    #[must_use]
    pub fn tool_id(&self) -> &ToolId {
        &self.tool_id
    }

    #[must_use]
    pub fn package_name(&self) -> &PackageName {
        &self.package_name
    }

    #[must_use]
    pub fn installed_version(&self) -> &VersionText {
        &self.installed_version
    }

    #[must_use]
    pub fn target_version(&self) -> &VersionText {
        &self.target_version
    }

    #[must_use]
    pub fn version_scheme(&self) -> VersionScheme {
        self.version_scheme
    }

    #[must_use]
    pub fn execution_eligibility(&self) -> ExecutionEligibility {
        self.execution_eligibility
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionEligibility {
    NativeOrExact,
    ExactOnly,
    NativeOnly,
    NotExecutable,
}

impl ExecutionEligibility {
    #[must_use]
    pub fn supports_execution(self) -> bool {
        !matches!(self, Self::NotExecutable)
    }

    #[must_use]
    pub fn supports_exact_target(self) -> bool {
        matches!(self, Self::NativeOrExact | Self::ExactOnly)
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
    #[must_use]
    pub fn id(&self) -> &PlanItemId {
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

    #[must_use]
    pub fn contains_item(&self, id: &PlanItemId) -> bool {
        self.items.iter().any(|item| item.id() == id)
    }

    #[must_use]
    pub fn item(&self, id: &PlanItemId) -> Option<&PlanItem> {
        self.items.iter().find(|item| item.id() == id)
    }

    #[must_use]
    pub fn contains_package(&self, package_name: &PackageName) -> bool {
        self.items
            .iter()
            .any(|item| item.package_name() == package_name)
    }
}

impl PlanItem {
    #[must_use]
    pub fn package_name(&self) -> &PackageName {
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
    UnsupportedExactExecution,
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
    UnsupportedPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    Pinned,
    ManagerDefault,
    ManagerRule(String),
}
