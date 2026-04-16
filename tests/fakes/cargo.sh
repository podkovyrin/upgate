#!/usr/bin/env bash
set -euo pipefail

scenario_dir="${UPNOW_FAKE_CARGO_SCENARIO_DIR:-}"
if [[ -z "${scenario_dir}" ]]; then
  echo "fake cargo: UPNOW_FAKE_CARGO_SCENARIO_DIR is required" >&2
  exit 70
fi

if [[ "$#" -eq 2 && "$1" == "install" && "$2" == "--list" ]]; then
  cat "${scenario_dir}/install-list.txt"
  exit 0
fi

if [[ "$#" -eq 4 && "$1" == "search" && "$3" == "--limit" && "$4" == "1" ]]; then
  crate_name="$2"
  file="${scenario_dir}/search/${crate_name}.txt"
  if [[ ! -f "${file}" ]]; then
    echo "fake cargo: missing search fixture for crate '${crate_name}' at ${file}" >&2
    exit 66
  fi

  cat "${file}"
  exit 0
fi

if [[ "$#" -ge 3 && "$1" == "install" && "$2" == "--force" ]]; then
  # apply paths are skipped by default via SKIP_MUTATING_COMMANDS, but keep
  # a harmless fallback for explicit local runs with that guard disabled.
  echo '{}'
  exit 0
fi

echo "fake cargo: unsupported args: $*" >&2
exit 64
