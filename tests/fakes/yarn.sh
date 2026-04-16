#!/usr/bin/env bash
set -euo pipefail

scenario_dir="${UPNOW_FAKE_YARN_SCENARIO_DIR:-}"
if [[ -z "${scenario_dir}" ]]; then
  echo "fake yarn: UPNOW_FAKE_YARN_SCENARIO_DIR is required" >&2
  exit 70
fi

if [[ "$#" -eq 1 && "$1" == "--version" ]]; then
  if [[ -f "${scenario_dir}/version.txt" ]]; then
    cat "${scenario_dir}/version.txt"
  else
    echo "1.22.22"
  fi
  exit 0
fi

if [[ "$#" -eq 4 && "$1" == "global" && "$2" == "list" && "$3" == "--depth=0" && "$4" == "--json" ]]; then
  cat "${scenario_dir}/global-list.jsonl"
  exit 0
fi

if [[ "$#" -eq 4 && "$1" == "info" && "$3" == "time" && "$4" == "--json" ]]; then
  pkg="$2"

  if [[ "${UPNOW_FAKE_YARN_REAL_INFO:-0}" == "1" ]]; then
    real_bin="${UPNOW_REAL_YARN_BIN:-yarn}"
    exec "${real_bin}" "$@"
  fi

  file="${scenario_dir}/time/${pkg}.jsonl"
  if [[ ! -f "${file}" ]]; then
    echo "fake yarn: missing fixture for package '${pkg}' at ${file}" >&2
    exit 66
  fi

  cat "${file}"
  exit 0
fi

if [[ "$#" -eq 3 && "$1" == "global" && "$2" == "add" ]]; then
  # apply paths are skipped by default via SKIP_MUTATING_COMMANDS, but keep
  # a harmless fallback for explicit local runs with that guard disabled.
  echo '{}'
  exit 0
fi

echo "fake yarn: unsupported args: $*" >&2
exit 64
