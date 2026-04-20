#!/usr/bin/env bash
set -euo pipefail

cargo +nightly fmt --all -- \
  --config unstable_features=true \
  --config group_imports=StdExternalCrate \
  --config imports_granularity=Module \
  --config comment_width=120 \
  --config format_code_in_doc_comments=true
