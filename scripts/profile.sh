#!/usr/bin/env bash
set -euo pipefail

# Lightweight profiler scaffold for upnow.
#
# Examples:
#   scripts/profile.sh
#   scripts/profile.sh --label baseline --compare-parallel -- -S brew.no_update=true
#   scripts/profile.sh --runs 10 --warmup 2 --parallel-values 1,2,4,6,8 -- -S brew.no_update=true

RUNS=6
WARMUP=1
COMPARE_PARALLEL=false
PARALLEL_VALUES=(1 2 4 6 8 12)
RESULTS_ROOT=".perf/runs"
LABEL=""
EXTRA_ARGS=()

usage() {
  cat <<'EOF'
Usage: scripts/profile.sh [options] [-- <upnow args...>]

Options:
  --runs <n>               Number of benchmark runs (default: 6)
  --warmup <n>             Number of warmup runs (default: 1)
  --compare-parallel       Run matrix over --max-parallel-checks-per-manager values
  --parallel-values <csv>  Comma-separated values for compare mode (default: 1,2,4,6,8,12)
  --results-root <path>    Root folder for benchmark artifacts (default: .perf/runs)
  --label <name>           Optional label appended to run id
  -h, --help               Show this help

If no extra upnow args are provided, defaults to:
  -S brew.no_update=true
EOF
}

is_non_negative_int() {
  [[ "$1" =~ ^[0-9]+$ ]]
}

parse_parallel_values() {
  local raw="$1"
  local -a parsed=()

  IFS=',' read -r -a parsed <<< "$raw"
  if [[ ${#parsed[@]} -eq 0 ]]; then
    echo "error: --parallel-values cannot be empty" >&2
    exit 1
  fi

  PARALLEL_VALUES=()
  for value in "${parsed[@]}"; do
    value="${value//[[:space:]]/}"
    if ! is_non_negative_int "$value" || [[ "$value" == "0" ]]; then
      echo "error: invalid parallel value '$value' (expected positive integer)" >&2
      exit 1
    fi
    PARALLEL_VALUES+=("$value")
  done
}

sanitize_label() {
  local raw="$1"
  # Keep it shell/path friendly.
  raw="${raw// /-}"
  raw="${raw//[^a-zA-Z0-9._-]/-}"
  printf '%s' "$raw"
}

build_command_string() {
  local -a cmd=("$@")
  local out=""
  local token

  for token in "${cmd[@]}"; do
    if [[ -n "$out" ]]; then
      out+=" "
    fi
    out+="$(printf '%q' "$token")"
  done

  printf '%s' "$out"
}

contains_arg() {
  local needle="$1"
  shift
  local token
  for token in "$@"; do
    if [[ "$token" == "$needle" ]]; then
      return 0
    fi
  done
  return 1
}

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
    --parallel-values)
      parse_parallel_values "$2"
      shift 2
      ;;
    --results-root)
      RESULTS_ROOT="$2"
      shift 2
      ;;
    --label)
      LABEL="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      EXTRA_ARGS=("$@")
      break
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if ! is_non_negative_int "$RUNS" || [[ "$RUNS" == "0" ]]; then
  echo "error: --runs must be a positive integer" >&2
  exit 1
fi

if ! is_non_negative_int "$WARMUP"; then
  echo "error: --warmup must be a non-negative integer" >&2
  exit 1
fi

if ! command -v hyperfine >/dev/null 2>&1; then
  echo "error: hyperfine not found. Install with: brew install hyperfine" >&2
  exit 1
fi

echo "==> Building release binary"
cargo build --release

BIN="target/release/upnow"
DEFAULT_ARGS=(-S brew.no_update=true)

