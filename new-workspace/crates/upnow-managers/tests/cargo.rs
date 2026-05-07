use std::path::{Path, PathBuf};

use upnow_domain::{
    ExecutionEligibility, PackageName, PlanItemId, ToolId, UpdateCandidate, VersionScheme,
    VersionText,
};
use upnow_execution::{ExecutionCommandIntent, ResolvedExecutionItem, ResolvedExecutionPlan};
use upnow_infra::{Env, ProcessRunner};
use upnow_managers::adapter::{CommandBuildSettings, ManagerAdapter};
use upnow_managers::cargo::{
    CargoInstallMeta, CargoManager, exact_command, parse_crates_io_json, parse_install_ledger,
    parse_install_list, parse_ledger_key_name, parse_search_latest_version,
};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/managers/cargo")
}

fn text(path: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(path)).expect("fixture should be readable")
}

#[test]
fn parses_cargo_install_list() {
    let installed = parse_install_list(&text("deterministic/install-list.txt"))
        .expect("install list should parse");

    assert!(installed.iter().any(|krate| {
        krate.name.as_str() == "alpha-ready" && krate.version.as_str() == "1.0.0"
    }));
    assert!(
        installed.iter().any(|krate| {
            krate.name.as_str() == "pinned-pkg" && krate.version.as_str() == "3.0.0"
        })
    );
}

#[test]
fn parses_cargo_install_ledger_flags() {
    let ledger =
        parse_install_ledger(&text("deterministic/.crates2.json")).expect("ledger should parse");

    let alpha = ledger
        .get("alpha-ready")
        .expect("alpha-ready should be tracked");
    assert_eq!(alpha.bins, ["alpha-ready"]);
    assert_eq!(alpha.features, ["fast-mode", "vendored-ssl"]);
    assert!(!alpha.all_features);
    assert!(alpha.no_default_features);
}

#[test]
fn parses_ledger_key_name() {
    assert_eq!(
        parse_ledger_key_name(
            "cargo-deny 0.19.0 (registry+https://github.com/rust-lang/crates.io-index)"
        )
        .as_deref(),
        Some("cargo-deny")
    );
}

#[test]
fn parses_cargo_search_latest_version() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let version =
        parse_search_latest_version(&package, &text("deterministic/search/alpha-ready.txt"))
            .expect("search result should parse");

    assert_eq!(version.to_string(), "1.2.0");
}

#[test]
fn parses_crates_io_versions_skipping_yanked_entries() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline = parse_crates_io_json(&package, &text("deterministic/crates/alpha-ready.json"))
        .expect("crates.io payload should parse");

    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version == VersionText::new("1.2.0").expect("valid version"))
    );
}

#[test]
fn constructs_exact_install_command_with_preserved_flags() {
    let meta = CargoInstallMeta {
        bins: vec!["alpha-ready".to_owned()],
        features: vec!["fast-mode".to_owned(), "native-tls".to_owned()],
        all_features: false,
        no_default_features: true,
    };
    let command = exact_command(&candidate(), Some(&meta));

    assert_eq!(
        command.display(),
        "cargo install --force --bin alpha-ready --features fast-mode,native-tls --no-default-features alpha-ready@1.2.0"
    );
}

#[test]
fn constructs_exact_install_command_with_all_bins_and_all_features() {
    let meta = CargoInstallMeta {
        bins: vec!["one".to_owned(), "two".to_owned()],
        features: Vec::new(),
        all_features: true,
        no_default_features: false,
    };
    let command = exact_command(&candidate(), Some(&meta));

    assert_eq!(
        command.display(),
        "cargo install --force --bins --all-features alpha-ready@1.2.0"
    );
}

#[test]
fn adapter_preserves_install_flags_from_fake_cargo_home() {
    let temp_dir = std::env::temp_dir().join(format!("upnow-cargo-ledger-{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
    std::fs::write(
        temp_dir.join(".crates2.json"),
        r#"{
            "installs": {
                "alpha-ready 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)": {
                    "bins": ["alpha-ready"],
                    "features": ["fast-mode", "native-tls"],
                    "no_default_features": true
                }
            }
        }"#,
    )
    .expect("ledger should be writable");
    let env = Env::fixed([(
        "CARGO_HOME".to_owned(),
        temp_dir.to_string_lossy().into_owned(),
    )]);
    let plan = ResolvedExecutionPlan {
        intents: vec![ExecutionCommandIntent::Exact(ResolvedExecutionItem {
            plan_item_id: PlanItemId::new("cargo:alpha-ready").expect("valid id"),
            package_name: PackageName::new("alpha-ready").expect("valid package"),
            installed_version: VersionText::new("1.0.0").expect("valid version"),
            target_version: VersionText::new("1.2.0").expect("valid version"),
            execution_eligibility: ExecutionEligibility::ExactOnly,
            execution_target_kind: upnow_domain::ExecutionTargetKind::Standard,
            exact_target_required: true,
            bypass_min_release_age: false,
        })],
    };

    let commands = CargoManager
        .commands_for_execution_plan(
            &ProcessRunner::fake([]),
            &env,
            &plan,
            CommandBuildSettings {
                min_release_age: std::time::Duration::from_secs(86_400),
            },
        )
        .expect("commands should build");

    assert_eq!(
        commands[0].command.display(),
        "cargo install --force --bin alpha-ready --features fast-mode,native-tls --no-default-features alpha-ready@1.2.0"
    );
    let _ = std::fs::remove_dir_all(temp_dir);
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
