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
- `TargetSelection`: planner-selectable timeline or manager-selected target
- `ManagerSelectedTarget`: target chosen by a manager resolver/outdated snapshot, plus typed evidence
- `ReleaseTimeline`: versions with publish timestamps
- `TargetAgeEvidence`: publish timestamp or manager-native target age evidence such as Brew tap commit age
- `UpdateCandidate`: target plus execution eligibility; policy and age outcomes live on `PlanItem`
- `VersionPolicy`: one no-policy mode, `stable`, `same-track`
- `PlanItem`: update, current, delayed, blocked, skipped, resolver error
- `UpdatePlan`: immutable typed plan per manager
- `PlanSelection`: selected candidates and pin changes
- `ExecutionPlan`: exact, native, grouped-native, or resolver-native commands derived from selected plan items

## Command Workflows
### Scan
CLI/config -> selected managers -> installed inventory -> optional verbose release-age lookup -> batch scan renderer. Preserve current scan inclusions/exclusions, including Brew dependency filtering and default gem skipping.

### Plan
CLI/config -> manager update discovery -> release or target-age evidence lookup -> target-selection evaluation -> version policy and min-age gates -> `UpdatePlan` -> batch renderer. No execution and no config mutation.

### Apply Batch
Build the same `UpdatePlan` as `plan` -> apply existing pins -> create default `PlanSelection` -> derive `ExecutionPlan` -> execute -> batch renderer.

### Apply Interactive
Build the same `UpdatePlan` -> TUI receives presentation view model -> user confirms typed `PlanSelection` -> persist pin changes after confirmation and before execution -> derive `ExecutionPlan` -> progress TUI executes and reports results.

## Module Boundaries
- `app`: CLI, orchestration, exit codes.
- `config`: TOML load/save, overrides, pin persistence, manager policy resolution.
- `domain`: strict types, policy model, plan model, scan model, error model.
- `planning`: shared planning logic from discovered target facts, release timelines, manager-selected target evidence, and policy/age settings.
- `execution`: selected plan to commands/results.
- `managers`: concrete adapters only; no UI output or config mutation.
- `release`: release-date and manager-native target-age evidence sources, cache, clock-aware age calculation.
- `presentation/batch`: terminal renderers.
- `presentation/tui`: TUI state, reducers, rendering.
- `infra`: process runner, HTTP client, clock, env, parallelism.

## Data Flow
Manager adapters produce typed update facts. A fact is either planner-selectable, where shared planning may choose from a release timeline, or manager-selected, where the manager has already selected the target and shared planning may only gate that target. Planning evaluates typed facts and returns typed outcomes. Presentation renders outcomes but does not create them. Selection modifies a typed plan selection. Execution consumes typed selections and manager execution capabilities.

Managers may use min-release-age when it is an input to a native resolver command, such as `uv --exclude-newer` or `mise --before`. Managers must not receive `now` or compare timestamps against policy age. Clock-aware age decisions belong to planning.

Planner-selectable managers provide a release timeline from which planning can select the newest policy/age-eligible candidate. Manager-selected target managers provide the selected target plus target evidence. For manager-selected targets, planning must not replace the selected target with another version from advisory metadata.

## Manager Abstraction
A `ManagerAdapter` trait is justified because there are multiple real managers. It should expose identity, defaults, capabilities, installed discovery, update discovery, target/release evidence lookup requests, and execution command construction. It must not emit outcomes, parse TUI choices, persist pins, compare release age against `now`, or decide batch vs interactive behavior.

Manager behavior to preserve includes:
- Brew: `brew update`, Homebrew-selected targets from outdated JSON, info JSON, tap git/GitHub commit target-age evidence, grouped formula/cask upgrades, `no_update`.
- npm/pnpm/yarn/bun: global installed/outdated, registry time maps, exact installs, native global shortcuts where valid.
- Cargo: install list, `.crates2.json`, crates.io, preserved install flags.
- pipx/uv: PyPI metadata; uv manager-selected target from dry-run with `--exclude-newer`; uv outdated latest is advisory metadata only.
- Go: Go bin discovery, `go version -m`, `go list -m`.
- Gem: default off, skip default gems, Ruby runtime requirement filtering.
- Dotnet: default off, NuGet registration metadata.
- Mise: manager-selected target from dry-run with `--before`; independently require publish-date metadata for the selected target; outdated latest is advisory metadata only.

## Versioning Policy
Collapse `Disabled` and `Any` into one no-policy concept. Keep `stable` and `same-track`. `stable` allows final releases only. `same-track` allows same-or-more-stable releases; unknown installed stability falls back to stable with a warning.

For planner-selectable timelines, version policy participates in candidate selection. For manager-selected targets, version policy is a target gate: accept or block the selected target, but do not synthesize an older target. Brew uses this target-gate behavior. uv and Mise support only no-policy and must reject unsupported policies before discovery.

## Release Date Lookup
Release and target-age evidence lookup is explicit and testable. Use typed lookup requests for npm registry time, PyPI, crates.io, Go module metadata, RubyGems, NuGet, Brew tap commits/GitHub fallback, and Mise backend/versions-host metadata. Results are item-scoped: known timeline, known target evidence, missing metadata, lookup failed. Missing required target metadata or lookup failure blocks that item, not the manager. Advisory latest metadata can annotate output but must not block a manager-selected target when the selected target evidence is known.

## Execution Planning
Execution planning must model the command shape, not just the displayed target. Some selections execute exact targets. Some execute native selected updates. Some execute grouped native commands such as Brew formula/cask groups. Some execute resolver-native commands such as uv and Mise, where apply re-runs the manager resolver with the same age constraint rather than installing the displayed target exactly.

Display plan and apply command shape may differ when the manager resolver is authoritative. This difference must be typed in `ExecutionPlan`; it must not be hidden in manager metadata or string fields.

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
Reject preserving current wiring with typed wrappers; coupling remains. Reject a generic workflow engine; current managers are too irregular. Reject stringly typed plans and display-note parsing. Reject making the TUI authoritative for domain decisions. Reject always using exact per-item commands; native shortcuts are needed for speed and default behavior when semantically safe. Reject passing `now` into manager discovery so managers can do age planning. Reject trimming release timelines in manager code to steer generic planning. Reject hidden one-off uv, Mise, or Brew branches instead of a typed manager-selected target model.

## Resolved Questions
- `scan` behavior stays as-is.
- Native global apply shortcuts stay when selected plan semantics allow them.
- Keep only one no-policy mode.
- Interactive pin changes persist after confirmation and before execution.
- Forced ineligible updates are available only when the manager has typed support for the required bypass command, such as exact target execution or resolver-native age bypass.
- Release metadata failure blocks the item.
- `any` policy is removed entirely and should be rejected as any other invalid policy.
- Manager-selected targets are a first-class planning input. uv, Mise, and Brew must not be forced through planner-selectable timeline semantics.

## Constraints For Future Codex Agents
Do not treat old `/src` architecture as a refactor target. Preserve observed behavior, commands, parsing rules, URLs, config behavior, and TUI UX where explicitly useful. Do not preserve accidental abstractions. Do not introduce traits without multiple real implementations. Prefer simple typed structs/enums over framework-like design. Keep manager-specific workarounds isolated and named.
