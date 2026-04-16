#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -eq 2 && "$1" == "-e" && "$2" == "print RUBY_VERSION" ]]; then
  echo -n "3.4.9"
  exit 0
fi

echo "fake ruby: unsupported args: $*" >&2
exit 64
