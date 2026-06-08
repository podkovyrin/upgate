use std::fmt::{self, Display};

use crate::DomainError;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VersionText(String);

impl VersionText {
    /// Creates version text without interpreting its scheme.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::EmptyVersionText`] when the value is blank.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DomainError::EmptyVersionText);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for VersionText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionScheme {
    SemVer,
    Pep440,
    ManagerNative,
}
