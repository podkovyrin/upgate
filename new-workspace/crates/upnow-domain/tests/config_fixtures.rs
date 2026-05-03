use std::path::{Path, PathBuf};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config")
}

fn load_current_behavior_config() -> toml::Value {
    let raw = std::fs::read_to_string(fixtures_dir().join("current-behavior.toml"))
        .expect("read current behavior config fixture");
    toml::from_str(&raw).expect("current behavior config fixture should be valid TOML")
}

#[test]
fn config_fixture_captures_global_scan_age_threshold() {
    let config = load_current_behavior_config();

    assert_eq!(
        config["upnow"]["scan_old_age_threshold"].as_str(),
        Some("30d")
    );
}

#[test]
fn config_fixture_captures_brew_policy_pins_mode_and_no_update() {
    let config = load_current_behavior_config();
    let brew = &config["brew"];

    assert_eq!(brew["mode"].as_str(), Some("apply"));
    assert_eq!(brew["min_release_age"].as_str(), Some("12h"));
    assert_eq!(brew["version_policy"].as_str(), Some("stable"));
    assert_eq!(brew["no_update"].as_bool(), Some(true));
    assert_eq!(
        brew["pinned"]
            .as_array()
            .expect("brew pinned should be an array")
            .iter()
            .map(|value| value.as_str().expect("pin should be a string"))
            .collect::<Vec<_>>(),
        vec!["aom", "docker"]
    );
}

#[test]
fn config_fixture_captures_package_manager_modes_policies_and_pins() {
    let config = load_current_behavior_config();

    assert_eq!(config["npm"]["mode"].as_str(), Some("scan"));
    assert_eq!(config["npm"]["version_policy"].as_str(), Some("same-track"));
    assert_eq!(config["npm"]["pinned"][0].as_str(), Some("npm"));

    assert_eq!(config["gem"]["mode"].as_str(), Some("off"));
    assert_eq!(config["gem"]["pinned"][0].as_str(), Some("bundler"));

    assert_eq!(config["dotnet"]["mode"].as_str(), Some("off"));
    assert_eq!(config["dotnet"]["pinned"][0].as_str(), Some("serilog"));
}
