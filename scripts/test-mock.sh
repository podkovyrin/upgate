#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

UPNOW_SKIP_MUTATING_COMMANDS=1 \
UPNOW_REQUIRE_MUTATION_MODE=skip \
cargo test --workspace -- --nocapture
