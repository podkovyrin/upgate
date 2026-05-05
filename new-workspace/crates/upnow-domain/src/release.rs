use std::time::SystemTime;

use crate::VersionText;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTimeline {
    pub versions: Vec<ReleaseEntry>,
}

impl ReleaseTimeline {
    #[must_use]
    pub fn new(versions: Vec<ReleaseEntry>) -> Self {
        Self { versions }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseEntry {
    pub version: VersionText,
    pub published_at: ReleaseTimestamp,
}

impl ReleaseEntry {
    #[must_use]
    pub fn new(version: VersionText, published_at: ReleaseTimestamp) -> Self {
        Self {
            version,
            published_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTimestamp(SystemTime);

impl ReleaseTimestamp {
    #[must_use]
    pub fn new(value: SystemTime) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn as_system_time(&self) -> &SystemTime {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseLookupResult {
    Known(ReleaseTimeline),
    MissingMetadata,
    LookupFailed(ReleaseLookupError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetAgeLookupResult {
    Known(TargetAgeEvidence),
    MissingMetadata,
    LookupFailed(ReleaseLookupError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetAgeEvidence {
    PublishedAt(ReleaseTimestamp),
    ManagerNativeTimestamp(ReleaseTimestamp),
}

impl TargetAgeEvidence {
    #[must_use]
    pub fn timestamp(&self) -> &ReleaseTimestamp {
        match self {
            Self::PublishedAt(timestamp) | Self::ManagerNativeTimestamp(timestamp) => timestamp,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseLookupError {
    pub detail: String,
}

impl ReleaseLookupError {
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}
