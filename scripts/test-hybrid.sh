#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

status=0

run_hybrid_case() {
  local test_target="$1"
  local test_name="$2"
  local -n status_ref="$3"

  UPNOW_SKIP_MUTATING_COMMANDS=0 \
  UPNOW_REQUIRE_MUTATION_MODE=real \
  UPNOW_RUN_HYBRID_TESTS=1 \
    cargo test --test "$test_target" "$test_name" -- --ignored --nocapture || status_ref=$?
}


HYBRID_CASES=(
  "mock_npm_harness hybrid_apply_uses_real_registry_time_data_with_fake_installed_state"
  "mock_pipx_harness hybrid_apply_uses_real_pypi_data_with_fake_installed_state"
  "mock_pnpm_harness hybrid_apply_uses_real_registry_time_data_with_fake_installed_state"
  "mock_yarn_harness hybrid_apply_uses_real_registry_time_data_with_fake_installed_state"
  "mock_bun_harness hybrid_apply_uses_real_registry_time_data_with_fake_installed_state"
  "mock_go_harness hybrid_apply_uses_real_module_data_with_fake_installed_state"
  "mock_mise_harness hybrid_apply_uses_real_npm_time_data_with_fake_mise_state"
  "mock_uv_harness hybrid_apply_uses_real_pypi_resolution_with_fake_installed_state"
  "mock_brew_harness hybrid_apply_uses_real_tap_metadata_and_git_history_with_fake_outdated_state"
  "mock_cargo_harness hybrid_apply_uses_real_crates_io_data_with_fake_installed_state"
  "mock_gem_harness hybrid_apply_uses_real_rubygems_data_with_fake_installed_state"
  "mock_dotnet_harness hybrid_apply_uses_real_nuget_data_with_fake_installed_state"
)

for case in "${HYBRID_CASES[@]}"; do
  # shellcheck disable=SC2086
  run_hybrid_case ${case} status
done

exit "$status"
