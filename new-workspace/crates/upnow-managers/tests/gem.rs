use std::path::{Path, PathBuf};

use upnow_domain::{
    ExecutionEligibility, ManagerScanInput, ManagerUpdateInput, PackageName, ReleaseLookupResult,
    TargetSelection, ToolId, UpdateCandidate, UpdateSeed, VersionScheme, VersionText,
};
use upnow_infra::{CommandOutput, Env, HttpClient, HttpResponse, ProcessRunner};
use upnow_managers::adapter::ManagerAdapter;
use upnow_managers::gem::{
    GemManager, exact_command, parse_gem_list, parse_gem_outdated, parse_rubygems_json,
};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/managers/gem")
}

fn text(path: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(path)).expect("fixture should be readable")
}

#[test]
fn parses_gem_list_and_marks_default_gems() {
    let installed = parse_gem_list(&text("deterministic/list.txt")).expect("list should parse");

    assert!(installed.iter().any(|package| {
        package.name.as_str() == "alpha-ready"
            && package.version.as_str() == "1.0.0"
            && !package.is_default
    }));
    assert!(installed.iter().any(|package| {
        package.name.as_str() == "default-skip"
            && package.version.as_str() == "9.9.9"
            && package.is_default
    }));
}

#[test]
fn parses_gem_outdated_current_versions() {
    let outdated =
        parse_gem_outdated(&text("deterministic/outdated.txt")).expect("outdated should parse");

    assert!(outdated.iter().any(|package| {
        package.name.as_str() == "alpha-ready" && package.current.as_str() == "1.0.0"
    }));
    assert!(outdated.iter().any(|package| {
        package.name.as_str() == "default-skip" && package.current.as_str() == "9.9.9"
    }));
}

#[test]
fn rubygems_payload_filters_incompatible_ruby_versions() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let runtime = semver::Version::parse("3.0.0").expect("valid runtime");
    let timeline = parse_rubygems_json(
        &package,
        r#"[
            {"number":"1.0.0","created_at":"2020-01-01T00:00:00Z","ruby_version":">= 2.6"},
            {"number":"2.0.0","created_at":"2021-01-01T00:00:00Z","ruby_version":">= 3.1"}
        ]"#,
        Some(&runtime),
    )
    .expect("compatible payload should parse");

    assert_eq!(timeline.versions.len(), 1);
    assert_eq!(
        timeline.versions[0].version,
        VersionText::new("1.0.0").expect("valid version")
    );
}

#[test]
fn rubygems_payload_preserves_original_legacy_versions() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let timeline = parse_rubygems_json(
        &package,
        r#"[
            {"number":"v1.2.0","created_at":"2020-01-01T00:00:00Z"},
            {"number":"1.3","created_at":"2021-01-01T00:00:00Z"},
            {"number":"1","created_at":"2022-01-01T00:00:00Z"}
        ]"#,
        None,
    )
    .expect("legacy-comparable versions should parse");

    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version.as_str() == "v1.2.0")
    );
    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version.as_str() == "1.3")
    );
    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version.as_str() == "1")
    );
}

#[test]
fn constructs_exact_gem_install_command() {
    let command = exact_command(&candidate());

    assert_eq!(command.display(), "gem install alpha-ready -v 1.2.0");
}

#[test]
fn adapter_release_lookup_applies_ruby_runtime_filter() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let lookup = GemManager
        .release_lookup(
            &ProcessRunner::fake([Ok(CommandOutput::from_parts(success_status(), "3.0.0", ""))]),
            &HttpClient::fake([response(
                "https://rubygems.test/api/v1/versions/alpha-ready.json",
                r#"[
                    {"number":"1.0.0","created_at":"2020-01-01T00:00:00Z","ruby_version":">= 2.6"},
                    {"number":"2.0.0","created_at":"2021-01-01T00:00:00Z","ruby_version":">= 3.1"}
                ]"#,
            )]),
            &Env::fixed([(
                "UPNOW_GEM_RUBYGEMS_BASE_URL".to_owned(),
                "https://rubygems.test".to_owned(),
            )]),
            upnow_managers::adapter::ReleaseLookupSubject::Package(&package),
        )
        .expect("lookup should complete");

    let ReleaseLookupResult::Known(timeline) = lookup else {
        panic!("lookup should be known");
    };
    assert_eq!(timeline.versions.len(), 1);
    assert_eq!(timeline.versions[0].version.as_str(), "1.0.0");
}

#[test]
fn adapter_skips_default_gems_for_scan() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(
        success_status(),
        text("deterministic/list.txt"),
        "",
    ))]);

    let inputs = GemManager
        .scan_inputs(&process, &Env::fixed([]))
        .expect("installed gems should parse");

    assert!(
        inputs
            .iter()
            .any(|input| scan_package_name(input) == Some("alpha-ready"))
    );
    assert!(
        !inputs
            .iter()
            .any(|input| scan_package_name(input) == Some("default-skip"))
    );
}

