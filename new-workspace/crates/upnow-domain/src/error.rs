use std::fmt::{self, Display};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    EmptyManagerId,
    EmptyToolId,
    EmptyPackageName,
    EmptyToolName,
    EmptyVersionText,
    EmptyPlanItemId,
    EmptyMetadataKey,
    InvalidVersionPolicy(String),
    DuplicatePlanItemId(String),
    UnknownPlanItemId(String),
}

impl Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyManagerId => formatter.write_str("manager id cannot be empty"),
            Self::EmptyToolId => formatter.write_str("tool id cannot be empty"),
            Self::EmptyPackageName => formatter.write_str("package name cannot be empty"),
            Self::EmptyToolName => formatter.write_str("tool name cannot be empty"),
            Self::EmptyVersionText => formatter.write_str("version text cannot be empty"),
            Self::EmptyPlanItemId => formatter.write_str("plan item id cannot be empty"),
            Self::EmptyMetadataKey => formatter.write_str("metadata key cannot be empty"),
            Self::InvalidVersionPolicy(value) => {
                write!(formatter, "invalid version policy `{value}`")
            }
            Self::DuplicatePlanItemId(value) => {
                write!(formatter, "duplicate plan item id `{value}`")
            }
            Self::UnknownPlanItemId(value) => write!(formatter, "unknown plan item id `{value}`"),
        }
    }
}

impl std::error::Error for DomainError {}
