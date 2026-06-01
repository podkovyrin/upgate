use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use upnow_cli::config::{ConfigError, UpnowConfig};
use upnow_domain::{
    ManagerMode, PackageName, UpdateSelectionMode, UpdateSelectionPolicy, VersionPolicy,
};

fn temp_config_path(test_name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after UNIX_EPOCH")
        .as_nanos();
    std::env::temp_dir()
        .join("upnow-cli-config-tests")
        .join(format!("{test_name}-{nanos}"))
        .join("config.toml")
}

fn write_config(path: &Path, raw: &str) {
    std::fs::create_dir_all(path.parent().expect("config path should have parent"))
        .expect("create temp config dir");
    std::fs::write(path, raw).expect("write temp config");
}

#[test]
fn missing_config_uses_global_defaults() {
    let path = temp_config_path("missing-config");
    let config = UpnowConfig::load_from_path(&path).expect("missing config should load defaults");

    assert_eq!(
        config
            .scan_old_age_threshold()
            .expect("scan age should resolve"),
        Duration::from_secs(365 * 24 * 60 * 60)
    );
    assert_eq!(
        config
            .manager_concurrency()
            .expect("manager concurrency should resolve"),
        4
    );

    let npm = config.resolve_manager("npm").expect("npm should resolve");
    assert_eq!(npm.mode, ManagerMode::Apply);
    assert_eq!(npm.min_release_age, Duration::from_secs(7 * 24 * 60 * 60));
    assert_eq!(npm.version_policy, VersionPolicy::None);
    assert_eq!(npm.selection, UpdateSelectionPolicy::default());
}

#[test]
fn file_values_resolve_to_typed_manager_config() {
    let path = temp_config_path("file-values");
    write_config(
        &path,
        r#"
[upnow]
scan_old_age_threshold = "30d"
manager_concurrency = 3

[brew]
mode = "plan"
min_release_age = "12h"
version_policy = "stable"
no_update = true

[brew.selection]
mode = "include"
except = ["aom"]
"#,
    );

    let config = UpnowConfig::load_from_path(&path).expect("config should load");
    assert_eq!(
        config
            .scan_old_age_threshold()
            .expect("scan threshold should parse"),
        Duration::from_secs(30 * 24 * 60 * 60)
    );
    assert_eq!(
        config
            .manager_concurrency()
            .expect("manager concurrency should parse"),
        3
    );

    let brew = config.resolve_manager("brew").expect("brew should resolve");
    assert_eq!(brew.mode, ManagerMode::Plan);
    assert_eq!(brew.version_policy, VersionPolicy::Stable);
    assert!(brew.no_update);
    assert_eq!(brew.selection.mode, UpdateSelectionMode::Include);
    assert!(
        brew.selection
            .except
            .contains(&PackageName::new("aom").expect("valid package name"))
    );
}

#[test]
fn config_rejects_old_pinned_key() {
    let path = temp_config_path("old-pinned");
    write_config(&path, "[npm]\npinned = [\"typescript\"]\n");

    assert!(matches!(
        UpnowConfig::load_from_path(&path),
        Err(ConfigError::Toml(_))
    ));
}

#[test]
fn explicit_cli_mode_override_wins_after_selected_manager_override() {
    let mut config = UpnowConfig::default();

    config
        .apply_selected_managers_cli_override(&["gem"])
        .expect("selected manager should override mode");
    config
        .apply_cli_override("gem.mode=plan")
        .expect("explicit override should apply");

    let gem = config.resolve_manager("gem").expect("gem should resolve");
    assert_eq!(gem.mode, ManagerMode::Plan);
}

#[test]
fn default_selection_policy_is_omitted_when_persisted() {
    let path = temp_config_path("persist-default-selection");
    write_config(
        &path,
        r#"
[npm]
mode = "apply"

[npm.selection]
mode = "skip"
"#,
    );
    let mut config = UpnowConfig::load_from_path(&path).expect("config should load");

    config
        .set_manager_selection_policy("npm", UpdateSelectionPolicy::default())
        .expect("selection policy should be set");
    config
        .persist_manager_selection_policy_to_path("npm", &path)
        .expect("selection policy should persist");

    let value: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).expect("config should be readable"))
            .expect("persisted TOML should parse");
    assert!(value["npm"].get("selection").is_none());
}

#[test]
fn config_rejects_removed_any_policy() {
    let config: UpnowConfig =
        toml::from_str("[npm]\nversion_policy = \"any\"\n").expect("TOML should parse");

    assert!(matches!(
        config.resolve_manager("npm"),
        Err(ConfigError::InvalidVersionPolicy {
            manager_id,
            value
        }) if manager_id == "npm" && value == "any"
    ));
}

