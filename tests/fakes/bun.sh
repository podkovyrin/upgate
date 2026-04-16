#!/usr/bin/env bash
set -euo pipefail

scenario_dir="${UPNOW_FAKE_BUN_SCENARIO_DIR:-}"
if [[ -z "${scenario_dir}" ]]; then
  echo "fake bun: UPNOW_FAKE_BUN_SCENARIO_DIR is required" >&2
  exit 70
fi

if [[ "$#" -eq 4 && "$1" == "pm" && "$2" == "ls" && "$3" == "-g" && "$4" == "--json" ]]; then
  cat "${scenario_dir}/pm-ls.json"
  exit 0
fi

if [[ "$#" -eq 7 && "$1" == "pm" && "$2" == "view" && "$4" == "time" && "$5" == "--json" && "$6" == "--cwd" ]]; then
  pkg="$3"

  if [[ "${UPNOW_FAKE_BUN_REAL_VIEW:-0}" == "1" ]]; then
    real_bin="${UPNOW_REAL_BUN_BIN:-bun}"
    exec "${real_bin}" "$@"
  fi

  file="${scenario_dir}/time/${pkg}.json"
  if [[ ! -f "${file}" ]]; then
    echo "fake bun: missing fixture for package '${pkg}' at ${file}" >&2
    exit 66
  fi

  cat "${file}"

  # For scan coverage: simulate non-zero status for one package while still
  # returning JSON. Plan path ignores status for this command; scan path treats
  # non-zero as age-unavailable.
  if [[ "${pkg}" == "scan-noage" ]]; then
    echo "simulated bun pm view failure for ${pkg}" >&2
    exit 1
  fi

  exit 0
fi

if [[ "$#" -ge 3 && "$1" == "update" && "$2" == "-g" ]]; then
  # apply paths are skipped by default via SKIP_MUTATING_COMMANDS, but keep
  # a harmless fallback for explicit local runs with that guard disabled.
  echo '{}'
  exit 0
fi

echo "fake bun: unsupported args: $*" >&2
exit 64
