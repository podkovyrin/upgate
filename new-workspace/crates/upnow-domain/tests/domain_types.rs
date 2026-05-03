use upnow_domain::{
    DomainError, InstalledTool, ManagerId, ManagerMetadata, ManagerMetadataField,
    ManagerMetadataKey, ManagerMetadataValue, PackageName, ToolId, ToolName, VersionText,
};

#[test]
fn identity_and_version_constructors_reject_empty_values() {
    assert_eq!(ManagerId::new(" "), Err(DomainError::EmptyManagerId));
    assert_eq!(ToolId::new(""), Err(DomainError::EmptyToolId));
    assert_eq!(PackageName::new("\t"), Err(DomainError::EmptyPackageName));
    assert_eq!(ToolName::new("\n"), Err(DomainError::EmptyToolName));
    assert_eq!(VersionText::new(""), Err(DomainError::EmptyVersionText));
}

#[test]
fn installed_tool_preserves_typed_identity() {
    let installed = InstalledTool::new(
        ManagerId::new("pnpm").expect("valid manager id"),
        ToolId::new("pnpm:alpha-ready").expect("valid tool id"),
        PackageName::new("alpha-ready").expect("valid package name"),
        ToolName::new("alpha-ready").expect("valid tool name"),
        VersionText::new("1.0.0").expect("valid version"),
        ManagerMetadata::new(vec![ManagerMetadataField::new(
            ManagerMetadataKey::new("install_flags").expect("valid metadata key"),
            ManagerMetadataValue::List(vec!["--features=fast".to_owned()]),
        )]),
    );

    assert_eq!(installed.manager_id.as_str(), "pnpm");
    assert_eq!(installed.tool_id.as_str(), "pnpm:alpha-ready");
    assert_eq!(installed.package_name.as_str(), "alpha-ready");
    assert_eq!(installed.tool_name.as_str(), "alpha-ready");
    assert_eq!(installed.installed_version.as_str(), "1.0.0");
    assert_eq!(installed.metadata.fields.len(), 1);
}

#[test]
fn metadata_keys_reject_empty_values() {
    assert_eq!(
        ManagerMetadataKey::new(" "),
        Err(DomainError::EmptyMetadataKey)
    );
}
