#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

# Harness tests manipulate shared fake command state and are flaky under
# intra-binary parallel test execution; default to serialized execution.
: "${RUST_TEST_THREADS:=1}"
export RUST_TEST_THREADS

HARNESS_TESTS=(
  --test mock_npm_harness
  --test mock_yarn_harness
  --test mock_pnpm_harness
  --test mock_bun_harness
  --test mock_go_harness
  --test mock_mise_harness
  --test mock_uv_harness
  --test mock_brew_harness
  --test mock_pipx_harness
  --test mock_cargo_harness
  --test mock_dotnet_harness
  --test mock_gem_harness
)

UPNOW_SKIP_MUTATING_COMMANDS=1 \
UPNOW_REQUIRE_MUTATION_MODE=skip \
cargo test "${HARNESS_TESTS[@]}" -- --nocapture
