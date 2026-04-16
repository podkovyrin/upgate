#!/usr/bin/env bash
set -euo pipefail

scenario_dir="${UPNOW_FAKE_DOTNET_SCENARIO_DIR:-}"
if [[ -z "${scenario_dir}" ]]; then
  echo "fake dotnet: UPNOW_FAKE_DOTNET_SCENARIO_DIR is required" >&2
  exit 70
fi

if [[ "$#" -eq 5 && "$1" == "tool" && "$2" == "list" && "$3" == "--global" && "$4" == "--format" && "$5" == "json" ]]; then
  cat "${scenario_dir}/tool-list.json"
  exit 0
fi

if [[ "$#" -eq 7 && "$1" == "tool" && "$2" == "update" && "$3" == "--global" && "$5" == "--version" && "$7" == "--allow-downgrade" ]]; then
  # apply paths are skipped by default via SKIP_MUTATING_COMMANDS, but keep
  # a harmless fallback for explicit local runs with that guard disabled.
  echo '{}'
  exit 0
fi

echo "fake dotnet: unsupported args: $*" >&2
exit 64
