#!/usr/bin/env bash
set -euo pipefail

scenario_dir="${UPNOW_FAKE_MISE_SCENARIO_DIR:-}"
if [[ -z "${scenario_dir}" ]]; then
  echo "fake mise: UPNOW_FAKE_MISE_SCENARIO_DIR is required" >&2
  exit 70
fi

if [[ "$#" -eq 4 && "$1" == "upgrade" && "$2" == "--dry-run" && "$3" == "--before" ]]; then
  cat "${scenario_dir}/upgrade-dry-run.txt"
  exit 0
fi

if [[ "$#" -eq 2 && "$1" == "outdated" && "$2" == "--json" ]]; then
  cat "${scenario_dir}/outdated.json"
  exit 0
fi

if [[ "$#" -eq 2 && "$1" == "ls" && "$2" == "--json" ]]; then
  cat "${scenario_dir}/ls.json"
  exit 0
fi

if [[ "$#" -eq 2 && "$1" == "registry" && "$2" == "--json" ]]; then
  printf '['
  first=1
  shopt -s nullglob
  for file in "${scenario_dir}"/registry/*.json; do
    if [[ "${first}" -eq 0 ]]; then
      printf ','
    fi
    cat "${file}"
    first=0
  done
  printf ']\n'
  exit 0
fi

if [[ "$#" -eq 3 && "$1" == "registry" && "$3" == "--json" ]]; then
  tool="$2"
  file="${scenario_dir}/registry/${tool}.json"
  if [[ ! -f "${file}" ]]; then
    echo "fake mise: missing registry fixture for tool '${tool}' at ${file}" >&2
    exit 66
  fi

  cat "${file}"
  exit 0
fi

if [[ "$#" -eq 3 && "$1" == "ls-remote" && "$2" == "--json" ]]; then
  tool="$3"
  file="${scenario_dir}/ls-remote/${tool}.json"
  if [[ ! -f "${file}" ]]; then
    echo "fake mise: missing fixture for tool '${tool}' at ${file}" >&2
    exit 66
  fi

  cat "${file}"
  exit 0
fi

if [[ "$#" -eq 2 && "$1" == "upgrade" ]]; then
  # apply paths are skipped by default via SKIP_MUTATING_COMMANDS, but keep
  # a harmless fallback for explicit local runs with that guard disabled.
  echo '{}'
  exit 0
fi

if [[ "$#" -ge 3 && "$1" == "upgrade" && "$2" == "--before" ]]; then
  # apply paths are skipped by default via SKIP_MUTATING_COMMANDS, but keep
  # a harmless fallback for explicit local runs with that guard disabled.
  echo '{}'
  exit 0
fi

echo "fake mise: unsupported args: $*" >&2
exit 64
