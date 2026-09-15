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
- `apply`: apply selected updates and explicitly selected removals. Interactive
  apply is the default; `--yolo` runs the default update selection and never
  selects removals.

Built-in managers are `brew`, `bun`, `cargo`, `npm`, `mise`, `pipx`, `pnpm`,
`uv`, `go`, `gem`, and `dotnet`.

Default manager modes are:

- `apply`: `brew`, `bun`, `cargo`, `npm`, `mise`, `pipx`, `pnpm`, `uv`, `go`
- `off`: `gem`, `dotnet`

Missing manager executables are treated as absent and omitted from normal
output.

## Layers

- `upgate`: CLI parsing, config loading, orchestration, manager
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

Plan inputs cover installed tools, including tools without an available update.
Managers retain their discovery identity and explicit removal support independently
of update lookup, policy, and audit facts. A missing update target does not imply
that an installed tool cannot be removed. Uncertain identities and protected
manager-owned installations carry an unsupported-removal reason.

Each selected item has one action: update (with its selected target) or remove.
Removal is a choice for this run only. It is not an update target, policy bypass,
or a persistent selection mode. Presentation only offers actions supported by
the immutable plan; execution validates the chosen action again.

Apply reports selected items from command exit status; it does not perform a
post-mutation rescan. Managers must therefore give independently fallible item
updates separate commands. A command may map to several selected items only
when it is one manager-level operation whose exit status applies to the whole
selection. The same rule applies to removals, whose result identifies the action
without inventing a target version. All selected updates run before selected
removals across managers. A manager with selected removals uses item-specific
update commands instead of a manager-wide update shortcut.

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

Interactive confirmation persists update preferences at the existing point before
execution. Confirmed removals clear the package from that manager's `except`
set in either selection mode, even if the later uninstall fails. Merely marking
or unmarking removal does not change the remembered update preference. Dry-run
does not persist preference changes or removal cleanup. Config writing remains
CLI-owned; it is not transactional with package-manager execution.

Npm `min_release_age` must be a whole number of days. Execution converts it to
an absolute `--before` cutoff for each exact global install.

## Output

Output is a product decision view, not resolver internals. User-visible item
states are `current`, `update`, `delayed`, `blocked`, `skipped`, and `error`.
Normal output should explain what will happen and why an update is withheld.
Verbose output may add release evidence, policy details, audit details, and
command diagnostics.

The interactive action column uses `↑` for update, `−` for removal, and a blank
for no action. Removal rows also show a textual removal target. Space toggles
updates; when removal is marked, Space clears it to no action without changing
the remembered update preference. A second Space selects an available update.
`d` toggles removal and restores the previous choice when undone, and
Enter opens item details/actions. Bulk update selection preserves removal marks.
The normal view stays focused on updates; `v` shows all installed tools, and
marked rows remain visible in either view. Confirmation lists removal identities
and scopes as well as separate update/removal counts. Footer hints follow the
focused row: Space/x says update or deselect, removal is offered only when
available and not already marked, and `v` alternates show all/hide all.
`C confirm` remains the primary action; narrower terminals omit secondary hints
according to their rendered width. Combined a/n all/none hints retain separate
mouse targets for each operation.

## Removal Semantics

Managers own native uninstall command construction and retain normal dependency
checks. Removal does not add force, recursive cleanup, dependency removal, or
application-data cleanup flags. Manager mode `plan` remains non-mutating.

Removal scope follows the installed item shown to the user:

- Brew: the identified formula or cask, using normal uninstall behavior.
- Bun, npm, pnpm: the global package.
- Cargo: the installed package and its binaries.
- pipx and uv: the installed tool environment.
- .NET: the global tool package.
- Gem: the displayed installed version; default gems are not removable.
- Go: the exact discovered binary path, with no recursive deletion.
- mise: the displayed concrete `tool@version` using `mise uninstall`. Upgate
  does not edit mise configuration or select all installed versions implicitly.

Mise rejects selecting an update and a removal for different versions of the
same tool in one run: its selected upgrade command operates on configured tool
requests rather than a single installed version. Ambiguous upgrade sources are
reported as update errors while retaining known installed items for removal.
Pipx keeps environment identities distinct; suffixed or renamed environments
show an unsupported-removal reason instead of targeting the underlying package's
ordinary environment.

Only upgate's own selection preferences are cleaned at confirmation. Native
package-manager behavior determines changes to manager-owned metadata. In
particular, a mise config entry can remain and later cause mise to reinstall
the tool.

## Testing

For agents, the detailed test policy in `AGENTS.md` takes precedence. In short,
tests should protect stable behavior: CLI behavior, public API contracts, domain
invariants, manager behavior that is part of the product contract, version
policy, real-world parsing, config behavior, important error handling, and risky
integration behavior.

Do not add tests for private helpers, getters/setters, mock-call counts,
implementation order, current module boundaries, internal view-model shape, or
coverage preservation.
