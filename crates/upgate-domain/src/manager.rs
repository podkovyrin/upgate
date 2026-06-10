use std::fmt::{self, Display};

use crate::{AuditSubject, DomainError, VersionText};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// Manager-level shortcuts that are safe only when the selected plan matches
/// the manager-wide command semantics.
///
/// Per-item execution support lives on update candidates; these flags do not
/// mean every individual item can use every command shape.
pub struct ManagerCapabilities {
    pub native_global_update: bool,
    pub resolver_native_global_update: bool,
}

impl ManagerCapabilities {
    pub const fn new() -> Self {
        Self {
            native_global_update: false,
            resolver_native_global_update: false,
        }
    }
    pub const fn with_native_global_update(mut self, native_global_update: bool) -> Self {
        self.native_global_update = native_global_update;
        self
    }
    pub const fn with_resolver_native_global_update(
        mut self,
        resolver_native_global_update: bool,
    ) -> Self {
        self.resolver_native_global_update = resolver_native_global_update;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ManagerId(String);

impl ManagerId {
    /// Creates a manager id from a static manager-owned id.
    ///
    /// # Panics
    ///
    /// Panics when the static id is blank.
    pub fn from_static(value: &'static str) -> Self {
        Self::new(value).expect("static manager id should be valid")
    }

    /// Creates a manager id.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::EmptyManagerId`] when the value is blank.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DomainError::EmptyManagerId);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ManagerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ToolId(String);

impl ToolId {
    /// Creates a tool id.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::EmptyToolId`] when the value is blank.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DomainError::EmptyToolId);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ToolId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageName(String);

impl PackageName {
    /// Creates a package name.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::EmptyPackageName`] when the value is blank.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DomainError::EmptyPackageName);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for PackageName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ToolName(String);

impl ToolName {
    /// Creates a tool display name.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::EmptyToolName`] when the value is blank.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DomainError::EmptyToolName);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ToolName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledTool {
    pub manager_id: ManagerId,
    pub tool_id: ToolId,
    pub package_name: PackageName,
    pub tool_name: ToolName,
    pub installed_version: VersionText,
    pub audit_subject: Option<AuditSubject>,
}

impl InstalledTool {
    pub const fn new(
        manager_id: ManagerId,
        tool_id: ToolId,
        package_name: PackageName,
        tool_name: ToolName,
        installed_version: VersionText,
    ) -> Self {
        Self {
            manager_id,
            tool_id,
            package_name,
            tool_name,
            installed_version,
            audit_subject: None,
        }
    }
    pub fn with_audit_subject(mut self, audit_subject: AuditSubject) -> Self {
        self.audit_subject = Some(audit_subject);
        self
    }
}
