use std::path::{Path, PathBuf};

use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/managers")
}

fn text(path: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(path)).expect("read text fixture")
}

fn json(path: &str) -> Value {
    serde_json::from_str(&text(path)).expect("fixture should be valid JSON")
}

fn jsonl(path: &str) -> Vec<Value> {
    text(path)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("fixture line should be valid JSON"))
        .collect()
}

#[test]
fn brew_fixtures_capture_outdated_info_and_tap_metadata_shapes() {
    let outdated = json("brew/deterministic/outdated.json");
    let formulae = outdated["formulae"]
        .as_array()
        .expect("brew outdated formulae should be an array");
    assert!(formulae.iter().any(|formula| {
        formula["name"].as_str() == Some("alpha-ready")
            && formula["current_version"].as_str() == Some("1.2.0")
            && formula["installed_versions"]
                .as_array()
                .expect("brew installed versions should be an array")
                .iter()
                .any(|version| version.as_str() == Some("1.0.0"))
    }));
    assert!(outdated["casks"].as_array().is_some());

    let installed = json("brew/deterministic/info-installed.json");
    let installed_formulae = installed["formulae"]
        .as_array()
        .expect("brew info formulae should be an array");
    assert!(installed_formulae.iter().any(|formula| {
        formula["full_name"].as_str() == Some("alpha-ready")
            && formula["installed"]
                .as_array()
                .expect("brew installed entries should be an array")
                .iter()
                .any(|entry| entry["installed_on_request"].as_bool() == Some(true))
    }));

    let tap = json("brew/deterministic/tap-info.json");
    let taps = tap.as_array().expect("tap info should be an array");
    assert!(taps.iter().any(|tap| {
        tap["name"].as_str() == Some("local/tap")
            && tap["path"]
                .as_str()
                .is_some_and(|path| !path.trim().is_empty())
            && tap["remote"].is_null()
            && tap["branch"].as_str() == Some("main")
    }));
}

#[test]
fn npm_family_fixtures_capture_installed_outdated_and_registry_time_shapes() {
    let npm_installed = json("npm/deterministic/installed.json");
    assert_eq!(
        npm_installed["dependencies"]["fresh-tool"]["version"].as_str(),
        Some("2.0.0")
    );

    let npm_outdated = json("npm/deterministic/outdated.json");
    assert_eq!(
        npm_outdated["alpha-ready"]["current"].as_str(),
        Some("1.0.0")
    );
    assert_eq!(text("npm/deterministic/outdated.exit").trim(), "1");

    let npm_time = json("npm/deterministic/time/alpha-ready.json");
    assert!(
        npm_time
            .as_object()
            .expect("npm time map")
            .contains_key("1.2.0")
    );

    let pnpm_list = json("pnpm/deterministic/list.json");
    assert_eq!(
        pnpm_list[0]["dependencies"]["pinned-pkg"]["version"].as_str(),
        Some("3.0.0")
    );
    assert_eq!(text("pnpm/deterministic/outdated.exit").trim(), "1");

    let bun_list = json("bun/deterministic/pm-ls.json");
    assert_eq!(
        bun_list["dependencies"]["alpha-ready"]["version"].as_str(),
        Some("1.0.0")
    );

    let yarn_lines = jsonl("yarn/deterministic/global-list.jsonl");
    assert_eq!(yarn_lines[0]["type"].as_str(), Some("tree"));
    assert_eq!(
        yarn_lines[0]["data"]["trees"][0]["name"].as_str(),
        Some("alpha-ready@1.0.0")
    );
}

#[test]
fn cargo_fixtures_capture_install_list_ledger_search_and_crates_payloads() {
    let install_list = text("cargo/deterministic/install-list.txt");
    assert!(install_list.contains("alpha-ready v1.0.0:"));
    assert!(install_list.contains("pinned-pkg v3.0.0:"));

    let ledger = json("cargo/deterministic/.crates2.json");
    let installs = ledger["installs"]
        .as_object()
        .expect("cargo ledger installs");
    let alpha =
        &installs["alpha-ready 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)"];
    assert!(
        alpha["bins"]
            .as_array()
            .expect("cargo ledger bins should be an array")
            .iter()
            .any(|bin| bin.as_str() == Some("alpha-ready"))
    );
    assert!(
        alpha["features"]
            .as_array()
            .expect("cargo ledger features should be an array")
            .iter()
            .any(|feature| feature.as_str() == Some("fast-mode"))
    );
    assert_eq!(alpha["no_default_features"].as_bool(), Some(true));

    let search = text("cargo/deterministic/search/alpha-ready.txt");
    assert!(search.contains("alpha-ready = \"1.2.0\""));

    let crate_payload = json("cargo/deterministic/crates/alpha-ready.json");
    assert!(
        crate_payload["versions"]
            .as_array()
            .expect("crate versions should be an array")
            .iter()
            .any(|version| version["num"].as_str() == Some("1.2.0"))
    );
}

