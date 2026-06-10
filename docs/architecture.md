# upgate v1 Architecture

This is the durable product and architecture contract for v1. Code is the source
of truth when this document and implementation disagree.

## Product Model

`upgate` keeps globally installed developer tools up to date across package
managers while avoiding very new releases until they pass a configured
`min_release_age`.

The supported workflows are:

- `scan`: list installed tools. `scan --verbose` may include release-age and
  audit notes.
- `plan`: show what would update without mutating the system.
- `apply`: apply selected updates. Interactive apply is the default; `--yolo`
  runs the batch default selection non-interactively.

Built-in managers are `brew`, `bun`, `cargo`, `npm`, `mise`, `pipx`, `pnpm`,
`uv`, `go`, `gem`, and `dotnet`.

Default manager modes are:

- `apply`: `brew`, `bun`, `cargo`, `npm`, `mise`, `pipx`, `pnpm`, `uv`, `go`
- `off`: `gem`, `dotnet`

Missing manager executables are treated as absent and omitted from normal
output.

## Layers

- `upgate-cli`: CLI parsing, config loading, orchestration, manager
  construction, audit service lifetime, selection persistence, and exit codes.
- `upgate-domain`: typed identities, versions, config, scan records, plan
  records, selections, audit facts, and errors.
- `upgate-planning`: version-policy evaluation, release-age evaluation, audit
  gating, candidate selection, and default batch selection.
- `upgate-managers`: concrete package-manager adapters. Managers own discovery,
  parsing, external command construction, release metadata lookup, audit subject
  emission, and manager-specific support checks.
- `upgate-release`: release-date and target-age evidence helpers.
- `upgate-audit`: OSV querybatch client, request batching, de-duplication,
  process-local caching, and audit request concurrency.
- `upgate-execution`: conversion from typed selections to execution commands
  and execution reports.
- `upgate-presentation`: batch output and interactive TUI state/rendering.
- `upgate-infra`: process execution, HTTP, environment, logging, and
  parallelism.

## Data Flow

Managers produce typed scan/update facts. Update facts are either:

- planner-selectable timelines, where shared planning chooses from release
  metadata; or
- manager-selected targets, where the manager resolver already chose a target
  and planning may only accept, delay, or block it.

Planning evaluates update facts with version policy, release age, and audit
evidence to produce an immutable `UpdatePlan`. Presentation renders plans but
does not create decisions. Selection produces a typed `PlanSelection`.
Execution resolves that selection into command intents, and managers turn those
intents into concrete commands.

Managers may pass `min_release_age` to native resolvers when the resolver owns
target selection, such as uv or Mise. Managers must not perform clock-aware
shared planning decisions. Those decisions belong in `upgate-planning`.

## Candidate Selection

For planner-selectable timelines, planning chooses the newest candidate by
publish timestamp after gates pass. Parsed version order is only a tie-breaker.
The gate order is:

1. installed-version comparison
2. version policy
3. release age
4. security audit, when the tool has a supported audit subject

If a newer candidate fails audit, planning may fall back to an older candidate
that already passed version policy and release age. If no candidate can safely
pass the gates, the item is blocked or delayed according to the failed gate.

For manager-selected targets, planning must not replace the manager-selected
target with another version. Brew, uv, and Mise depend on this shape.

## Version Policy

`version_policy` is per manager and accepts:

- `none`: no prerelease filtering
- `stable`: only final releases are eligible
- `same-track`: candidates must be at least as stable as the installed version

Unset policy resolves to `none`, except Gem resolves to `stable`.
`version_policy = "any"` is invalid.

Supported policy matrix:

- `brew`, `bun`, `cargo`, `dotnet`, `go`, `npm`, `pipx`, `pnpm`: all policy
  values
- `gem`: `stable` only
- `mise`, `uv`: `none` only

Release classes are ordered as `dev`, `alpha`, `beta`, `rc`, `final`.
Unknown prereleases are never treated as final. When `same-track` cannot
classify the installed stability track safely, it falls back to stable behavior
with a warning.

## Security Audit

Security audit uses OSV.dev. Managers emit an audit subject only when they can
map a tool to an OSV ecosystem/package identity without guessing. Unsupported
tools have no audit subject and are not audited.

Supported OSV ecosystems in the domain are `npm`, `crates.io`, `PyPI`,
`RubyGems`, `Go`, `NuGet`, and `GIT`.

Plan/apply audit behavior for supported subjects is fail-closed:

- clean target: eligible
- vulnerable target: blocked unless the user explicitly chooses a forced target
  in an interactive flow that exposes one
- audit lookup failure: blocked unless explicitly forced the same way

Unsupported audit subjects do not block and produce no audit note.

`scan --verbose` audits installed versions with supported subjects and may show
vulnerability or audit-unavailable notes. Non-verbose scan does not query audit.
Execution never queries OSV or re-evaluates audit.

## Config

Config is resolved before managers run. It controls global scan/audit/concurrency
settings, manager mode, `min_release_age`, `version_policy`, Brew `no_update`,
and interactive selection policy.

Selection policy is manager-local:

```toml
[npm.selection]
mode = "include" # or "skip"
except = ["typescript"]
```

`except` always means the opposite of `mode`. Omitted selection resolves to
`mode = "include", except = []` and is omitted when persisted.

Npm `min_release_age` must be a whole number of days because npm's native
`--min-release-age` accepts days.

## Output

Output is a product decision view, not resolver internals. User-visible item
states are `current`, `update`, `delayed`, `blocked`, `skipped`, and `error`.
Normal output should explain what will happen and why an update is withheld.
Verbose output may add release evidence, policy details, audit details, and
command diagnostics.

## Testing

For agents, the detailed test policy in `AGENTS.md` takes precedence. In short,
tests should protect stable behavior: CLI behavior, public API contracts, domain
invariants, manager behavior that is part of the product contract, version
policy, real-world parsing, config behavior, important error handling, and risky
integration behavior.

Do not add tests for private helpers, getters/setters, mock-call counts,
implementation order, current module boundaries, internal view-model shape, or
coverage preservation.
