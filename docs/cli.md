# CLI Specification (vNext draft)

This document defines the intended CLI surface for `upnow`.

It complements `docs/spec.md` and focuses only on command-line UX and automation behavior.

## 1. Command model

`upnow` should use explicit subcommands:

- `plan` (safe, non-mutating)
- `apply` (mutating)
- `scan` (optional future command; not required now)

### Default behavior (no subcommand)

Running `upnow` with no subcommand should behave as:

- `upnow plan`

Reason: safe default for humans and automation.

---

## 2. Interactivity posture

- No interactive prompts are expected for `plan` or `apply`.
- Commands should remain non-interactive by design.
- `stdin` is not required for normal flows.

---

## 3. Shared options

These options apply to `plan` and `apply` unless noted otherwise:

- `--managers <list>`
  - comma-separated manager names
  - current values: `brew,npm,mise,pipx,uv`
  - default: `brew`
- `--no-update`
  - skip manager metadata refresh steps where supported
- `--dry-run`
  - mainly meaningful for `apply`
  - if provided on `apply`, behavior is equivalent to planning-only execution
- `--json`
  - output machine-readable JSON to `stdout`
  - intentionally no formal schema guarantee yet
- `-h, --help`
- `--version`

Manager-specific delay knobs remain as currently implemented (for example `--min-release-age` for brew).

---

## 4. Output contract

### Streams

- `stdout`: primary results (text or JSON)
- `stderr`: diagnostics/errors

### Human-readable mode (default)

- concise actionable lines
- include delayed/skipped reason text where useful
- avoid noisy internal logs

### JSON mode (`--json`)

- emit one complete JSON document per command invocation
- prioritize practical fields over strict schema design
- no schema versioning commitment at this stage

---

## 5. Exit codes

- `0`: command completed successfully (including “nothing to do”)
- `1`: operational failure (manager command failure, runtime error)
- `2`: invalid CLI usage/config
- `130`: interrupted (SIGINT)

---

## 6. Command behavior

## `plan`

Purpose:
- compute and print intended updates under current policies

Properties:
- non-mutating
- safe default
- suitable for both humans and automation

Examples:
- `upnow plan`
- `upnow plan --managers brew,npm,uv --no-update`
- `upnow plan --json`

## `apply`

Purpose:
- execute manager-native upgrades according to current planning logic

Properties:
- mutating
- should print what it applied (and failures) clearly

Examples:
- `upnow apply --managers brew,npm`
- `upnow apply --dry-run --managers uv`
- `upnow apply --json`

---

## 7. Evolution notes

Deferred for now:
- strict JSON schema/versioning
- interactive confirmations/prompts
- advanced command graphing / manager dependency orchestration
- additional UX layers beyond concise text + JSON
