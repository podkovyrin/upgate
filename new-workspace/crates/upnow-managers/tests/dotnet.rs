use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use flate2::Compression;
use flate2::write::GzEncoder;
use upnow_domain::{
    ExecutionEligibility, ManagerConfig, ManagerId, ManagerMode, PackageName, ReleaseLookupResult,
    ToolId, UpdateCandidate, VersionPolicy, VersionScheme, VersionText,
};
use upnow_infra::{CommandOutput, Env, HttpBytesResponse, HttpClient, ProcessRunner};
use upnow_managers::adapter::{ManagerAdapter, ReleaseLookupSubject};
use upnow_managers::dotnet::{
    DotnetManager, exact_command, parse_nuget_page_json, parse_tool_list_json,
};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/managers/dotnet")
}

fn text(path: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(path)).expect("fixture should be readable")
}

fn dotnet_manager() -> DotnetManager {
    DotnetManager::new(ManagerConfig {
        manager_id: ManagerId::new("dotnet").expect("valid manager id"),
        mode: ManagerMode::Apply,
        min_release_age: Duration::from_secs(7 * 86_400),
        version_policy: VersionPolicy::None,
        no_update: false,
        pinned: BTreeSet::new(),
    })
}

#[test]
fn parses_dotnet_tool_list_json() {
    let packages =
        parse_tool_list_json(&text("deterministic/tool-list.json")).expect("list should parse");

    assert!(packages.iter().any(|package| {
        package.package_id.as_str() == "alpha-ready" && package.version.as_str() == "1.0.0"
    }));
    assert!(packages.iter().any(|package| {
        package.package_id.as_str() == "pinned-pkg" && package.version.as_str() == "3.0.0"
    }));
}

#[test]
fn missing_sdk_is_reported_as_discovery_error() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(
        exit_status(1),
        "",
        "No .NET SDKs were found",
    ))]);

    let err = dotnet_manager()
        .scan_inputs(&process, &Env::fixed([]))
        .expect_err("missing SDK should fail discovery");

    let detail = err.to_string();
    assert!(detail.contains("dotnet tool list --global --format json failed"));
    assert!(detail.contains("No .NET SDKs were found"));
}

#[test]
fn parses_nuget_registration_page_and_skips_unlisted_or_missing_publish_time() {
    let entries = parse_nuget_page_json(
        r#"{
            "items": [
                {"catalogEntry":{"version":"1.0.0","published":"2020-01-01T00:00:00Z","listed":true}},
                {"catalogEntry":{"version":"1.1.0","published":"2021-01-01T00:00:00Z","listed":false}},
                {"catalogEntry":{"version":"1.2.0","listed":true}}
            ]
        }"#,
    )
    .expect("page should parse");

    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].version,
        VersionText::new("1.0.0").expect("valid version")
    );
}

#[test]
fn release_lookup_reads_nuget_registration_pages() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let http = nuget_http("alpha-ready");

    let lookup = dotnet_manager()
        .release_lookup(
            &ProcessRunner::fake([]),
            &http,
            &nuget_env(),
            ReleaseLookupSubject::Package(&package),
        )
        .expect("lookup should complete");

    let ReleaseLookupResult::Known(timeline) = lookup else {
        panic!("expected known timeline");
    };
    assert!(
        timeline
            .versions
            .iter()
            .any(|entry| { entry.version == VersionText::new("1.2.0").expect("valid version") })
    );
}

#[test]
fn release_lookup_decodes_gzipped_semver2_registration_pages() {
    let package = PackageName::new("alpha-ready").expect("valid package");
    let http = nuget_http("alpha-ready");

    let lookup = dotnet_manager()
        .release_lookup(
            &ProcessRunner::fake([]),
            &http,
            &nuget_env(),
            ReleaseLookupSubject::Package(&package),
        )
        .expect("lookup should complete");

    let ReleaseLookupResult::Known(timeline) = lookup else {
        panic!("expected known timeline");
    };
    assert_eq!(
        timeline
            .versions
            .iter()
            .map(|entry| entry.version.as_str())
            .collect::<Vec<_>>(),
        ["1.0.0", "1.2.0"]
    );
}

#[test]
fn release_lookup_failure_is_item_scoped() {
    let package = PackageName::new("omega-error").expect("valid package");
    let http = nuget_http("omega-error");

    let lookup = dotnet_manager()
        .release_lookup(
            &ProcessRunner::fake([]),
            &http,
            &nuget_env(),
            ReleaseLookupSubject::Package(&package),
        )
        .expect("lookup should complete");

    assert!(matches!(lookup, ReleaseLookupResult::LookupFailed(_)));
}

#[test]
fn constructs_exact_dotnet_update_command() {
    let command = exact_command(&candidate());

    assert_eq!(
        command.display(),
        "dotnet tool update --global alpha-ready --version 1.2.0 --allow-downgrade"
    );
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

fn nuget_http(package: &str) -> HttpClient {
    let base = "https://nuget.test";
    let index =
        text(&format!("deterministic/nuget/{package}.index.json")).replace("__BASE__", base);
    let page = text(&format!("deterministic/nuget/{package}.page.json"));
    HttpClient::fake_bytes([
        gzipped_response(
            &format!("{base}/v3/registration5-gz-semver2/{package}/index.json"),
            &index,
        ),
        gzipped_response(
            &format!("{base}/v3/registration5-gz-semver2/{package}/page/1.json"),
            &page,
        ),
    ])
}

fn nuget_env() -> Env {
    Env::fixed([(
        "UPNOW_DOTNET_NUGET_BASE_URL".to_owned(),
        "https://nuget.test".to_owned(),
    )])
}

fn gzipped_response(url: &str, body: &str) -> (String, HttpBytesResponse) {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(body.as_bytes())
        .expect("fixture should gzip");
    (
        url.to_owned(),
        HttpBytesResponse {
            status: 200,
            body: encoder.finish().expect("fixture should gzip"),
        },
    )
}

#[cfg(unix)]
fn exit_status(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(code << 8)
}

#[cfg(windows)]
fn exit_status(code: u32) -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(code)
}
