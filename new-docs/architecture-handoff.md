# Architecture Handoff

## Project Goal
Rebuild `upnow` around `apply` as the central feature. `apply` has two modes: interactive TUI mode and batch terminal mode. `plan` exposes the planning half of `apply`; `scan` lists installed versions. The existing `/src` code is behavioral reference only, not an architecture target.

## Current Problem
The old architecture mixes manager behavior, planning, terminal rendering, TUI state, config mutation, and execution. Domain decisions are encoded in strings, display notes, indices, and fields such as `apply_spec_base`. Interactive selection mutates presentation-derived structures instead of a typed plan. Manager files emit outcomes directly and contain workflow orchestration that should be shared.

## Approved Architecture
Use a typed layered architecture with concrete manager adapters. Batch and TUI modes share the same planning and execution core. Managers stay manager-specific but are isolated behind narrow boundaries. Do not build a framework for hypothetical future managers.

## Core Domain Concepts
- `ManagerId`, `ToolId`, `PackageName`, `ToolName`
- `VersionText` plus version scheme: SemVer, PEP 440, manager-native
- `InstalledTool`: installed item plus manager metadata
- `UpdateSeed`: manager-discovered update input
- `ReleaseTimeline`: versions with publish timestamps
- `UpdateCandidate`: target plus policy/age eligibility
- `VersionPolicy`: one no-policy mode, `stable`, `same-track`
- `PlanItem`: update, current, delayed, blocked, skipped, resolver error
- `UpdatePlan`: immutable typed plan per manager
- `PlanSelection`: selected candidates and pin changes
- `ExecutionPlan`: concrete commands derived from selected plan items

## Command Workflows
### Scan
CLI/config -> selected managers -> installed inventory -> optional verbose release-age lookup -> batch scan renderer. Preserve current scan inclusions/exclusions, including Brew dependency filtering and default gem skipping.

### Plan
CLI/config -> manager update discovery -> release timeline lookup -> version policy and min-age evaluation -> `UpdatePlan` -> batch renderer. No execution and no config mutation.

### Apply Batch
Build the same `UpdatePlan` as `plan` -> apply existing pins -> create default `PlanSelection` -> derive `ExecutionPlan` -> execute -> batch renderer.

### Apply Interactive
Build the same `UpdatePlan` -> TUI receives presentation view model -> user confirms typed `PlanSelection` -> persist pin changes after confirmation and before execution -> derive `ExecutionPlan` -> progress TUI executes and reports results.

## Module Boundaries
- `app`: CLI, orchestration, exit codes.
- `config`: TOML load/save, overrides, pin persistence, manager policy resolution.
- `domain`: strict types, policy model, plan model, scan model, error model.
- `planning`: shared planning logic from discovered data and release timelines.
- `execution`: selected plan to commands/results.
- `managers`: concrete adapters only; no UI output or config mutation.
- `release`: release-date lookup sources, cache, clock-aware age calculation.
- `presentation/batch`: terminal renderers.
- `presentation/tui`: TUI state, reducers, rendering.
- `infra`: process runner, HTTP client, clock, env, parallelism.

## Data Flow
Manager adapters produce typed data. Planning evaluates that data and returns typed outcomes. Presentation renders outcomes but does not create them. Selection modifies a typed plan selection. Execution consumes only typed selections and manager execution capabilities.

## Manager Abstraction
A `ManagerAdapter` trait is justified because there are multiple real managers. It should expose identity, defaults, capabilities, installed discovery, update discovery, release lookup requests, and execution command construction. It must not emit outcomes, parse TUI choices, persist pins, or decide batch vs interactive behavior.

Manager behavior to preserve includes:
- Brew: `brew update`, outdated JSON, info JSON, tap git/GitHub commit age, grouped formula/cask upgrades, `no_update`.
- npm/pnpm/yarn/bun: global installed/outdated, registry time maps, exact installs, native global shortcuts where valid.
- Cargo: install list, `.crates2.json`, crates.io, preserved install flags.
- pipx/uv: PyPI metadata; uv dry-run with `--exclude-newer`.
- Go: Go bin discovery, `go version -m`, `go list -m`.
- Gem: default off, skip default gems, Ruby runtime requirement filtering.
- Dotnet: default off, NuGet registration metadata.
- Mise: trust dry-run target, then independently require publish-date metadata.

