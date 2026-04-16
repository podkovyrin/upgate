#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ge 3 && "$1" == "version" && "$2" == "-m" ]]; then
  bin_path="$3"
  bin_name="$(basename "$bin_path")"

  if [[ "${UPNOW_FAKE_GO_HYBRID:-0}" == "1" ]]; then
    case "$bin_name" in
      alpha-ready)
        cat <<'OUT'
path	rsc.io/quote
mod	rsc.io/quote	v1.0.0	h1:alpha
OUT
        exit 0
        ;;
      pinned-pkg)
        cat <<'OUT'
path	github.com/spf13/cobra/cobra
mod	github.com/spf13/cobra	v1.0.0	h1:pinned
OUT
        exit 0
        ;;
      gamma-delayed)
        cat <<'OUT'
path	golang.org/x/tools/cmd/stringer
mod	golang.org/x/tools	v9999.0.0	h1:gamma
OUT
        exit 0
        ;;
      omega-error)
        cat <<'OUT'
path	zzzz-upnow-no-such-module-000000000000/cmd/nope
mod	zzzz-upnow-no-such-module-000000000000	v1.0.0	h1:omega
OUT
        exit 0
        ;;
    esac
  fi

  case "$bin_name" in
    alpha-ready)
      cat <<'OUT'
path	example.com/alpha/cmd/alpha-ready
mod	example.com/alpha	v1.0.0	h1:alpha
OUT
      exit 0
      ;;
    beta-fresh-latest)
      cat <<'OUT'
path	example.com/beta/cmd/beta-fresh-latest
mod	example.com/beta	v1.0.0	h1:beta
OUT
      exit 0
      ;;
    gamma-delayed)
      cat <<'OUT'
path	example.com/gamma/cmd/gamma-delayed
mod	example.com/gamma	v2.0.0	h1:gamma
OUT
      exit 0
      ;;
    omega-error)
      cat <<'OUT'
path	example.com/omega/cmd/omega-error
mod	example.com/omega	v0.1.0	h1:omega
OUT
      exit 0
      ;;
    pinned-pkg)
      cat <<'OUT'
path	example.com/pinned/cmd/pinned-pkg
mod	example.com/pinned	v3.0.0	h1:pinned
OUT
      exit 0
      ;;
    scan-noage)
      cat <<'OUT'
path	example.com/scan/cmd/scan-noage
mod	example.com/scan	v5.0.0	h1:scan
OUT
      exit 0
      ;;
    skip-nometa)
      echo "binary has no module metadata" >&2
      exit 1
      ;;
  esac

  echo "unknown fake go binary: ${bin_name}" >&2
  exit 1
fi

if [[ "$#" -eq 5 && "$1" == "list" && "$2" == "-m" && "$3" == "-json" && "$4" == "-versions" ]]; then
  module="$5"

  if [[ "${UPNOW_FAKE_GO_REAL_LIST:-0}" == "1" ]]; then
    real_bin="${UPNOW_REAL_GO_BIN:-go}"
    exec "${real_bin}" "$@"
  fi

  case "$module" in
    example.com/alpha)
      echo '{"Versions":["v1.0.0","v1.2.0"]}'
      exit 0
      ;;
    example.com/beta)
      echo '{"Versions":["v1.0.0","v1.0.5","v1.1.0"]}'
      exit 0
      ;;
    example.com/gamma)
      echo '{"Versions":["v1.9.0","v2.1.0"]}'
      exit 0
      ;;
    example.com/omega)
      echo "failed to list versions for ${module}" >&2
      exit 1
      ;;
    example.com/pinned)
      echo '{"Versions":["v3.0.0","v3.1.0"]}'
      exit 0
      ;;
    example.com/scan)
      echo '{"Versions":["v4.9.0","v5.1.0"]}'
      exit 0
      ;;
  esac

  echo '{"Versions":[]}'
  exit 0
fi

if [[ "$#" -eq 4 && "$1" == "list" && "$2" == "-m" && "$3" == "-json" ]]; then
  spec="$4"

  if [[ "${UPNOW_FAKE_GO_REAL_LIST:-0}" == "1" ]]; then
    real_bin="${UPNOW_REAL_GO_BIN:-go}"
    exec "${real_bin}" "$@"
  fi

  case "$spec" in
    example.com/alpha@v1.0.0)
      echo '{"Time":"2020-01-01T00:00:00Z"}'
      exit 0
      ;;
    example.com/alpha@v1.2.0)
      echo '{"Time":"2021-01-01T00:00:00Z"}'
      exit 0
      ;;
    example.com/beta@v1.0.0)
      echo '{"Time":"2020-01-01T00:00:00Z"}'
      exit 0
      ;;
    example.com/beta@v1.0.5)
      echo '{"Time":"2021-01-01T00:00:00Z"}'
      exit 0
      ;;
    example.com/beta@v1.1.0)
      echo '{"Time":"2099-01-01T00:00:00Z"}'
      exit 0
      ;;
    example.com/gamma@v1.9.0)
      echo '{"Time":"2020-01-01T00:00:00Z"}'
      exit 0
      ;;
    example.com/gamma@v2.0.0)
      echo '{"Time":"2020-06-01T00:00:00Z"}'
      exit 0
      ;;
    example.com/gamma@v2.1.0)
      echo '{"Time":"2099-01-01T00:00:00Z"}'
      exit 0
      ;;
    example.com/omega@v0.1.0)
      echo '{"Time":"2020-01-01T00:00:00Z"}'
      exit 0
      ;;
    example.com/pinned@v3.0.0)
      echo '{"Time":"2020-01-01T00:00:00Z"}'
      exit 0
      ;;
    example.com/pinned@v3.1.0)
      echo '{"Time":"2021-01-01T00:00:00Z"}'
      exit 0
      ;;
    example.com/scan@v5.0.0)
      echo '{}'
      exit 0
      ;;
    example.com/scan@v4.9.0)
      echo '{"Time":"2020-01-01T00:00:00Z"}'
      exit 0
      ;;
    example.com/scan@v5.1.0)
      echo '{"Time":"2099-01-01T00:00:00Z"}'
      exit 0
      ;;
  esac

  echo '{}'
  exit 0
fi

if [[ "$#" -eq 2 && "$1" == "install" ]]; then
  # apply paths are skipped by default via SKIP_MUTATING_COMMANDS, but keep
  # a harmless fallback for explicit local runs with that guard disabled.
  echo '{}'
  exit 0
fi

if [[ "$#" -eq 3 && "$1" == "env" && "$2" == "-json" && "$3" == "GOPATH" ]]; then
  # Should not be needed in tests because GOBIN is set, but keep safe fallback.
  echo '{"GOPATH":"/tmp/fake-gopath"}'
  exit 0
fi

echo "fake go: unsupported args: $*" >&2
exit 64
