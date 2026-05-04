use std::path::{Path, PathBuf};
use std::time::Duration;

use upnow_domain::{
    DelayReason, ExecutionEligibility, ManagerId, PackageName, PlanItem, PlanItemId, PlanSelection,
    ReleaseLookupResult, SelectedItem, ToolId, UpdateCandidate, UpdatePlan, VersionScheme,
    VersionText,
};
use upnow_managers::bun::{
    BunError, bun_global_cwd_from_values, exact_command, is_missing_global_manifest,
    parse_pm_ls_json, parse_time_json,
};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/managers/bun")
}

fn text(path: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(path)).expect("fixture should be readable")
}

#[test]
fn parses_pm_ls_root_object_shape() {
    let installed = parse_pm_ls_json(&text("deterministic/pm-ls.json")).expect("pm ls");

    assert!(installed.iter().any(|package| {
        package.name.as_str() == "alpha-ready" && package.version.as_str() == "1.0.0"
    }));
}

#[test]
fn parses_pm_ls_array_shape() {
    let raw = r#"[
        {"dependencies": {"npm": {"version": "11.12.0"}}},
        {"dependencies": {"typescript": {"version": "5.9.3"}}}
    ]"#;

    let installed = parse_pm_ls_json(raw).expect("pm ls array should parse");

    assert!(installed.iter().any(|package| {
        package.name.as_str() == "typescript" && package.version.as_str() == "5.9.3"
    }));
}

#[test]
fn detects_missing_global_manifest_messages() {
    assert!(is_missing_global_manifest(
        "error: missing package.json, nothing outdated"
    ));
    assert!(is_missing_global_manifest(
        "error: failed to initialize bun install: MissingPackageJSON"
    ));
    assert!(is_missing_global_manifest(
        "error: No package.json was found for directory '/tmp/x'"
    ));
    assert!(is_missing_global_manifest("error: Lockfile not found"));
}

#[test]
fn parses_registry_time_json() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline =
        parse_time_json(&package, &text("deterministic/time/alpha-ready.json")).expect("time");

    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version == VersionText::new("1.2.0").expect("valid version"))
    );
}

#[test]
fn empty_time_map_is_missing_metadata_source() {
    let package = PackageName::new("empty").expect("valid package");
    let err = parse_time_json(&package, "{}").expect_err("empty time map should fail");

    let lookup = match err {
        BunError::EmptyTimeMap { .. } => ReleaseLookupResult::MissingMetadata,
        other => panic!("unexpected error: {other}"),
    };
    assert!(matches!(lookup, ReleaseLookupResult::MissingMetadata));
}

#[test]
fn resolves_global_cwd_from_bun_install_before_home() {
    assert_eq!(
        bun_global_cwd_from_values(Some("/opt/bun"), Some("/home/user")).as_deref(),
        Some("/opt/bun/install/global")
    );
    assert_eq!(
        bun_global_cwd_from_values(None, Some("/home/user")).as_deref(),
        Some("/home/user/.bun/install/global")
    );
}

#[test]
fn constructs_exact_bun_update_command_with_min_age_seconds() {
    let command = exact_command(&candidate(), Duration::from_secs(604_800), false);

    assert_eq!(
        command.display(),
        "bun update -g alpha-ready@1.2.0 --minimum-release-age 604800"
    );
}

#[test]
fn forced_exact_bun_update_omits_min_age() {
    let command = exact_command(&candidate(), Duration::from_secs(604_800), true);

    assert_eq!(command.display(), "bun update -g alpha-ready@1.2.0");
}

#[allow(dead_code)]
fn delayed_plan() -> UpdatePlan {
    UpdatePlan::new(
        ManagerId::new("bun").expect("valid manager"),
        vec![PlanItem::Delayed {
            id: PlanItemId::new("bun:alpha-ready").expect("valid id"),
            candidate: candidate(),
            reason: DelayReason::ReleaseTooFresh,
        }],
    )
    .expect("valid plan")
}

#[allow(dead_code)]
fn selection(plan: &UpdatePlan) -> PlanSelection {
    PlanSelection::new(
        plan,
        vec![SelectedItem::new(
            PlanItemId::new("bun:alpha-ready").expect("valid id"),
            true,
        )],
        Vec::new(),
    )
    .expect("valid selection")
}

fn candidate() -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new("alpha-ready").expect("valid tool id"),
        PackageName::new("alpha-ready").expect("valid package"),
        VersionText::new("1.0.0").expect("valid version"),
        VersionText::new("1.2.0").expect("valid version"),
        VersionScheme::SemVer,
        ExecutionEligibility::ExactOnly,
    )
}
