#!/usr/bin/env bash
set -euo pipefail

scenario_dir="${UPNOW_FAKE_BREW_SCENARIO_DIR:-}"
if [[ -z "${scenario_dir}" ]]; then
  echo "fake brew: UPNOW_FAKE_BREW_SCENARIO_DIR is required" >&2
  exit 70
fi

real_brew="${UPNOW_REAL_BREW_BIN:-brew}"

if [[ "$#" -eq 2 && "$1" == "outdated" && "$2" == "--json=v2" ]]; then
  cat "${scenario_dir}/outdated.json"
  exit 0
fi

if [[ "$#" -ge 2 && "$1" == "info" && "$2" == "--json=v2" ]]; then
  for arg in "$@"; do
    if [[ "$arg" == "--installed" ]]; then
      cat "${scenario_dir}/info-installed.json"
      exit 0
    fi
  done

  cat "${scenario_dir}/info-plan.json"
  exit 0
fi

if [[ "$#" -eq 3 && "$1" == "tap-info" && "$2" == "--json" && "$3" == "--installed" ]]; then
  if [[ "${UPNOW_FAKE_BREW_REAL_TAP_INFO:-0}" == "1" ]]; then
    exec "${real_brew}" "$@"
  fi

  cat "${scenario_dir}/tap-info.json"
  exit 0
fi

if [[ "$#" -eq 2 && "$1" == "update" && "$2" == "--quiet" ]]; then
  echo '{}'
  exit 0
fi

if [[ "$#" -ge 2 && "$1" == "upgrade" ]]; then
  # apply paths are skipped by default via SKIP_MUTATING_COMMANDS, but keep
  # a harmless fallback for explicit local runs with that guard disabled.
  echo '{}'
  exit 0
fi

echo "fake brew: unsupported args: $*" >&2
exit 64
