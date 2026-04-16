#!/usr/bin/env bash
set -euo pipefail

scenario_dir="${UPNOW_FAKE_NPM_SCENARIO_DIR:-}"
if [[ -z "${scenario_dir}" ]]; then
  echo "fake npm: UPNOW_FAKE_NPM_SCENARIO_DIR is required" >&2
  exit 70
fi

real_view="${UPNOW_FAKE_NPM_REAL_VIEW:-0}"

if [[ "$#" -eq 3 && "$1" == "outdated" && "$2" == "-g" && "$3" == "--json" ]]; then
  cat "${scenario_dir}/outdated.json"
  if [[ -f "${scenario_dir}/outdated.exit" ]]; then
    code="$(tr -d '[:space:]' < "${scenario_dir}/outdated.exit")"
    exit "${code:-1}"
  fi
  exit 1
fi

if [[ "$#" -eq 4 && "$1" == "ls" && "$2" == "-g" && "$3" == "--depth=0" && "$4" == "--json" ]]; then
  cat "${scenario_dir}/installed.json"
  exit 0
fi

if [[ "$#" -eq 4 && "$1" == "view" && "$3" == "time" && "$4" == "--json" ]]; then
  pkg="$2"
  if [[ "${real_view}" == "1" ]]; then
    real_npm="${UPNOW_REAL_NPM_BIN:-}"
    if [[ -z "${real_npm}" ]]; then
      echo "fake npm: UPNOW_REAL_NPM_BIN is required when UPNOW_FAKE_NPM_REAL_VIEW=1" >&2
      exit 70
    fi
    exec "${real_npm}" "$@"
  fi

  file="${scenario_dir}/time/${pkg}.json"
  if [[ ! -f "${file}" ]]; then
    echo "fake npm: missing fixture for package '${pkg}' at ${file}" >&2
    exit 66
  fi

  cat "${file}"
  exit 0
fi

if [[ "$#" -ge 3 && "$1" == "-g" && "$2" == "update" ]]; then
  # apply paths are skipped by default via SKIP_MUTATING_COMMANDS, but keep
  # a harmless fallback for explicit local runs with that guard disabled.
  echo '{}'
  exit 0
fi

echo "fake npm: unsupported args: $*" >&2
exit 64
