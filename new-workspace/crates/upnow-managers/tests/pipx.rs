use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use upnow_domain::{
    ExecutionEligibility, PackageName, ToolId, UpdateCandidate, VersionScheme, VersionText,
};
use upnow_managers::pipx::{exact_command, parse_list_json, parse_pypi_json};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/managers/pipx")
}

fn text(path: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(path)).expect("fixture should be readable")
}

#[test]
fn parses_pipx_list_json() {
    let installed = parse_list_json(&text("deterministic/list.json")).expect("list should parse");

    assert!(installed.iter().any(|package| {
        package.name.as_str() == "alpha-ready" && package.version.as_str() == "1.0.0"
    }));
    assert!(installed.iter().any(|package| {
        package.name.as_str() == "pinned-pkg" && package.version.as_str() == "3.0.0"
    }));
}

#[test]
fn collapses_duplicate_venvs_by_main_package_name() {
    let installed = parse_list_json(
        r#"{
            "venvs": {
                "alpha-ready": {
                    "metadata": {
                        "main_package": {
                            "package": "alpha-ready",
                            "package_version": "1.0.0"
                        }
                    }
                },
                "alpha-ready-copy": {
                    "metadata": {
                        "main_package": {
                            "package": "alpha-ready",
                            "package_version": "1.0.0"
                        }
                    }
                },
                "beta-ready": {
                    "metadata": {
                        "main_package": {
                            "package": "beta-ready",
                            "package_version": "2.0.0"
                        }
                    }
                }
            }
        }"#,
    )
    .expect("list should parse");

    let names: Vec<_> = installed
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    assert_eq!(names, ["alpha-ready", "beta-ready"]);
}

#[test]
fn constructs_exact_pipx_upgrade_command() {
    let command = exact_command(&candidate());

    assert_eq!(command.display(), "pipx upgrade alpha-ready==1.2.0");
}

#[test]
fn parses_pypi_release_payload() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline = parse_pypi_json(&package, &text("deterministic/pypi/alpha-ready.json"))
        .expect("PyPI payload should parse");

    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version == VersionText::new("1.2.0").expect("valid version"))
    );
}

#[test]
fn pypi_release_payload_uses_newest_file_timestamp_per_version() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline = parse_pypi_json(
        &package,
        r#"{
            "releases": {
                "1.0.0": [
                    {"upload_time_iso_8601": "2021-01-01T00:00:00Z"},
                    {"upload_time_iso_8601": "2021-01-03T00:00:00Z"},
                    {"upload_time": "2021-01-02T00:00:00"}
                ]
            }
        }"#,
    )
    .expect("PyPI payload should parse");

    let entry = timeline
        .versions
        .iter()
        .find(|entry| entry.version == VersionText::new("1.0.0").expect("valid version"))
        .expect("version should be present");
    assert_eq!(
        *entry.published_at.as_system_time(),
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_609_632_000)
    );
}

#[test]
fn pypi_release_payload_ignores_invalid_release_versions() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline = parse_pypi_json(
        &package,
        r#"{
            "releases": {
                "not a pep440 version": [
                    {"upload_time_iso_8601": "2021-01-01T00:00:00Z"}
                ],
                "1.2.0": [
                    {"upload_time_iso_8601": "2021-01-03T00:00:00Z"}
                ]
            }
        }"#,
    )
    .expect("valid release should keep payload usable");

    assert_eq!(timeline.versions.len(), 1);
    assert_eq!(
        timeline.versions[0].version,
        VersionText::new("1.2.0").expect("valid version")
    );
}

fn candidate() -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new("alpha-ready").expect("valid tool id"),
        PackageName::new("alpha-ready").expect("valid package"),
        VersionText::new("1.0.0").expect("valid version"),
        VersionText::new("1.2.0").expect("valid version"),
        VersionScheme::Pep440,
        ExecutionEligibility::ExactOnly,
    )
}
