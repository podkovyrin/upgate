#!/usr/bin/env bash
set -euo pipefail

scenario_dir="${UPNOW_FAKE_PNPM_SCENARIO_DIR:-}"
if [[ -z "${scenario_dir}" ]]; then
  echo "fake pnpm: UPNOW_FAKE_PNPM_SCENARIO_DIR is required" >&2
  exit 70
fi

if [[ "$#" -eq 5 && "$1" == "list" && "$2" == "-g" && "$3" == "--depth" && "$4" == "0" && "$5" == "--json" ]]; then
  cat "${scenario_dir}/list.json"
  exit 0
fi

if [[ "$#" -eq 3 && "$1" == "outdated" && "$2" == "-g" && "$3" == "--json" ]]; then
  cat "${scenario_dir}/outdated.json"
  if [[ -f "${scenario_dir}/outdated.exit" ]]; then
    code="$(tr -d '[:space:]' < "${scenario_dir}/outdated.exit")"
    exit "${code:-1}"
  fi
  exit 1
fi

if [[ "$#" -eq 4 && "$1" == "view" && "$3" == "time" && "$4" == "--json" ]]; then
  pkg="$2"

  if [[ "${UPNOW_FAKE_PNPM_REAL_VIEW:-0}" == "1" ]]; then
    real_bin="${UPNOW_REAL_PNPM_BIN:-pnpm}"
    exec "${real_bin}" "$@"
  fi

  file="${scenario_dir}/time/${pkg}.json"
  if [[ ! -f "${file}" ]]; then
    echo "fake pnpm: missing fixture for package '${pkg}' at ${file}" >&2
    exit 66
  fi

  cat "${file}"
  exit 0
fi

if [[ "$#" -eq 3 && "$1" == "add" && "$2" == "-g" ]]; then
  # apply paths are skipped by default via SKIP_MUTATING_COMMANDS, but keep
  # a harmless fallback for explicit local runs with that guard disabled.
  echo '{}'
  exit 0
fi

echo "fake pnpm: unsupported args: $*" >&2
exit 64