if [[ ${#EXTRA_ARGS[@]} -eq 0 ]]; then
  ARGS=("${DEFAULT_ARGS[@]}")
else
  ARGS=("${EXTRA_ARGS[@]}")
fi

if [[ "$COMPARE_PARALLEL" == "true" ]] && contains_arg "--max-parallel-checks-per-manager" "${ARGS[@]}"; then
  echo "error: do not pass --max-parallel-checks-per-manager explicitly with --compare-parallel" >&2
  exit 1
fi

mkdir -p .perf
mkdir -p "$RESULTS_ROOT"

STAMP="$(date +%Y%m%d-%H%M%S)"
SHORT_SHA="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
FULL_SHA="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
BRANCH="$(git branch --show-current 2>/dev/null || echo unknown)"
DIRTY="false"
if [[ -n "$(git status --porcelain 2>/dev/null || true)" ]]; then
  DIRTY="true"
fi

RUN_ID="${STAMP}-${SHORT_SHA}"
if [[ -n "$LABEL" ]]; then
  RUN_ID+="-$(sanitize_label "$LABEL")"
fi

RUN_DIR="${RESULTS_ROOT}/${RUN_ID}"
mkdir -p "$RUN_DIR"

JSON_OUT="${RUN_DIR}/hyperfine.json"
CSV_OUT="${RUN_DIR}/hyperfine.csv"
MD_OUT="${RUN_DIR}/hyperfine.md"
TXT_OUT="${RUN_DIR}/hyperfine.txt"
META_OUT="${RUN_DIR}/meta.env"
ARGS_OUT="${RUN_DIR}/upnow-args.txt"
SYSTEM_OUT="${RUN_DIR}/system.txt"

printf '%s\n' "${ARGS[@]}" > "$ARGS_OUT"

PARALLEL_VALUES_JOINED="$(IFS=,; echo "${PARALLEL_VALUES[*]}")"
{
  echo "run_id=${RUN_ID}"
  echo "timestamp=$(date -Iseconds)"
  echo "git_commit=${FULL_SHA}"
  echo "git_branch=${BRANCH}"
  echo "git_dirty=${DIRTY}"
  echo "runs=${RUNS}"
  echo "warmup=${WARMUP}"
  echo "compare_parallel=${COMPARE_PARALLEL}"
  echo "parallel_values=${PARALLEL_VALUES_JOINED}"
  echo -n "upnow_args="
  for arg in "${ARGS[@]}"; do
    printf '%q ' "$arg"
  done
  echo
} > "$META_OUT"

{
  uname -a || true
  rustc --version || true
  cargo --version || true
} > "$SYSTEM_OUT"

HF_ARGS=(
  --runs "$RUNS"
  --warmup "$WARMUP"
  --export-json "$JSON_OUT"
  --export-csv "$CSV_OUT"
  --export-markdown "$MD_OUT"
)

if [[ "$COMPARE_PARALLEL" == "true" ]]; then
  echo "==> Comparing --max-parallel-checks-per-manager values: ${PARALLEL_VALUES_JOINED}"

  for parallel in "${PARALLEL_VALUES[@]}"; do
    cmd="$(build_command_string "$BIN" --max-parallel-checks-per-manager "$parallel" "${ARGS[@]}")"
    HF_ARGS+=(--command-name "checks=${parallel}" "$cmd")
  done
else
  echo "==> Single profile run-set"
  cmd="$(build_command_string "$BIN" "${ARGS[@]}")"
  HF_ARGS+=("$cmd")
fi

echo "==> Run ID: ${RUN_ID}"
echo "==> Artifacts: ${RUN_DIR}"

hyperfine "${HF_ARGS[@]}" 2>&1 | tee "$TXT_OUT"

INDEX_FILE=".perf/index.tsv"
if [[ ! -f "$INDEX_FILE" ]]; then
  printf 'run_id\ttimestamp\tbranch\tcommit\tdirty\tcompare_parallel\truns\twarmup\tparallel_values\trun_dir\n' > "$INDEX_FILE"
fi
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
  "$RUN_ID" \
  "$(date -Iseconds)" \
  "$BRANCH" \
  "$SHORT_SHA" \
  "$DIRTY" \
  "$COMPARE_PARALLEL" \
  "$RUNS" \
  "$WARMUP" \
  "$PARALLEL_VALUES_JOINED" \
  "$RUN_DIR" >> "$INDEX_FILE"

ln -sfn "$RUN_ID" "${RESULTS_ROOT}/latest"

echo
echo "Saved artifacts:"
echo "  ${JSON_OUT}"
echo "  ${CSV_OUT}"
echo "  ${MD_OUT}"
echo "  ${TXT_OUT}"
echo "  ${META_OUT}"
echo "  ${INDEX_FILE}"
