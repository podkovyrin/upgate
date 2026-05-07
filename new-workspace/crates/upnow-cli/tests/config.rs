use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use upnow_cli::config::{ConfigError, ManagerMode, PIN_ALL, UpnowConfig};
use upnow_domain::{PackageName, VersionPolicy};
use upnow_managers::adapter::ManagerDefaultMode;
use upnow_managers::registry::available_managers;

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

    let npm = config.resolve_manager("npm").expect("npm should resolve");
    assert_eq!(npm.mode, ManagerMode::Apply);
    assert_eq!(npm.min_release_age, Duration::from_secs(7 * 24 * 60 * 60));
    assert_eq!(npm.version_policy, VersionPolicy::None);
    assert!(npm.pinned.is_empty());
}

#[test]
fn manager_defaults_cover_brew_gem_and_dotnet() {
    let config = UpnowConfig::default();

    let brew = config.resolve_manager("brew").expect("brew should resolve");
    assert_eq!(brew.mode, ManagerMode::Apply);
    assert_eq!(brew.min_release_age, Duration::from_secs(12 * 60 * 60));
    assert!(!brew.no_update);

    let gem = config.resolve_manager("gem").expect("gem should resolve");
    assert_eq!(gem.mode, ManagerMode::Off);
    assert_eq!(gem.min_release_age, Duration::from_secs(7 * 24 * 60 * 60));

    let dotnet = config
        .resolve_manager("dotnet")
        .expect("dotnet should resolve");
    assert_eq!(dotnet.mode, ManagerMode::Off);
}

#[test]
fn config_resolves_every_registered_manager_default() {
    let config = UpnowConfig::default();

    for manager in available_managers() {
        let resolved = config
            .resolve_manager(manager.id())
            .expect("registered manager should resolve from adapter defaults");
        assert_eq!(resolved.manager_id.as_str(), manager.id());
        assert_eq!(resolved.min_release_age, manager.defaults().min_release_age);
        assert_eq!(
            resolved.mode,
            match manager.defaults().mode {
                ManagerDefaultMode::Off => ManagerMode::Off,
                ManagerDefaultMode::Apply => ManagerMode::Apply,
            }
        );
    }
}

#[test]
fn file_values_resolve_to_typed_manager_config() {
    let path = temp_config_path("file-values");
    write_config(
        &path,
        r#"
[upnow]
scan_old_age_threshold = "30d"

[brew]
mode = "plan"
min_release_age = "12h"
version_policy = "stable"
no_update = true
pinned = ["aom", "*"]
"#,
    );

    let config = UpnowConfig::load_from_path(&path).expect("config should load");
    assert_eq!(
        config
            .scan_old_age_threshold()
            .expect("scan threshold should parse"),
        Duration::from_secs(30 * 24 * 60 * 60)
    );

    let brew = config.resolve_manager("brew").expect("brew should resolve");
    assert_eq!(brew.mode, ManagerMode::Plan);
    assert_eq!(brew.version_policy, VersionPolicy::Stable);
    assert!(brew.no_update);
    assert!(
        brew.pinned
            .contains(&PackageName::new("aom").expect("valid package name"))
    );
    assert!(
        brew.pinned
            .contains(&PackageName::new(PIN_ALL).expect("valid package name"))
    );
}