#[test]
fn adapter_builds_update_inputs_with_ruby_runtime_filter() {
    let base_url = "https://rubygems.test";
    let http = HttpClient::fake([
        response(
            &format!("{base_url}/api/v1/versions/alpha-ready.json"),
            &text("deterministic/rubygems/alpha-ready.json"),
        ),
        response(
            &format!("{base_url}/api/v1/versions/gamma-delayed.json"),
            &text("deterministic/rubygems/gamma-delayed.json"),
        ),
        response(
            &format!("{base_url}/api/v1/versions/omega-error.json"),
            &text("deterministic/rubygems/omega-error.json"),
        ),
        response(
            &format!("{base_url}/api/v1/versions/pinned-pkg.json"),
            &text("deterministic/rubygems/pinned-pkg.json"),
        ),
        response(
            &format!("{base_url}/api/v1/versions/prerelease-blocked.json"),
            &text("deterministic/rubygems/prerelease-blocked.json"),
        ),
    ]);
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/list.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            text("deterministic/outdated.txt"),
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "3.0.0", "")),
    ]);
    let env = Env::fixed([(
        "UPNOW_GEM_RUBYGEMS_BASE_URL".to_owned(),
        base_url.to_owned(),
    )]);

    let inputs = GemManager
        .update_inputs(
            &process,
            &http,
            &env,
            upnow_domain::VersionPolicy::Stable,
            std::time::Duration::from_secs(7 * 86_400),
            true,
        )
        .expect("inputs should build");

    assert_eq!(inputs.len(), 5);
    assert!(
        inputs
            .iter()
            .all(|input| input_package_name(input) != "default-skip")
    );
}

#[test]
fn update_inputs_uses_outdated_current_and_keeps_full_registry_timeline() {
    let base_url = "https://rubygems.test";
    let http = HttpClient::fake([response(
        &format!("{base_url}/api/v1/versions/alpha-ready.json"),
        r#"[
            {"number":"1.3.0","created_at":"2022-01-01T00:00:00Z"},
            {"number":"1.2.0","created_at":"2021-01-01T00:00:00Z"},
            {"number":"1.0.0","created_at":"2020-01-01T00:00:00Z"}
        ]"#,
    )]);
    let process = ProcessRunner::fake([
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready (1.0.0)",
            "",
        )),
        Ok(CommandOutput::from_parts(
            success_status(),
            "alpha-ready (1.0.0 < 1.2.0)",
            "",
        )),
        Ok(CommandOutput::from_parts(success_status(), "3.0.0", "")),
    ]);
    let env = Env::fixed([(
        "UPNOW_GEM_RUBYGEMS_BASE_URL".to_owned(),
        base_url.to_owned(),
    )]);

    let inputs = GemManager
        .update_inputs(
            &process,
            &http,
            &env,
            upnow_domain::VersionPolicy::Stable,
            std::time::Duration::from_secs(7 * 86_400),
            true,
        )
        .expect("inputs should build");
    let seed = only_seed(&inputs);

    let TargetSelection::PlannerSelectable {
        discovered_target,
        release_lookup,
    } = &seed.target_selection
    else {
        panic!("gem should remain planner-selectable");
    };
    assert_eq!(discovered_target.as_str(), "1.0.0");
    let ReleaseLookupResult::Known(timeline) = release_lookup else {
        panic!("lookup should be known");
    };
    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| entry.version.as_str() == "1.3.0")
    );
}

#[test]
fn release_lookup_failure_is_item_scoped() {
    let package = PackageName::new("omega-error").expect("valid package");
    let lookup = GemManager
        .release_lookup(
            &ProcessRunner::fake([Ok(CommandOutput::from_parts(success_status(), "3.0.0", ""))]),
            &HttpClient::fake([response(
                "https://rubygems.test/api/v1/versions/omega-error.json",
                &text("deterministic/rubygems/omega-error.json"),
            )]),
            &Env::fixed([(
                "UPNOW_GEM_RUBYGEMS_BASE_URL".to_owned(),
                "https://rubygems.test".to_owned(),
            )]),
            upnow_managers::adapter::ReleaseLookupSubject::Package(&package),
        )
        .expect("lookup should complete");

    assert!(matches!(lookup, ReleaseLookupResult::MissingMetadata));
}

fn only_seed(inputs: &[ManagerUpdateInput]) -> &UpdateSeed {
    assert_eq!(inputs.len(), 1);
    let ManagerUpdateInput::Seed(seed) = &inputs[0] else {
        panic!("input should be seed");
    };
    seed
}

fn candidate() -> UpdateCandidate {
    UpdateCandidate::new(
        ToolId::new("alpha-ready").expect("valid tool"),
        PackageName::new("alpha-ready").expect("valid package"),
        VersionText::new("1.0.0").expect("valid version"),
        VersionText::new("1.2.0").expect("valid version"),
        VersionScheme::SemVer,
        ExecutionEligibility::ExactOnly,
    )
}

fn response(url: &str, body: &str) -> (String, HttpResponse) {
    (
        url.to_owned(),
        HttpResponse {
            status: 200,
            body: body.to_owned(),
        },
    )
}

fn input_package_name(input: &upnow_domain::ManagerUpdateInput) -> &str {
    match input {
        upnow_domain::ManagerUpdateInput::Seed(seed) => seed.installed.package_name.as_str(),
        upnow_domain::ManagerUpdateInput::Skipped { installed, .. }
        | upnow_domain::ManagerUpdateInput::ResolverError { installed, .. } => {
            installed.package_name.as_str()
        }
    }
}

fn scan_package_name(input: &ManagerScanInput) -> Option<&str> {
    match input {
        ManagerScanInput::Installed(tool) => Some(tool.package_name.as_str()),
        ManagerScanInput::Skipped { .. } => None,
    }
}

#[cfg(unix)]
fn success_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn success_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}
