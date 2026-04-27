#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeStatus {
    Current,
    Update,
    Delayed,
    Skipped,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasonCode {
    Pinned,
    MissingMetadata,
    UnsupportedPlatform,
    MissingCommand,
    CommandFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutcomeSubject {
    Manager,
    Item,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutcomeVersions {
    None,
    Current { version: String },
    Change { from: String, to: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeVisibility {
    Always,
    VerboseOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutcomeReason {
    Current(CurrentReason),
    Update(UpdateReason),
    Delayed(DelayedReason),
    Skipped(SkippedReason),
    Error(ErrorReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrentReason {
    NoNewerVersion,
    BlockedByVersionPolicy,
    Scan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateReason {
    Eligible,
    LatestTooFresh,
    LatestBlockedByVersionPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelayedReason {
    TargetTooFresh,
    NoAgeEligibleRelease,
    NoPolicyAndAgeEligibleRelease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkippedReason {
    Pinned,
    MissingCommand,
    UnsupportedPlatform,
    MissingMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorReason {
    CommandFailed,
    ResolverFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutcomeDiagnostics {
    pub release_age: Option<ReleaseAgeDiagnostic>,
    pub latest_too_fresh: Option<AgeGateDiagnostic>,
    pub target_too_fresh: Option<AgeGateDiagnostic>,
    pub required_age: Option<String>,
    pub version_policy: Option<VersionPolicyDiagnostic>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAgeDiagnostic {
    pub age: String,
    pub is_old: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgeGateDiagnostic {
    pub version: Option<String>,
    pub age: String,
    pub required_age: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionPolicyDiagnostic {
    pub policy: String,
    pub latest_blocked_version: Option<String>,
    pub warning: Option<String>,
}
