#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -lt 8 || "$1" != "-C" || "$3" != "log" ]]; then
  echo "fake git: unsupported args: $*" >&2
  exit 64
fi

# Expected shape:
# git -C <repo> log -1 --format=%ct <ref> -- <source_path>
source_path="${@: -1}"

if [[ "${UPNOW_FAKE_GIT_REAL_LOG:-0}" == "1" ]]; then
  keep_fake=",${UPNOW_FAKE_GIT_KEEP_FAKE_PATHS:-},"
  if [[ "${keep_fake}" != *",${source_path},"* ]]; then
    real_git="${UPNOW_REAL_GIT_BIN:-git}"
    exec "${real_git}" "$@"
  fi
fi

case "$source_path" in
  Formula/alpha-ready.rb)
    echo "1000000000"
    exit 0
    ;;
  Formula/beta-fresh-latest.rb)
    # Future timestamp => saturating_sub => 0s age => delayed.
    echo "9999999999"
    exit 0
    ;;
  Formula/pinned-pkg.rb)
    echo "1000000000"
    exit 0
    ;;
  Formula/omega-error.rb)
    # Invalid timestamp to force local git failure and no remote fallback.
    echo "not-a-timestamp"
    exit 0
    ;;
  Formula/jq.rb)
    # Future timestamp => saturating_sub => 0s age => delayed.
    echo "9999999999"
    exit 0
    ;;
  Formula/zzzz-upnow-no-such-formula-000000000000.rb)
    # Invalid timestamp to force local git failure in hybrid tests.
    echo "not-a-timestamp"
    exit 0
    ;;
esac

echo "1000000000"
exit 0