#[test]
fn config_rejects_unsupported_policy_per_manager() {
    let config: UpnowConfig =
        toml::from_str("[uv]\nversion_policy = \"stable\"\n").expect("TOML should parse");

    assert!(matches!(
        config.resolve_manager("uv"),
        Err(ConfigError::UnsupportedVersionPolicy {
            manager_id,
            policy: VersionPolicy::Stable
        }) if manager_id == "uv"
    ));

    let config: UpnowConfig =
        toml::from_str("[gem]\nversion_policy = \"same-track\"\n").expect("TOML should parse");

    assert!(matches!(
        config.resolve_manager("gem"),
        Err(ConfigError::UnsupportedVersionPolicy {
            manager_id,
            policy: VersionPolicy::SameTrack
        }) if manager_id == "gem"
    ));
}

#[test]
fn config_rejects_invalid_modes_and_npm_subday_age() {
    let config: UpnowConfig =
        toml::from_str("[npm]\nmode = \"scan\"\n").expect("TOML should parse");
    assert!(matches!(
        config.resolve_manager("npm"),
        Err(ConfigError::InvalidMode { manager_id, value })
            if manager_id == "npm" && value == "scan"
    ));

    let config: UpnowConfig =
        toml::from_str("[npm]\nmin_release_age = \"12h\"\n").expect("TOML should parse");
    assert!(matches!(
        config.resolve_manager("npm"),
        Err(ConfigError::InvalidDuration { key, value })
            if key == "[npm].min_release_age" && value == "12h"
    ));
}

#[test]
fn cli_overrides_reject_unknown_and_non_phase_five_values() {
    let mut config = UpnowConfig::default();

    assert!(matches!(
        config.apply_cli_override("unknown.mode=apply"),
        Err(ConfigError::UnknownManager(manager_id)) if manager_id == "unknown"
    ));
    assert!(matches!(
        config.apply_cli_override("npm.selection=typescript"),
        Err(ConfigError::SelectionOverrideNotSupported(_))
    ));
    assert!(matches!(
        config.apply_cli_override("npm.no_update=true"),
        Err(ConfigError::NoUpdateOnlyBrew(_))
    ));
    assert!(matches!(
        config.apply_cli_override("npm.version_policy=any"),
        Err(ConfigError::InvalidVersionPolicy {
            manager_id,
            value
        }) if manager_id == "npm" && value == "any"
    ));
    assert!(matches!(
        config.apply_cli_override("upnow.manager_concurrency=0"),
        Err(ConfigError::InvalidManagerConcurrency { value: 0 })
    ));
}

#[test]
fn selection_persistence_preserves_unrelated_toml_and_writes_only_one_manager() {
    let path = temp_config_path("persist-selection");
    write_config(
        &path,
        r#"
[upnow]
scan_old_age_threshold = "30d"

[npm]
mode = "apply"

[npm.selection]
mode = "include"
except = ["old"]

[brew]
no_update = true

[brew.selection]
mode = "include"
except = ["aom"]
"#,
    );
    let mut config = UpnowConfig::load_from_path(&path).expect("config should load");
    let policy = UpdateSelectionPolicy {
        mode: UpdateSelectionMode::Skip,
        except: [
            PackageName::new("typescript").expect("valid package"),
            PackageName::new("vite").expect("valid package"),
        ]
        .into_iter()
        .collect(),
    };

    config
        .set_manager_selection_policy("npm", policy)
        .expect("selection policy should be set");
    config
        .persist_manager_selection_policy_to_path("npm", &path)
        .expect("selection policy should persist");

    let raw = std::fs::read_to_string(&path).expect("config should be readable");
    assert!(raw.contains("scan_old_age_threshold = \"30d\""));
    assert!(raw.contains("no_update = true"));
    assert!(raw.contains("[brew.selection]"));
    assert!(raw.contains("except = [\"aom\"]"));

    let value: toml::Value = toml::from_str(&raw).expect("persisted TOML should parse");
    assert_eq!(value["npm"]["selection"]["mode"].as_str(), Some("skip"));
    let npm_exceptions = value["npm"]["selection"]["except"]
        .as_array()
        .expect("npm exceptions should be array")
        .iter()
        .map(|value| value.as_str().expect("exception should be string"))
        .collect::<Vec<_>>();
    assert_eq!(npm_exceptions, vec!["typescript", "vite"]);
}
