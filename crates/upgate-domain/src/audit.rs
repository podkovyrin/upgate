use std::fmt::{self, Display};

use crate::{DomainError, VersionText};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OsvEcosystem {
    Npm,
    CratesIo,
    Pypi,
    RubyGems,
    Go,
    NuGet,
    Git,
}

impl OsvEcosystem {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::CratesIo => "crates.io",
            Self::Pypi => "PyPI",
            Self::RubyGems => "RubyGems",
            Self::Go => "Go",
            Self::NuGet => "NuGet",
            Self::Git => "GIT",
        }
    }
}

impl Display for OsvEcosystem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuditPackageName(String);

impl AuditPackageName {
    /// Creates an audit package name.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::EmptyAuditPackageName`] when the value is blank.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DomainError::EmptyAuditPackageName);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for AuditPackageName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuditSubject {
    pub ecosystem: OsvEcosystem,
    pub package_name: AuditPackageName,
}

impl AuditSubject {
    pub const fn new(ecosystem: OsvEcosystem, package_name: AuditPackageName) -> Self {
        Self {
            ecosystem,
            package_name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuditQuery {
    pub subject: AuditSubject,
    pub version: VersionText,
}

impl AuditQuery {
    pub const fn new(subject: AuditSubject, version: VersionText) -> Self {
        Self { subject, version }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditLookupResult {
    Clean,
    Vulnerable { findings: Vec<AuditFinding> },
    LookupFailed { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditFinding {
    pub id: String,
    pub aliases: Vec<String>,
    pub summary: Option<String>,
    pub severity: Option<String>,
    pub references: Vec<String>,
}
