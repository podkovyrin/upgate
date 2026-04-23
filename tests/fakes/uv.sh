#!/usr/bin/env bash
set -euo pipefail

scenario_dir="${UPNOW_FAKE_UV_SCENARIO_DIR:-}"
if [[ -z "${scenario_dir}" ]]; then
  echo "fake uv: UPNOW_FAKE_UV_SCENARIO_DIR is required" >&2
  exit 70
fi

tool_dir="${UPNOW_FAKE_UV_TOOL_DIR:-}"
if [[ -z "${tool_dir}" ]]; then
  echo "fake uv: UPNOW_FAKE_UV_TOOL_DIR is required" >&2
  exit 70
fi

if [[ "$#" -eq 2 && "$1" == "tool" && "$2" == "dir" ]]; then
  printf '%s\n' "${tool_dir}"
  exit 0
fi

if [[ "$#" -eq 3 && "$1" == "tool" && "$2" == "list" && "$3" == "--show-version-specifiers" ]]; then
  cat "${scenario_dir}/tool-list-show.txt"
  exit 0
fi

if [[ "$#" -eq 3 && "$1" == "tool" && "$2" == "list" && "$3" == "--outdated" ]]; then
  # Intentionally empty latest map to keep tests deterministic without PyPI lookups.
  if [[ -f "${scenario_dir}/tool-list-outdated.txt" ]]; then
    cat "${scenario_dir}/tool-list-outdated.txt"
  fi
  exit 0
fi

if [[ "$#" -ge 9 && "$1" == "pip" && "$2" == "install" && "$3" == "--dry-run" ]]; then
  requirement="${@: -1}"
  tool="${requirement%%>=*}"

  if [[ "${UPNOW_FAKE_UV_REAL_PIP_DRY_RUN:-0}" == "1" ]]; then
    keep_fake=",${UPNOW_FAKE_UV_KEEP_FAKE_TOOLS:-},"
    if [[ "${keep_fake}" != *",${tool},"* ]]; then
      real_uv="${UPNOW_REAL_UV_BIN:-uv}"
      exec "${real_uv}" "$@"
    fi
  fi

  plan_file="${scenario_dir}/pip-plan/${tool}.txt"
  exit_file="${scenario_dir}/pip-plan/${tool}.exit"

  if [[ -f "${plan_file}" ]]; then
    cat "${plan_file}" >&2
  else
    echo "fake uv: missing pip plan for '${tool}' at ${plan_file}" >&2
    exit 66
  fi

  if [[ -f "${exit_file}" ]]; then
    code="$(tr -d '[:space:]' < "${exit_file}")"
    exit "${code:-1}"
  fi

  exit 0
fi

if [[ "$#" -eq 6 && "$1" == "tool" && "$2" == "install" && "$3" == "--upgrade" && "$4" == "--exclude-newer" ]]; then
  # apply paths are skipped by default via SKIP_MUTATING_COMMANDS, but keep
  # a harmless fallback for explicit local runs with that guard disabled.
  echo '{}'
  exit 0
fi

if [[ "$#" -eq 4 && "$1" == "tool" && "$2" == "install" && "$3" == "--upgrade" ]]; then
  # apply paths are skipped by default via SKIP_MUTATING_COMMANDS, but keep
  # a harmless fallback for explicit local runs with that guard disabled.
  echo '{}'
  exit 0
fi

echo "fake uv: unsupported args: $*" >&2
exit 64
