#!/usr/bin/env bash
set -euo pipefail

# Lightweight profiler scaffold for upnow.
# Usage examples:
#   scripts/profile.sh
#   scripts/profile.sh --runs 8 --warmup 2 -- --dry-run --no-update
#   scripts/profile.sh --compare-parallel -- --dry-run --no-update

RUNS=6
WARMUP=1
COMPARE_PARALLEL=false
EXTRA_ARGS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --runs)
      RUNS="$2"
      shift 2
      ;;
    --warmup)
      WARMUP="$2"
      shift 2
      ;;
    --compare-parallel)
      COMPARE_PARALLEL=true
      shift
      ;;
    --)
      shift
      EXTRA_ARGS=("$@")
      break
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

if ! command -v hyperfine >/dev/null 2>&1; then
  echo "error: hyperfine not found. Install with: brew install hyperfine" >&2
  exit 1
fi

echo "==> Building release binary"
cargo build --release

BIN="target/release/upnow"
DEFAULT_ARGS=(--dry-run --no-update)

if [[ ${#EXTRA_ARGS[@]} -eq 0 ]]; then
  ARGS=("${DEFAULT_ARGS[@]}")
else
  ARGS=("${EXTRA_ARGS[@]}")
fi

mkdir -p .perf
STAMP="$(date +%Y%m%d-%H%M%S)"

if [[ "$COMPARE_PARALLEL" == "true" ]]; then
  echo "==> Comparing max parallel checks"
  hyperfine \
    --runs "$RUNS" \
    --warmup "$WARMUP" \
    --export-json ".perf/hyperfine-${STAMP}.json" \
    --command-name "p=1"   "$BIN --max-parallel-checks 1 ${ARGS[*]}" \
    --command-name "p=2"   "$BIN --max-parallel-checks 2 ${ARGS[*]}" \
    --command-name "p=4"   "$BIN --max-parallel-checks 4 ${ARGS[*]}" \
    --command-name "p=6"   "$BIN --max-parallel-checks 6 ${ARGS[*]}" \
    --command-name "p=8"   "$BIN --max-parallel-checks 8 ${ARGS[*]}" \
    --command-name "p=12"  "$BIN --max-parallel-checks 12 ${ARGS[*]}"
else
  echo "==> Single profile run-set"
  hyperfine \
    --runs "$RUNS" \
    --warmup "$WARMUP" \
    --export-json ".perf/hyperfine-${STAMP}.json" \
    "$BIN ${ARGS[*]}"
fi

echo
echo "Saved: .perf/hyperfine-${STAMP}.json"