#[test]
fn selected_manager_override_sets_apply_mode() {
    let mut config: UpnowConfig =
        toml::from_str("[gem]\nmode = \"off\"\n").expect("inline config should be valid");

    config
        .apply_selected_managers_cli_override(&["gem"])
        .expect("selected manager should override mode");

    let gem = config.resolve_manager("gem").expect("gem should resolve");
    assert_eq!(gem.mode, ManagerMode::Apply);
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
fn cli_overrides_parse_supported_values() {
    let mut config = UpnowConfig::default();

    config
        .apply_cli_override("upnow.scan_old_age_threshold=14d")
        .expect("global override should apply");
    config
        .apply_cli_override("brew.no_update=true")
        .expect("brew override should apply");
    config
        .apply_cli_override("npm.min_release_age=10d")
        .expect("min age override should apply");
    config
        .apply_cli_override("npm.version_policy=same-track")
        .expect("policy override should apply");

    assert_eq!(
        config
            .scan_old_age_threshold()
            .expect("scan age should resolve"),
        Duration::from_secs(14 * 24 * 60 * 60)
    );

    let brew = config.resolve_manager("brew").expect("brew should resolve");
    assert!(brew.no_update);

    let npm = config.resolve_manager("npm").expect("npm should resolve");
    assert_eq!(npm.min_release_age, Duration::from_secs(10 * 24 * 60 * 60));
    assert_eq!(npm.version_policy, VersionPolicy::SameTrack);
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
        config.apply_cli_override("npm.pinned=typescript"),
        Err(ConfigError::PinnedOverrideNotSupported(_))
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
}

#[test]
fn cli_overrides_reject_invalid_duration_values_immediately() {
    let mut config = UpnowConfig::default();

    assert!(matches!(
        config.apply_cli_override("upnow.scan_old_age_threshold=7w"),
        Err(ConfigError::InvalidDurationUnit { key, value, unit })
            if key == "[upnow].scan_old_age_threshold" && value == "7w" && unit == "w"
    ));
    assert!(matches!(
        config.apply_cli_override("npm.min_release_age=12h"),
        Err(ConfigError::InvalidDuration { key, value })
            if key == "[npm].min_release_age" && value == "12h"
    ));
    assert!(matches!(
        config.apply_cli_override("brew.min_release_age=soon"),
        Err(ConfigError::InvalidDuration { key, value })
            if key == "[brew].min_release_age" && value == "soon"
    ));
}

#[test]
fn pin_persistence_preserves_unrelated_toml_and_writes_only_one_manager() {
    let path = temp_config_path("persist-pins");
    write_config(
        &path,
        r#"
[upnow]
scan_old_age_threshold = "30d"

[npm]
mode = "apply"
pinned = ["old"]

[brew]
no_update = true
pinned = ["aom"]
"#,
    );
    let mut config = UpnowConfig::load_from_path(&path).expect("config should load");
    let pins = BTreeSet::from([
        PackageName::new("typescript").expect("valid package"),
        PackageName::new("vite").expect("valid package"),
    ]);

    config
        .set_manager_pins("npm", pins)
        .expect("pins should be set");
    config
        .persist_manager_pins_to_path("npm", &path)
        .expect("pins should persist");

    let raw = std::fs::read_to_string(&path).expect("config should be readable");
    assert!(raw.contains("scan_old_age_threshold = \"30d\""));
    assert!(raw.contains("no_update = true"));
    assert!(raw.contains("pinned = [\"aom\"]"));

    let value: toml::Value = toml::from_str(&raw).expect("persisted TOML should parse");
    let npm_pins = value["npm"]["pinned"]
        .as_array()
        .expect("npm pins should be array")
        .iter()
        .map(|value| value.as_str().expect("pin should be string"))
        .collect::<Vec<_>>();
    assert_eq!(npm_pins, vec!["typescript", "vite"]);
}

#[test]
fn pin_persistence_removes_empty_pin_array_only_for_target_manager() {
    let path = temp_config_path("remove-pins");
    write_config(
        &path,
        r#"
[npm]
pinned = ["old"]

[brew]
pinned = ["aom"]
"#,
    );
    let mut config = UpnowConfig::load_from_path(&path).expect("config should load");

    config
        .set_manager_pins("npm", BTreeSet::new())
        .expect("empty pins should be set");
    config
        .persist_manager_pins_to_path("npm", &path)
        .expect("pins should persist");

    let value: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).expect("config should be readable"))
            .expect("persisted TOML should parse");
    assert!(value["npm"].get("pinned").is_none());
    assert_eq!(value["brew"]["pinned"][0].as_str(), Some("aom"));
}
