# Implementation Principles

This file documents implementation-level guidance and rationale.

`docs/spec.md` is the behavior/source-of-truth document.

## 1) Core direction

- Keep manager logic primarily manager-native and easy to reason about.
- Prefer small focused shared utilities over large abstraction frameworks.
- Avoid centralized cross-manager policy engines unless usage clearly demands it.

## 2) Plan/apply determinism posture

- `plan` is advisory.
- `apply` may differ slightly from `plan` when manager-native commands resolve targets at execution time.
  - Example: `npm -g update --min-release-age <days-from-config>` can shift due to time drift.
- Strict decision-time freeze is intentionally not implemented.

## 3) Lightweight structured outcomes (implemented)

Internal per-item shape includes statuses:
- `update`, `delayed`, `skipped`, `error`

Reason examples:
- `too_fresh`
- `pinned`
- `missing_metadata`
- `command_failed`

This supports consistent text rendering and future aggregation/machine output.

## 4) Lightweight shared helpers (implemented)

Current shared utility modules:
- `src/util/process.rs` — subprocess execution + common failure formatting
- `src/util/timefmt.rs` — human age formatting
- `src/util/timeparse.rs` — RFC3339 -> unix seconds parsing
- `src/util/durationparse.rs` — CLI duration parsing
- `src/util/parallel.rs` — indexed internal parallel job execution helpers

These are intentionally narrow and avoid introducing framework-style coupling.

## 5) Exit code contract (current)

Current implementation behavior:
- `0`: run completed without fatal manager-level error (can include handled per-item errors)
- `1`: fatal manager/runtime error bubbled to `main`
- CLI parse/usage errors are clap-managed (typically `2`)

Previously discussed explicit `130`/signal mapping is not yet implemented in dedicated handling.

## 6) Testing posture

- Prefer small semantic/unit tests for parsers and decision logic.
- Avoid large snapshot/golden sprawl.
- Keep fixtures and test strings hand-curated/minimal.

## 7) Deferred / open

- Cross-manager parallel apply orchestration and dependency ordering policy
- Optional summary/aggregation output lines
- Optional structured log sinks for crash forensics
