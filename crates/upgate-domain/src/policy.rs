use std::fmt::{self, Display};
use std::str::FromStr;

use crate::DomainError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionPolicy {
    None,
    Stable,
    SameTrack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyWarning {
    InstalledTrackUnknownFallbackStable,
}

impl Display for VersionPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::None => "none",
            Self::Stable => "stable",
            Self::SameTrack => "same-track",
        })
    }
}

impl FromStr for VersionPolicy {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "none" => Ok(Self::None),
            "stable" => Ok(Self::Stable),
            "same-track" => Ok(Self::SameTrack),
            other => Err(DomainError::InvalidVersionPolicy(other.to_owned())),
        }
    }
}
