# Implementation Principles (Post-clarification)

This document captures implementation guidance agreed after reviewing the previous `upnow-mvp` attempt.

It is intentionally lightweight and does **not** replace `docs/spec.md`.

## 1) Core direction

- Keep manager implementations **self-contained**.
- Prefer simple, manager-native behavior over global abstractions.
- Do not introduce a centralized cross-manager policy engine.

## 2) Plan/apply determinism posture

- `plan` is advisory.
- `apply` may differ slightly from `plan` when manager-native commands resolve targets at execution time.
  - Example: `npm -g update --min-release-age 7` can shift due to time drift.
- We accept this tradeoff for simplicity.
- No strict `decision_time_utc` freeze is required right now.

## 3) Lightweight structured outcomes

Use a small, consistent internal outcome shape per item (no heavy domain modeling):

- Status: `update`, `delayed`, `skipped`, `error`
- Optional reason examples:
  - `too_fresh`
  - `pinned`
  - `missing_metadata`
  - `command_failed`

Goals:
- consistent output across managers
- easy summary counts
- easier future machine-readable output if needed

## 4) Lightweight process runner helper

A tiny shared subprocess helper is recommended (not a framework), to centralize:

- command execution
- common error formatting (command, exit code, stderr)
- optional timeout/env handling
- optional per-command logging

This is purely to remove repetition and improve consistency.

## 5) Exit code contract

- `0`: command completed successfully (including “nothing to do”)
- `1`: operational failure (manager command failure, runtime error)
- `2`: invalid CLI usage/config
- `130`: interrupted (SIGINT)

This is treated as implementation-level behavior and should remain stable across commands.

## 6) Testing posture (pragmatic)

- Prefer small semantic tests for parsing and decision logic.
- Avoid large snapshot/golden test sprawl.
- Keep fixtures minimal and hand-curated.
- If golden tests are used, keep only a few high-value CLI contract cases.

## 7) Explicitly deferred

- Cross-manager parallel apply orchestration
- manager dependency graphing
- broad architecture refactors
- strict reproducibility guarantees between `plan` and `apply`

These can be revisited later if real usage demands them.
