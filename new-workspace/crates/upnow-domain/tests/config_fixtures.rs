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
fn config_fixture_captures_brew_policy_selection_mode_and_no_update() {
    let config = load_current_behavior_config();
    let brew = &config["brew"];

    assert_eq!(brew["mode"].as_str(), Some("apply"));
    assert_eq!(brew["min_release_age"].as_str(), Some("12h"));
    assert_eq!(brew["version_policy"].as_str(), Some("stable"));
    assert_eq!(brew["no_update"].as_bool(), Some(true));
    assert_eq!(brew["selection"]["mode"].as_str(), Some("include"));
    assert_eq!(
        brew["selection"]["except"]
            .as_array()
            .expect("brew selection exceptions should be an array")
            .iter()
            .map(|value| value.as_str().expect("exception should be a string"))
            .collect::<Vec<_>>(),
        vec!["aom", "docker"]
    );
}

#[test]
fn config_fixture_captures_package_manager_modes_policies_and_selection() {
    let config = load_current_behavior_config();

    assert_eq!(config["npm"]["mode"].as_str(), Some("plan"));
    assert_eq!(config["npm"]["version_policy"].as_str(), Some("same-track"));
    assert_eq!(
        config["npm"]["selection"]["except"][0].as_str(),
        Some("npm")
    );

    assert_eq!(config["gem"]["mode"].as_str(), Some("off"));
    assert_eq!(
        config["gem"]["selection"]["except"][0].as_str(),
        Some("bundler")
    );

    assert_eq!(config["dotnet"]["mode"].as_str(), Some("off"));
    assert_eq!(
        config["dotnet"]["selection"]["except"][0].as_str(),
        Some("serilog")
    );
}