#[test]
fn python_manager_fixtures_capture_pipx_pypi_and_uv_outputs() {
    let pipx = json("pipx/deterministic/list.json");
    assert_eq!(
        pipx["venvs"]["alpha-ready"]["metadata"]["main_package"]["package_version"].as_str(),
        Some("1.0.0")
    );

    let pypi = json("pipx/deterministic/pypi/alpha-ready.json");
    assert!(
        pypi["releases"]
            .as_object()
            .expect("PyPI releases")
            .contains_key("1.2.0")
    );

    let uv_show = text("uv/deterministic/tool-list-show.txt");
    assert!(uv_show.contains("alpha-ready v1.0.0 [required: ==1.0.0]"));

    let uv_outdated = text("uv/deterministic/tool-list-outdated.txt");
    assert!(uv_outdated.contains("alpha-ready v1.0.0 [latest: 1.2.0]"));
    assert!(uv_outdated.contains("optional-latest-missing v1.1.0"));

    let dry_run = text("uv/deterministic/pip-plan/alpha-ready.txt");
    assert!(dry_run.contains("alpha-ready"));
}

#[test]
fn go_fixtures_capture_version_metadata_and_module_list_shapes() {
    let version = text("go/deterministic/version-m-alpha-ready.txt");
    assert!(version.contains("path\texample.com/alpha/cmd/alpha-ready"));
    assert!(version.contains("mod\texample.com/alpha\tv1.0.0"));

    let versions = json("go/deterministic/list-versions-alpha.json");
    assert!(
        versions["Versions"]
            .as_array()
            .expect("go versions should be an array")
            .iter()
            .any(|version| version.as_str() == Some("v1.2.0"))
    );

    let module_time = json("go/deterministic/list-module-alpha-v1.2.0.json");
    assert_eq!(module_time["Time"].as_str(), Some("2021-01-01T00:00:00Z"));

    let missing_metadata = text("go/deterministic/version-m-skip-nometa.stderr");
    assert!(missing_metadata.contains("binary has no module metadata"));
}

#[test]
fn gem_and_dotnet_fixtures_capture_manager_and_registry_payloads() {
    let gem_list = text("gem/deterministic/list.txt");
    assert!(gem_list.contains("alpha-ready (1.0.0)"));
    assert!(gem_list.contains("default-skip (default: 9.9.9)"));

    let gem_outdated = text("gem/deterministic/outdated.txt");
    assert!(gem_outdated.contains("prerelease-blocked (1.0.0 < 1.1.0-beta.1)"));

    let rubygems = json("gem/deterministic/rubygems/alpha-ready.json");
    assert!(rubygems.as_array().expect("RubyGems versions").len() >= 2);

    let dotnet = json("dotnet/deterministic/tool-list.json");
    assert_eq!(dotnet["version"].as_i64(), Some(1));
    assert!(
        dotnet["data"]
            .as_array()
            .expect("dotnet data should be an array")
            .iter()
            .any(|tool| tool["packageId"].as_str() == Some("alpha-ready"))
    );

    let nuget_index = json("dotnet/deterministic/nuget/alpha-ready.index.json");
    assert!(
        nuget_index["items"]
            .as_array()
            .expect("NuGet registration index")
            .len()
            >= 1
    );
}

#[test]
fn mise_fixtures_capture_dry_run_registry_ls_remote_and_versions_host_shapes() {
    let dry_run = text("mise/deterministic/upgrade-dry-run.txt");
    assert!(dry_run.contains("Would uninstall npm:alpha-ready@1.0.0"));
    assert!(dry_run.contains("Would install npm:alpha-ready@1.2.0"));

    let outdated = json("mise/deterministic/outdated.json");
    assert_eq!(
        outdated["npm:alpha-ready"]["latest"].as_str(),
        Some("1.2.0")
    );

    let registry = json("mise/deterministic/registry/node.json");
    assert_eq!(registry["short"].as_str(), Some("node"));
    assert!(
        registry["backends"]
            .as_array()
            .expect("mise registry backends should be an array")
            .iter()
            .any(|backend| backend.as_str() == Some("core:node"))
    );

    let ls_remote = json("mise/deterministic/ls-remote/node.json");
    assert!(
        ls_remote
            .as_array()
            .expect("mise ls-remote should be an array")
            .iter()
            .any(|version| version["version"].as_str() == Some("20.1.0"))
    );

    let versions_host = text("mise/deterministic/versions/tools/emsdk.toml");
    assert!(versions_host.contains("[versions]"));
    assert!(versions_host.contains("created_at"));
}
