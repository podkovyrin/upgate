use std::time::SystemTime;

use crate::VersionText;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTimeline {
    pub versions: Vec<ReleaseEntry>,
}

impl ReleaseTimeline {
    pub const fn new(versions: Vec<ReleaseEntry>) -> Self {
        Self { versions }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseEntry {
    pub version: VersionText,
    pub published_at: ReleaseTimestamp,
}

impl ReleaseEntry {
    pub const fn new(version: VersionText, published_at: ReleaseTimestamp) -> Self {
        Self {
            version,
            published_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTimestamp(SystemTime);

impl ReleaseTimestamp {
    pub const fn new(value: SystemTime) -> Self {
        Self(value)
    }
    pub const fn as_system_time(&self) -> &SystemTime {
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
    pub const fn timestamp(&self) -> &ReleaseTimestamp {
        match self {
            Self::PublishedAt(timestamp) | Self::ManagerNativeTimestamp(timestamp) => timestamp,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseEvidenceSource {
    ReleaseTimeline,
    PublishedAt,
    ManagerNativeTimestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionReleaseEvidence {
    pub version: VersionText,
    pub published_at: ReleaseTimestamp,
    pub source: ReleaseEvidenceSource,
}

impl VersionReleaseEvidence {
    pub const fn new(
        version: VersionText,
        published_at: ReleaseTimestamp,
        source: ReleaseEvidenceSource,
    ) -> Self {
        Self {
            version,
            published_at,
            source,
        }
    }

    pub fn from_target_age(version: VersionText, evidence: &TargetAgeEvidence) -> Self {
        let source = match evidence {
            TargetAgeEvidence::PublishedAt(_) => ReleaseEvidenceSource::PublishedAt,
            TargetAgeEvidence::ManagerNativeTimestamp(_) => {
                ReleaseEvidenceSource::ManagerNativeTimestamp
            }
        };
        Self::new(version, evidence.timestamp().clone(), source)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseLookupError {
    pub detail: String,
}

impl ReleaseLookupError {
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}
