#!/usr/bin/env bash
set -euo pipefail

scenario_dir="${UPNOW_FAKE_GEM_SCENARIO_DIR:-}"
if [[ -z "${scenario_dir}" ]]; then
  echo "fake gem: UPNOW_FAKE_GEM_SCENARIO_DIR is required" >&2
  exit 70
fi

if [[ "$#" -eq 1 && "$1" == "list" ]]; then
  cat "${scenario_dir}/list.txt"
  exit 0
fi

if [[ "$#" -eq 1 && "$1" == "outdated" ]]; then
  cat "${scenario_dir}/outdated.txt"
  exit 0
fi

if [[ "$#" -eq 4 && "$1" == "install" && "$3" == "-v" ]]; then
  # apply paths are skipped by default via SKIP_MUTATING_COMMANDS, but keep
  # a harmless fallback for explicit local runs with that guard disabled.
  echo '{}'
  exit 0
fi

echo "fake gem: unsupported args: $*" >&2
exit 64
