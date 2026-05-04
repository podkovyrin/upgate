use std::path::{Path, PathBuf};

use upnow_domain::{PackageName, VersionText};
use upnow_managers::npm_family_release::{
    ReleaseParseError, bun_global_cwd_from_values, parse_npm_time_json, parse_yarn_time_jsonl,
};

fn fixtures_dir(manager: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/managers")
        .join(manager)
}

fn text(manager: &str, path: &str) -> String {
    std::fs::read_to_string(fixtures_dir(manager).join(path)).expect("fixture should be readable")
}

#[test]
fn parses_npm_registry_time_map() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline = parse_npm_time_json(
        &package,
        &text("npm", "deterministic/time/alpha-ready.json"),
    )
    .expect("time map");

    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version == VersionText::new("1.2.0").expect("valid version"))
    );
}

#[test]
fn parses_pnpm_registry_time_map() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline = parse_npm_time_json(
        &package,
        &text("pnpm", "deterministic/time/alpha-ready.json"),
    )
    .expect("time map");

    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version == VersionText::new("1.2.0").expect("valid version"))
    );
}

#[test]
fn parses_bun_registry_time_map() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline = parse_npm_time_json(
        &package,
        &text("bun", "deterministic/time/alpha-ready.json"),
    )
    .expect("time map");

    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version == VersionText::new("1.2.0").expect("valid version"))
    );
}

#[test]
fn parses_registry_time_map_skipping_created_and_modified_metadata() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline = parse_npm_time_json(
        &package,
        r#"{
            "created": "2020-01-01T00:00:00.000Z",
            "modified": "2022-01-01T00:00:00.000Z",
            "1.0.0": "2021-01-01T00:00:00.000Z"
        }"#,
    )
    .expect("time map should parse");

    assert_eq!(timeline.versions.len(), 1);
    assert_eq!(
        timeline.versions[0].version,
        VersionText::new("1.0.0").expect("valid version")
    );
}

#[test]
fn empty_time_map_is_missing_metadata_source() {
    let package = PackageName::new("empty").expect("valid package");
    let err = parse_npm_time_json(&package, "{}").expect_err("empty time map should fail");

    assert!(matches!(err, ReleaseParseError::EmptyTimeMap { .. }));
}

#[test]
fn parses_yarn_registry_time_jsonl() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline = parse_yarn_time_jsonl(
        &package,
        &text("yarn", "deterministic/time/alpha-ready.jsonl"),
    )
    .expect("time");

    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version == VersionText::new("1.2.0").expect("valid version"))
    );
}

#[test]
fn empty_or_missing_yarn_inspect_data_is_missing_metadata_source() {
    let package = PackageName::new("empty").expect("valid package");
    let err = parse_yarn_time_jsonl(&package, r#"{"type":"info","data":"ignored"}"#)
        .expect_err("missing inspect data should fail");

    assert!(matches!(err, ReleaseParseError::EmptyTimeMap { .. }));
}

#[test]
fn resolves_bun_global_cwd_from_bun_install_before_home() {
    assert_eq!(
        bun_global_cwd_from_values(Some("/opt/bun"), Some("/home/user")).as_deref(),
        Some("/opt/bun/install/global")
    );
    assert_eq!(
        bun_global_cwd_from_values(None, Some("/home/user")).as_deref(),
        Some("/home/user/.bun/install/global")
    );
}
