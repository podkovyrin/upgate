# upnow Architecture

This is the current implementation contract after the workspace rebuild. Historical
phase notes were removed; this document records the decisions that should still
guide future changes.

## Goal

`upnow` is built around `apply`. Batch `apply` and interactive `apply` share the
same planning and execution core. `plan` exposes the planning half of `apply`,
and `scan` lists installed tools.

## Layers

- `upnow-cli`: CLI parsing, orchestration, exit codes, manager construction, and
  config persistence timing.
- `upnow-domain`: typed identities, versions, policies, scan records, plan
  outcomes, selections, and errors.
- `upnow-planning`: candidate evaluation, version policy, release-age gates, and
  default batch selection.
- `upnow-managers`: concrete manager adapters. Managers own parsing, requests,
  manager-specific command construction, policy support checks, and named
  workarounds.
- `upnow-release`: release-date and target-age evidence sources.
- `upnow-audit`: shared security-audit evidence source. It owns OSV API
  requests, batching, response parsing, de-duplication, and process-local audit
  lookup caching.
- `upnow-execution`: selected plan items to execution commands/results.
- `upnow-presentation`: batch output and TUI state/rendering.
- `upnow-infra`: process, HTTP, clock, environment, logging, and parallelism.

## Data Flow

Managers produce typed update facts and optional typed audit identities.
Planning evaluates update facts with release evidence, version policy,
release-age settings, and audit evidence to produce an immutable `UpdatePlan`.
Presentation renders plan outcomes but does not create them. Selection produces
a typed `PlanSelection`. Execution resolves that selection to command intents,
and managers turn those intents into concrete commands.

Managers may pass `min_release_age` into native resolver commands when that is
part of the manager's resolver, such as `uv --exclude-newer` or `mise --before`.
Managers must not receive `now` or perform clock-aware age comparisons for
planning. Clock-aware planning decisions belong in `upnow-planning`; scan age
display belongs in CLI/presentation.

Managers must not query vulnerability databases, parse vulnerability database
responses, decide whether a vulnerability blocks an update, or format audit
messages. Their audit responsibility is limited to emitting an explicit package
identity when the manager can map an installed tool to an OSV package ecosystem
without guessing.

Audit lookup orchestration belongs outside manager adapters. `upnow-cli` owns
the command-run audit service instance and passes audit evidence into planning.
`upnow-planning` owns the audit gate decision. Unsupported audit identities are
silent and must not change plan, apply, or scan behavior.

## Manager Targets

There are two target-selection shapes:

- Planner-selectable timelines: shared planning chooses the newest candidate
  allowed by version policy, release age, and security audit.
- Manager-selected targets: the manager has already selected a target, and
  shared planning only gates that target.

Manager-selected targets are required for Brew, uv, and Mise. Planning must not
replace a manager-selected target with an older or newer advisory version.
Security audit must respect this: for manager-selected targets, audit may accept
or block the selected target, but must not cause planning to choose an alternate
version.

## Execution

Execution planning models command shape explicitly. Some selections execute exact
targets. Some execute native selected updates. Some execute grouped native
commands, such as Brew formula/cask groups. Some execute resolver-native commands,
such as uv and Mise, where apply re-runs the native resolver with the same age
constraint rather than installing the displayed target exactly.

The command shape belongs in typed execution data. It must not be hidden in
manager metadata, display notes, or private manager command types.

## Interactive Apply

Interactive selection starts from manager ids so users can see planning activity.
Ready managers enter the TUI as view models derived from `UpdatePlan`. The TUI
owns presentation state only: tabs, cursor, visible rows, selected ids, modal
state, and live planning activity/error state.

The TUI must not mutate `UpdatePlan`, parse display notes, run manager commands,
or persist config. Confirmed selections return typed `PlanSelection` values.
Interactive selection-policy persistence happens after confirmation and before
execution. Cancellation returns without execution or config persistence.

## Config

Config resolves manager mode, `min_release_age`, `version_policy`, update
selection policy, Brew `no_update`, manager concurrency, and scan old-age
threshold before planning. Security audit concurrency is resolved before any
audit lookups. Resolved manager config is passed into concrete manager adapters
at construction.

Selection policy is persisted as:

```toml
[npm.selection]
mode = "include" # or "skip"
except = ["typescript"]
```

`except` always means the opposite of `mode`. Omitted `[manager.selection]`
resolves to `mode = "include", except = []`, and that default is omitted when
persisted.

## Version Policy

There is one no-policy mode, plus `stable` and `same-track`.

- No policy: no prerelease filtering.
- `stable`: only final releases are eligible.
- `same-track`: candidates must be at least as stable as the installed version.

For planner-selectable timelines, version policy participates in candidate
selection. For manager-selected targets, version policy gates only the selected
target. uv and Mise do not support version policy and reject configured policies.

## Security Audit

Security audit is an update gate for supported OSV package identities. For
planner-selectable timelines, planning may choose the newest candidate that
passes version policy, release age, and audit. For manager-selected targets,
planning may only gate the manager-selected target.

For supported audit identities, audit lookup is fail-closed during plan/apply:
a vulnerable target or failed audit lookup blocks that target. For unsupported
or unknown audit identities, audit is skipped silently and must not block.

Verbose scan may annotate installed tools with vulnerability findings. Non-
verbose scan remains a simple installed-tool listing.

See [Security audit feature spec](security-audit.md).

## Testing

Tests should protect stable behavior: CLI behavior, public API contracts, durable
domain invariants, manager behavior that is part of the product contract,
version-policy behavior, real-world parsing, config behavior, important error
handling, and expensive integration paths.

Do not add tests for private helpers, getters/setters, mock-call counts,
implementation order, current module boundaries, internal view-model shape,
temporary architecture scaffolding, or snapshots/goldens without a clear
user-visible contract.

## Rejected Approaches

- Restoring the old `/src` implementation or old manager framework.
- Preserving old wiring with typed wrappers.
- Generic workflow engines for manager behavior.
- Stringly typed plans or display-note parsing.
- Making the TUI authoritative for domain decisions.
- Passing `now` into manager discovery for age planning.
- Trimming release timelines inside manager code to steer planning.
- Hidden uv, Mise, or Brew branches instead of typed manager-selected targets.
- Shared ecosystem helper layers whose only current reason is reducing
  duplication.
- Manager-local vulnerability lookup, OSV parsing, audit gating, or audit note
  formatting.
- Inferring audit package ecosystems from display names, free-form metadata, or
  presentation strings.
- Using manager metadata as planner audit input instead of typed audit
  identities and typed audit evidence.
- Duplicate manager-private execution command types.
- Boolean forced-selection state instead of typed selected targets.
- Materializing all installed package names to represent "skip all except one";
  use `mode = "skip"` with exceptions.
