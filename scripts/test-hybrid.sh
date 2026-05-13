#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

UPNOW_SKIP_MUTATING_COMMANDS=0 \
UPNOW_REQUIRE_MUTATION_MODE=real \
cargo test --workspace -- --ignored --nocapture
