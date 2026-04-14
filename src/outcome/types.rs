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
    TooFresh,
    NoEligibleRelease,
    NoChange,
    Pinned,
    MissingMetadata,
    CommandFailed,
}