## Versioning Policy
Collapse `Disabled` and `Any` into one no-policy concept. Keep `stable` and `same-track`. `stable` allows final releases only. `same-track` allows same-or-more-stable releases; unknown installed stability falls back to stable with a warning. Brew policy is a target gate only because Homebrew exposes one selected outdated target, not a full candidate timeline.

## Release Date Lookup
Release lookup is explicit and testable. Use typed lookup requests for npm registry time, PyPI, crates.io, Go module metadata, RubyGems, NuGet, Brew tap commits/GitHub fallback, and Mise backend/versions-host metadata. Results are item-scoped: known timeline, missing metadata, lookup failed. Missing metadata or lookup failure blocks that item, not the manager.

## TUI Boundary
TUI state is presentation state only: tabs, cursor, visible rows, selected ids, modal state. The TUI receives a view model derived from `UpdatePlan` and returns `PlanSelection`. It must not parse display notes, mutate plan strings, own apply closures, or persist pins.

## Config Boundary
Config resolves manager mode, min release age, version policy, pins, Brew `no_update`, and scan old-age threshold before planning. Interactive pin persistence happens only after confirmed apply selection and before execution. CLI manager selection may override manager mode as current behavior does.

## Error Handling
Use typed errors below `app`: manager unavailable, discovery failed, release lookup failed, missing metadata, parse failed, unsupported policy, execution failed, interrupted. Planning supports item-level and manager-level errors. Execution errors attach to selected items. Signal interruption maps to exit `130`.

## Testing Strategy
Use fixture tests for every manager command and HTTP payload parser. Use fake clock/process/HTTP clients for planner, release lookup, and execution command construction. Test version policy across SemVer, PEP 440, Brew-native versions, and same-track fallback. Test TUI reducers separately from rendering. Add batch renderer golden tests and integration tests with fake manager adapters.

## Reusable Old Code
Reuse with cleanup: version classification and candidate evaluation, SemVer/PEP 440 timestamp parsing, manager parsers, config load/overrides/pin persistence, mutation-skip process runner behavior, HTTP defaults, parallel order-preserving helper, TUI visual components, terminal outcome formatting.

## Delete Or Redesign
Redesign `ManagerCtx`, `ManagerPlugin::scan/apply/interactive_apply`, `run_plan_apply_framework`, `PlannedUpdate`, `ApplyCandidate`, string display notes as metadata, `apply_spec_base`, outcome emission during planning, closure-owned interactive apply plans, `chosen_versions: Vec<Option<usize>>`, and manager soft-fail helpers that emit UI output.

## Rejected Approaches
Reject preserving current wiring with typed wrappers; coupling remains. Reject a generic workflow engine; current managers are too irregular. Reject stringly typed plans and display-note parsing. Reject making the TUI authoritative for domain decisions. Reject always using exact per-item commands; native shortcuts are needed for speed and default behavior when semantically safe.

## Resolved Questions
- `scan` behavior stays as-is.
- Native global apply shortcuts stay when selected plan semantics allow them.
- Keep only one no-policy mode.
- Interactive pin changes persist after confirmation and before execution.
- Forced ineligible updates are available only for managers that support exact target execution.
- Release metadata failure blocks the item.
- `any` policy is removed entirely and should be rejected as any other invalid policy.

## Constraints For Future Codex Agents
Do not treat old `/src` architecture as a refactor target. Preserve observed behavior, commands, parsing rules, URLs, config behavior, and TUI UX where explicitly useful. Do not preserve accidental abstractions. Do not introduce traits without multiple real implementations. Prefer simple typed structs/enums over framework-like design. Keep manager-specific workarounds isolated and named.
