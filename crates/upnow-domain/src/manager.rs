use crate::{DomainError, VersionText};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledTool {
    pub manager_id: ManagerId,
    pub tool_id: ToolId,
    pub package_name: PackageName,
    pub tool_name: ToolName,
    pub installed_version: VersionText,
    pub metadata: ManagerMetadata,
}

impl InstalledTool {
    pub const fn new(
        manager_id: ManagerId,
        tool_id: ToolId,
        package_name: PackageName,
        tool_name: ToolName,
        installed_version: VersionText,
        metadata: ManagerMetadata,
    ) -> Self {
        Self {
            manager_id,
            tool_id,
            package_name,
            tool_name,
            installed_version,
            metadata,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ManagerMetadata {
    pub fields: Vec<ManagerMetadataField>,
}

impl ManagerMetadata {
    pub fn empty() -> Self {
        Self::default()
    }
    pub const fn new(fields: Vec<ManagerMetadataField>) -> Self {
        Self { fields }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerMetadataField {
    pub key: ManagerMetadataKey,
    pub value: ManagerMetadataValue,
}

impl ManagerMetadataField {
    pub const fn new(key: ManagerMetadataKey, value: ManagerMetadataValue) -> Self {
        Self { key, value }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ManagerMetadataKey(String);

impl ManagerMetadataKey {
    /// Creates a manager-owned metadata key.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::EmptyMetadataKey`] when the value is blank.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DomainError::EmptyMetadataKey);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerMetadataValue {
    Bool(bool),
    Text(String),
    List(Vec<String>),
}
