#!/usr/bin/env bash
set -euo pipefail

scenario_dir="${UPNOW_FAKE_PIPX_SCENARIO_DIR:-}"
if [[ -z "${scenario_dir}" ]]; then
  echo "fake pipx: UPNOW_FAKE_PIPX_SCENARIO_DIR is required" >&2
  exit 70
fi

if [[ "$#" -eq 2 && "$1" == "list" && "$2" == "--json" ]]; then
  cat "${scenario_dir}/list.json"
  exit 0
fi

if [[ "$#" -eq 2 && "$1" == "upgrade" ]]; then
  # apply paths are skipped by default via SKIP_MUTATING_COMMANDS, but keep
  # a harmless fallback for explicit local runs with that guard disabled.
  echo '{}'
  exit 0
fi

echo "fake pipx: unsupported args: $*" >&2
exit 64
