# Implementation Plan

## Assumptions
This rebuild happens in a new Rust workspace inside the current project folder. This document uses `<new-workspace>/` as a placeholder path. The existing `/src` tree is read-only behavioral reference. Do not import old modules into the new workspace.

## Phase 0: Workspace Shell And Guardrails

### Goal
Create the new workspace with enough structure to develop independently from the old architecture.

### Behavior Delivered
No product behavior yet. The new workspace builds and runs an empty CLI skeleton or placeholder binary.

### Modules/Files Likely Created Or Changed
- `<new-workspace>/Cargo.toml`
- `<new-workspace>/crates/upnow-cli/`
- `<new-workspace>/crates/upnow-domain/`
- `<new-workspace>/crates/upnow-infra/`
- `<new-workspace>/crates/upnow-planning/`
- `<new-workspace>/crates/upnow-managers/`
- `<new-workspace>/crates/upnow-presentation/`

### Tests Required
- Workspace build smoke test.
- Empty test suite runs successfully.
- CI/local command documented for the new workspace.

### What Must Not Be Included Yet
- No manager implementation.
- No TUI.
- No old `/src` imports.
- No compatibility layer around old `ManagerPlugin`, `ManagerCtx`, `PlannedUpdate`, or `ApplyCandidate`.

### Architectural Risks
Developers may copy old structure because it is nearby.

### Stop Conditions
Stop when the new workspace compiles independently and has explicit project boundaries.

### Review Checklist
- New workspace does not depend on the old crate internals.
- Old `/src` is referenced only in comments/tests as behavioral source.
- Crate names reflect layers, not old modules.

## Phase 1: Behavioral Fixtures From Old Code

### Goal
Capture current behavior before rewriting managers.

### Behavior Delivered
No new command behavior. Fixture files and tests define expected parsing/config behavior.

### Modules/Files Likely Created Or Changed
- `<new-workspace>/tests/fixtures/managers/...`
- `<new-workspace>/crates/upnow-managers/tests/...`
- `<new-workspace>/crates/upnow-domain/tests/...`

### Tests Required
- Fixtures for current manager command outputs:
  - Brew outdated/info/tap metadata shapes.
  - npm/pnpm/yarn/bun installed/outdated/time outputs.
  - Cargo install list and `.crates2.json`.
  - pipx list JSON and PyPI payloads.
  - uv tool list/outdated/dry-run output.
  - Go `version -m` and `go list -m` payloads.
  - Gem list/outdated and RubyGems payloads.
  - Dotnet tool list and NuGet registration payloads.
  - Mise dry-run, registry, ls-remote, versions-host payloads.
- Config fixture tests for policy, pins, mode, `no_update`, and scan age threshold.

### What Must Not Be Included Yet
- No production parser code unless needed to prove fixture shape.
- No planning core.
- No TUI wiring.
- No broad manager abstractions.

### Architectural Risks
Tests may encode old internal names rather than externally visible behavior.

### Stop Conditions
Stop when fixtures cover the behaviors that must survive the rebuild.

### Review Checklist
- Fixtures test command output parsing, not old types.
- No stringly plan fields are introduced.
- No outdated `docs/*` content is used as source of truth.

## Phase 2: Core Domain Types

### Goal
Define the typed model used by all later phases.

### Behavior Delivered
No CLI behavior yet. Domain types compile and are unit-tested. This phase is now explicitly representation-first: it defines stable domain state shapes and lightweight constructor validation, but does not evaluate version policy, min-release-age eligibility, forced update eligibility, or apply selection semantics.

### Completed In This Phase
- Added typed identities and names: `ManagerId`, `ToolId`, `PackageName`, `ToolName`, `VersionText`, `VersionScheme`.
- Added installed and scan representation: `InstalledTool`, manager-owned metadata representation, `ScanReport`, `ScanItem`, `ScanIssue`.
- Added release lookup representation: `ReleaseTimestamp`, `ReleaseTimeline`, `ReleaseLookupResult`.
- Added plan representation: `UpdateSeed`, `UpdateCandidate`, `PlanItem`, `UpdatePlan`, policy block reasons, delay reasons, skip reasons, and execution eligibility.
- Added selection representation: `PlanSelection`, `SelectedItem`, `PinChange`, and `PinOperation`.
- Added domain error types for constructor validation and referential selection validation.
- Stabilized `PlanSelection` to validate only stable references: selected item ids must exist in the plan, and pin-change packages must exist in the plan.

### Modules/Files Likely Created Or Changed
- `<new-workspace>/crates/upnow-domain/src/lib.rs`
- `<new-workspace>/crates/upnow-domain/src/manager.rs`
- `<new-workspace>/crates/upnow-domain/src/version.rs`
- `<new-workspace>/crates/upnow-domain/src/policy.rs`
- `<new-workspace>/crates/upnow-domain/src/release.rs`
- `<new-workspace>/crates/upnow-domain/src/plan.rs`
- `<new-workspace>/crates/upnow-domain/src/scan.rs`
- `<new-workspace>/crates/upnow-domain/src/selection.rs`
- `<new-workspace>/crates/upnow-domain/src/error.rs`

### Tests Added
- Domain constructor and validation tests.
- `VersionPolicy` parse/display tests.
- `PlanItem` state tests: update, current, delayed, blocked, skipped, resolver error.
- `UpdateCandidate` representation tests for target and execution eligibility.
- `PlanSelection` tests for selected item ids and pin changes.
- Release lookup representation tests.

### Intentionally Removed During Stabilization
- Removed phase-2 enforcement of policy and release-age readiness from `UpdateCandidate`.
- Removed `PolicyEligibility` and `ReleaseAgeEligibility` from `UpdateCandidate`; policy and age outcomes are represented by `PlanItem` variants until phase 3 evaluation exists.
- Removed candidate readiness constructors and readiness checks that tried to encode phase 3 behavior early.
- Removed apply/forced-update eligibility checks from `PlanSelection`; those semantics belong after policy evaluation and execution capability modeling are present.

### Changed Assumptions
- Phase 2 does not make invalid planner outcomes unrepresentable. It defines typed states that phase 3 must construct consistently.
- `UpdateCandidate` represents the discovered target, versions, version scheme, and execution eligibility. Policy and age decisions are represented at the plan item level in phase 2.
- `PlanSelection` is referentially valid, not execution-valid. It does not decide whether a selected item should execute.

### What Must Not Be Included Yet
- No manager discovery.
- No process or HTTP code.
- No renderer.
- No TUI.
- No traits unless the domain has multiple concrete uses.

### Architectural Risks
Over-modeling or recreating old string fields under typed names. The remaining generic manager metadata representation is a known risk: later phases must not use metadata keys as hidden control flow or as a replacement for typed manager adapter capabilities.

### Deferred Findings
- Exact manager metadata shape is deferred until concrete manager adapters need it. Keep manager-specific workarounds isolated and named.
- Release lookup error detail validation can be tightened later if release lookup implementation needs it.
- Forced delayed-update selection semantics are deferred until phase 3 and execution capability modeling.

### Stop Conditions
Stop when the domain can represent scan rows, update candidates, policy decisions as plan item outcomes, release lookup results, pin changes, and execution eligibility without process, HTTP, rendering, TUI, manager discovery, or planner evaluation logic.

### Review Checklist
- Domain types contain no terminal display strings.
- No opaque string fields for manager control flow.
- No equivalent of `apply_spec_base`.
- TUI concepts do not appear in domain types.

## Phase 3: Policy And Candidate Evaluation

### Goal
Implement typed version policy and min-release-age evaluation.

### Behavior Delivered
Given installed version, release timeline, policy, and clock time, the planner can produce typed plan item outcomes: update, current, delayed, blocked, skipped, or resolver error. Phase 3 owns policy and release-age consistency; it must not rely on `PlanSelection` to filter invalid planner output.

### Modules/Files Likely Created Or Changed
- `<new-workspace>/crates/upnow-planning/src/lib.rs`
- `<new-workspace>/crates/upnow-planning/src/evaluate.rs`
- `<new-workspace>/crates/upnow-domain/src/policy.rs`
- `<new-workspace>/crates/upnow-domain/src/release.rs`
- `<new-workspace>/crates/upnow-domain/src/plan.rs` only for small model adjustments required by actual evaluation.

### Tests Required
- SemVer candidate ordering.
- PEP 440 candidate ordering.
- Brew-native stability classification.
- `stable` policy blocks prereleases.
- `same-track` allows same-or-more-stable versions.
- Unknown installed track falls back to stable with typed warning.
- No-policy mode applies no stability filter.
- Too-fresh versions are delayed.
- Missing release metadata blocks the item.
- Policy-blocked candidates produce `PlanItem::Blocked` with typed policy reason.
- Release-age-blocked candidates produce `PlanItem::Delayed`.
- Eligible candidates produce `PlanItem::Update`.

### What Must Not Be Included Yet
- No manager adapters.
- No CLI orchestration.
- No TUI selection.
- No execution commands.

### Architectural Risks
Reintroducing old `Disabled` and `Any` as separate concepts. Another risk is pushing selection/execution behavior into policy evaluation; phase 3 should produce plan outcomes only.

### Stop Conditions
Stop when policy evaluation can fully explain why each candidate is eligible, delayed, blocked, current, skipped, or a resolver error, using typed plan outcomes and without manager adapters, CLI orchestration, TUI selection, execution commands, process, or HTTP.

### Review Checklist
- Only one no-policy mode exists.
- Policy warnings are typed.
- Age gate and policy gate are separate typed facts.
- Forced eligibility is not wired to UI.
- `PlanSelection` remains referential validation only.

## Phase 4: Infrastructure Layer

### Goal
Add testable process, HTTP, clock, env, and parallelism infrastructure.

### Behavior Delivered
No product behavior yet. Infrastructure can be faked in tests.

### Modules/Files Likely Created Or Changed
- `<new-workspace>/crates/upnow-infra/src/lib.rs`
- `<new-workspace>/crates/upnow-infra/src/process.rs`
- `<new-workspace>/crates/upnow-infra/src/http.rs`
- `<new-workspace>/crates/upnow-infra/src/clock.rs`
- `<new-workspace>/crates/upnow-infra/src/env.rs`
- `<new-workspace>/crates/upnow-infra/src/parallel.rs`

### Tests Required
- Command success/failure classification.
- Signal/interruption classification.
- Mutation-skip behavior.
- HTTP timeout/user-agent behavior.
- Base URL env override behavior.
- Fake clock tests.
- Parallel execution preserves result order.

### What Must Not Be Included Yet
- No manager logic beyond infra tests.
- No UI output.
- No generic plugin framework.

### Architectural Risks
Introducing traits everywhere for testability. Prefer concrete wrappers and introduce traits only where fake and real implementations are both used now.

### Stop Conditions
Stop when managers and release lookups can use real or fake process/HTTP/clock dependencies.

### Review Checklist
- Domain and planning crates do not call process or HTTP directly.
- Mutation safety behavior is preserved.
- Infra errors are typed enough for app-level mapping.

## Phase 5: Config Boundary

### Goal
Implement config loading, overrides, policy resolution, and pin persistence in the new workspace.

### Behavior Delivered
The new CLI can resolve config values into typed domain settings, but commands may still be incomplete.

### Modules/Files Likely Created Or Changed
- `<new-workspace>/crates/upnow-cli/src/config.rs`
- `<new-workspace>/crates/upnow-domain/src/config.rs` if shared typed config is needed
- `<new-workspace>/crates/upnow-cli/tests/config.rs`

### Tests Required
- Default manager mode and release age.
- Brew default `12h`.
- Gem and Dotnet default `off`.
- Brew-only `no_update`.
- Manager mode override from CLI manager selection.
- Pin persistence preserves unrelated TOML.

### What Must Not Be Included Yet
- No TUI pin persistence.
- No manager execution.
- No global app orchestration beyond config resolution.

### Architectural Risks
Letting config own runtime behavior decisions that belong to planning/execution.

### Stop Conditions
Stop when config produces typed per-manager settings and can persist pins independently.

### Review Checklist
- Config does not know about TUI state.
- Pins are typed names, not rendered strings.
- Unsupported policy per manager can be reported cleanly.

## Phase 6: First Manager Vertical Slice

### Goal
Prove the architecture with one simple manager end to end in batch mode.

### Recommended Manager
Use `pnpm` first. It has clear installed/outdated JSON, registry time lookup, exact target execution, and no npm whole-day release-age nuance.

### Behavior Delivered
For `pnpm`, support:
- `scan`
- `plan`
- `apply` batch
- pins
- exact target execution
- native shortcut where valid

### Modules/Files Likely Created Or Changed
- `<new-workspace>/crates/upnow-managers/src/lib.rs`
- `<new-workspace>/crates/upnow-managers/src/pnpm.rs`
- `<new-workspace>/crates/upnow-planning/src/planner.rs`
- `<new-workspace>/crates/upnow-cli/src/main.rs`
- `<new-workspace>/crates/upnow-presentation/src/batch.rs`
- `<new-workspace>/crates/upnow-execution/src/lib.rs` if execution is separated as its own crate

### Tests Required
- `pnpm list -g --depth 0 --json` parser.
- `pnpm outdated -g --json` parser.
- `pnpm view <name> time --json` parser.
- Plan update/current/delayed/blocked cases.
- Apply exact command construction.
- Native shortcut eligibility.
- Release lookup failure blocks only the item.

### What Must Not Be Included Yet
- No TUI.
- No other manager migrations.
- No generic manager framework beyond what `pnpm` proves necessary.

### Architectural Risks
Designing a manager abstraction from one manager. Keep abstraction minimal; defer trait extraction until the second manager.

### Stop Conditions
Stop when `pnpm` batch behavior works through typed domain, planning, release lookup, and execution.

### Review Checklist
- Manager code emits no terminal outcomes directly.
- Manager code does not inspect CLI mode.
- Execution uses typed selected plan items.
- No display-note parsing exists.

## Phase 7: Manager Abstraction Extraction

### Goal
Introduce the manager adapter boundary only after at least two real managers need it.

### Behavior Delivered
`pnpm` plus one additional npm-family manager share a small concrete manager adapter interface.

### Suggested Second Manager
Use `npm` to validate whole-day min age and native shortcut semantics.

### Modules/Files Likely Created Or Changed
- `<new-workspace>/crates/upnow-managers/src/adapter.rs`
- `<new-workspace>/crates/upnow-managers/src/registry.rs`
- `<new-workspace>/crates/upnow-managers/src/npm.rs`
- `<new-workspace>/crates/upnow-managers/src/pnpm.rs`

### Tests Required
- Registry selection by manager id.
- Unknown manager error.
- Policy support validation.
- npm installed/outdated/time parser tests.
- npm exact install and native update command tests.

### What Must Not Be Included Yet
- No support for hypothetical managers.
- No TUI.
- No Brew/Mise abstractions.

### Architectural Risks
Making the adapter trait too broad. It should cover only behavior used by current migrated managers.

### Stop Conditions
Stop when two real managers use the same narrow adapter boundary cleanly.

### Review Checklist
- Trait methods map to real current behavior.
- No `interactive_apply` equivalent exists.
- No manager returns presentation strings.
- Manager capabilities are typed.

## Phase 8: Batch Command Orchestration

### Goal
Route `scan`, `plan`, and `apply` batch mode through the new shared core for migrated managers.

### Behavior Delivered
The new CLI supports selected managers, config, planning, execution, and batch rendering for migrated managers.

### Modules/Files Likely Created Or Changed
- `<new-workspace>/crates/upnow-cli/src/app.rs`
- `<new-workspace>/crates/upnow-cli/src/cli.rs`
- `<new-workspace>/crates/upnow-presentation/src/batch.rs`
- `<new-workspace>/crates/upnow-execution/src/...`
- `<new-workspace>/crates/upnow-managers/src/registry.rs`

### Tests Required
- CLI command parsing.
- Default command is `plan`.
- `--managers` filtering.
- `--set` overrides.
- Batch output golden tests.
- Batch apply with pins.
- Missing command and unsupported manager behavior.
- Exit code behavior.

### What Must Not Be Included Yet
- No interactive mode.
- No progress TUI.
- No migrated complex managers unless in separate phases.

### Architectural Risks
Renderer leaking back into planner through convenience fields.

### Stop Conditions
Stop when batch mode is fully typed for migrated managers and output is equivalent.

### Review Checklist
- Planner returns typed outcomes only.
- Renderer owns text formatting.
- Execution result owns command failure details.
- Native shortcut is used only when selected plan exactly allows it.

## Phase 9: Manager Migration Waves

### Goal
Migrate all remaining managers in small independent reviews.

### Behavior Delivered
Each manager preserves current behavior in the new architecture.

### Completed Required Refactor: Manager-Selected Targets
Before continuing uv, Mise, and Brew migration, typed manager-selected target planning was added. This refactor belongs to Phase 9 because it is required by the managers being migrated now.

#### Goal
Represent manager-selected targets directly so uv, Mise, and Brew do not fake planner-selectable timelines.

#### Behavior Delivered
Planning supports two target-selection shapes:
- Planner-selectable timeline: shared planning may choose the newest policy/age-eligible candidate from a release timeline.
- Manager-selected target: the manager has already selected the target, and shared planning only gates that target by policy and age.

For manager-selected targets, planning must not replace the target with another version from advisory latest metadata. Managers may pass min-release-age to native resolver commands when required, such as `uv --exclude-newer` or `mise --before`, but managers must not receive `now` or compare target age against policy.

#### Completed In This Refactor
- Added `TargetSelection` with planner-selectable and manager-selected shapes.
- Added `ManagerSelectedTarget` with required target-age evidence and optional advisory release metadata.
- Added target-age lookup representation for publish timestamps and manager-native target-age evidence.
- Updated planning evaluation to dispatch by target-selection shape.
- Updated manager-selected evaluation to gate only the selected target by policy and age.
- Kept advisory/latest metadata separate from required target-age evidence.
- Added resolver-native execution intent support for manager resolvers such as uv.
- Preserved grouped native execution reporting for each selected item.
- Kept clock-aware age comparison in planning; manager discovery receives min-release-age when needed by native resolver commands, but does not receive `now`.

#### Modules/Files Changed
- `<new-workspace>/crates/upnow-domain/src/plan.rs`
- `<new-workspace>/crates/upnow-domain/src/release.rs`
- `<new-workspace>/crates/upnow-planning/src/evaluate.rs`
- `<new-workspace>/crates/upnow-planning/src/planner.rs`
- `<new-workspace>/crates/upnow-execution/src/lib.rs`
- `<new-workspace>/crates/upnow-managers/src/adapter.rs`
- `<new-workspace>/crates/upnow-cli/src/lib.rs`

#### Tests Added
- Manager-selected target evaluation gates only the selected target.
- Manager-selected target evaluation does not choose a newer advisory/latest version.
- Manager-selected target missing required target evidence blocks only that item.
- Advisory latest metadata lookup failure does not block an otherwise valid manager-selected target.
- Resolver-native execution commands preserve their age constraint or typed age bypass.
- Grouped native execution reports item results for each selected item.
- Manager discovery does not receive `now`.

#### Validation
- `cargo test -p upnow-domain -p upnow-planning -p upnow-execution`
- `cargo test -p upnow-managers --test uv`
- `cargo test -p upnow-cli selected_uv`
- `cargo test -p upnow-cli uv_apply`

#### What Was Not Included
- No Mise or Brew manager implementation changes.
- No TUI selection changes.
- No visual changes.
- No new manager support.

#### Architectural Risks
Preserving manager-side planning by wrapping it in typed names. Manager-selected target mode must still leave policy and clock-aware age gates in planning.

#### Stop Conditions Met
The shared domain, planning, execution, adapter, and CLI boundaries can represent manager-selected targets without manager-side age planning or timeline trimming.

#### Review Checklist
- Manager-selected targets are evaluated as selected-target gates, not as trimmed timelines.
- Managers do not receive `now` to make release-age decisions.
- Managers do not trim timelines or evidence to steer generic planning.
- No planner code dispatches hidden uv, Mise, or Brew one-off branches when target-selection mode would express the rule.
- Forced/bypassed support exists only where a typed exact-target or resolver-native bypass command exists.

### Completed Manager Migration Status
All current managers have migrated batch scan/plan/apply coverage in the new architecture. Brew, uv, and Mise use the typed manager-selected target model instead of planner-selectable timeline workarounds. Manager-specific parsing, requests, command construction, policy support checks, min-age resolver arguments, and workarounds live inside concrete managers.

### Completed Refactorings After Manager-Selected Targets
- Managers now receive resolved `ManagerConfig` at construction and use it internally instead of receiving loose policy/min-age/`no_update` settings through adapter methods.
- npm-family and Python-family manager duplication is intentional when the logic belongs to independent concrete managers.
- Per-item execution support is represented by `ExecutionEligibility`; manager-level capabilities only advertise global native or resolver-native shortcuts.
- Manager-private execution command types were removed; managers now build shared `ExecutionCommand` values from `ResolvedExecutionPlan`.
- Interactive selected targets are typed as recommended, forced candidate, or alternate exact target.
- Pins are typed as package pins or manager-wide pins; persisted `*` represents the manager-wide pin.
- Broad fixture-shape tests were removed in favor of stable-boundary parser, manager adapter, planner, execution, config, and CLI tests.

### Tests Required
For each manager:
- Discovery parser tests.
- Outdated/native plan parser tests.
- Release lookup tests.
- Policy support tests.
- Execution command construction tests.
- Missing metadata blocks only that item.
- Manager-specific skip behavior.

Additional required tests for uv, Mise, and Brew:
- uv dry-run target is authoritative; PyPI/advisory latest must not replace it.
- uv may pass min-release-age to `--exclude-newer`, but manager discovery must not receive `now`.
- uv `tool list --outdated` latest is advisory metadata only.
- uv dry-run resolver failures are item-scoped resolver errors.
- Mise dry-run target is authoritative; `mise outdated --json` latest is advisory metadata only.
- Mise selected target publish-date metadata is required.
- Mise advisory latest metadata failure does not block the selected target.
- Brew outdated target is authoritative.
- Brew version policy gates the selected target only.
- Brew target age is based on tap git/GitHub commit evidence.
- Brew apply groups selected formulae and casks without using opaque selection indices.

### What Must Not Be Included Yet
- No TUI selection.
- No visual changes.
- No new manager support.

### Architectural Risks
Complex managers may tempt opaque metadata strings. Use manager-specific typed metadata instead.

### Stop Conditions
Stop each manager migration when it has scan/plan/apply batch behavior and no dependency on old architecture. This condition is met for current managers.

### Review Checklist
- Manager-specific workaround is named and isolated.
- Forced/bypassed support only where a typed exact-target or resolver-native bypass command exists.
- No opaque index/string control fields.
- Manager emits no UI output.
- Managers do not receive `now` to make release-age decisions.
- Managers do not trim timelines or evidence to steer generic planning.

## Phase 10: Interactive Selection Domain

### Goal
Add typed interactive selection behavior and view-model state without rendering.

### Behavior Delivered
Given an `UpdatePlan`, selection reducers can select, deselect, pin, unpin, choose typed selected targets, and return a typed `PlanSelection`. Existing typed domain pieces include `SelectedTarget` and package/global `PinTarget`; Phase 10 should build reducer behavior around those types rather than remodel execution.

### Modules/Files Likely Created Or Changed
- `<new-workspace>/crates/upnow-domain/src/selection.rs`
- `<new-workspace>/crates/upnow-planning/src/selection_view.rs`
- `<new-workspace>/crates/upnow-presentation/src/tui/selection_state.rs`

### Tests Required
- Default selected state excludes pinned items.
- Deselecting recommended item adds pin.
- Selecting pinned item removes pin.
- Global pin removal behavior.
- Forced candidate does not mutate pins.
- Alternate exact target marks selection as exact-required.
- Managers without exact execution expose no forced candidates.
- Alternate version choices are sourced from existing typed plan data or the phase stops for an explicit architecture decision.

### What Must Not Be Included Yet
- No terminal drawing.
- No crossterm/ratatui event loop.
- No execution progress UI.

### Architectural Risks
Using row indices as domain identity. Reducers should use stable typed ids. Another risk is inventing an alternate-version source that is not already represented by the typed plan.

### Stop Conditions
Stop when selection behavior is fully testable without a terminal. Stop earlier if alternate exact selection needs data that the current typed plan does not expose.

### Review Checklist
- Selection returns typed `PlanSelection`.
- No rendering strings are parsed.
- TUI state remains presentation-only.
- Pin persistence is not performed by selection state.

## Phase 11: Interactive Selection TUI

### Goal
Wire the tested selection domain into the TUI.

### Behavior Delivered
Interactive apply can display plans and return a typed confirmed selection. UX should preserve useful current behavior: tabs, all/manager views, select all/none, view all, version picker, force visibility.

### Modules/Files Likely Created Or Changed
- `<new-workspace>/crates/upnow-presentation/src/tui/mod.rs`
- `<new-workspace>/crates/upnow-presentation/src/tui/selection.rs`
- `<new-workspace>/crates/upnow-presentation/src/tui/components/...`
- `<new-workspace>/crates/upnow-cli/src/app.rs`

### Tests Required
- Event reducer tests.
- Snapshot or golden tests for view-model rows where practical.
- Confirm/cancel behavior.
- Planning error display behavior.
- No selectable updates behavior.

### What Must Not Be Included Yet
- No apply progress TUI.
- No renderer-driven domain decisions.
- No config persistence inside TUI.

### Architectural Risks
TUI may start owning domain state. Keep TUI state to tabs, cursor, modal, visible rows, and selected ids.

### Stop Conditions
Stop when interactive selection produces typed `PlanSelection` and no execution occurs from TUI closures.

### Review Checklist
- TUI does not mutate `UpdatePlan`.
- TUI does not persist pins.
- TUI does not run manager commands.
- View model is derived from domain plan.

## Phase 12: Interactive Apply Execution And Progress

### Goal
Execute confirmed interactive selections with typed progress reporting.

### Behavior Delivered
After user confirmation, pin changes persist, selected updates execute, and progress TUI shows pending/running/done/failed rows.

### Modules/Files Likely Created Or Changed
- `<new-workspace>/crates/upnow-execution/src/progress.rs`
- `<new-workspace>/crates/upnow-presentation/src/tui/progress.rs`
- `<new-workspace>/crates/upnow-cli/src/app.rs`
- `<new-workspace>/crates/upnow-cli/src/config.rs`

### Tests Required
- Pin persistence occurs after confirm and before execution.
- Per-item execution failure display.
- Manager-level execution failure display.
- Resolver-native execution commands preserve their age constraint or typed age bypass.
- Grouped native execution reports item results for each selected item.
- Stop-after-current behavior.
- Signal interruption maps to exit `130`.
- Successful execution summary.

### What Must Not Be Included Yet
- No new features.
- No UX redesign beyond preserving existing useful behavior.

### Architectural Risks
Progress UI may become executor. Executor should produce typed events/results; progress UI should render them.

### Stop Conditions
Stop when interactive apply shares batch planning/execution core and TUI state is presentation-only.

### Review Checklist
- Execution is not owned by TUI closures.
- Progress rows are derived from typed selected items.
- Failures attach to selected items or manager-level results.
- Outcome buffering is not used as state.

## Phase 13: Policy Config Finalization

### Goal
Record completed external config behavior for the collapsed no-policy mode.

### Behavior Delivered
Config supports one no-policy behavior internally. Missing policy defaults to no policy, `stable` and `same-track` parse, removed `any` is rejected, and unsupported policies are reported per manager before discovery.

### Modules/Files Changed
- `<new-workspace>/crates/upnow-cli/src/config.rs`
- `<new-workspace>/crates/upnow-domain/src/policy.rs`
- `<new-workspace>/crates/upnow-cli/tests/config.rs`

### Tests Added
- Missing policy defaults to no policy.
- `stable` parses.
- `same-track` parses.
- `any` is rejected.
- Unsupported policy per manager errors clearly.

### What Must Not Be Included Yet
- No new policies.
- No unrelated config migration.

### Architectural Risks
Breaking existing configs unintentionally.

### Stop Conditions
Stop when no duplicate no-policy concepts exist and migration behavior is explicit. This condition is met.

### Review Checklist
- One internal no-policy enum variant.
- External spelling is documented in tests.
- Error messages are actionable.
- Manager policy support remains capability-based.

## Phase 14: Old Architecture Removal And Replacement Cutover

### Goal
Make the new workspace the project implementation and remove old accidental complexity from active paths.

### Behavior Delivered
All commands run from the new architecture. Old `/src` is deleted, archived, or disconnected from builds according to project decision.

### Modules/Files Likely Created Or Changed
- root `Cargo.toml`
- old `/src` removal or archival path
- new workspace crates
- CI/build scripts
- README or command docs if present

### Tests Required
- Full new workspace test suite.
- End-to-end fake-manager tests for `scan`, `plan`, `apply`, and `apply --interactive`.
- Build/install smoke test.
- Regression tests for all migrated manager fixtures.

### What Must Not Be Included Yet
- No new managers.
- No feature expansion.
- No broad cosmetic rewrite.

### Architectural Risks
Leaving old crate paths active can hide accidental dependencies.

### Stop Conditions
Stop when the old implementation is no longer part of the active build and all intended behavior is covered by the new workspace.

### Review Checklist
- No `ManagerCtx`, old `ManagerPlugin`, `run_plan_apply_framework`, `PlannedUpdate`, `ApplyCandidate`, or `apply_spec_base`.
- No outcome emission during planning.
- No display-note parsing for decisions.
- Batch and TUI share planning/execution core.

## Phase 15: Final End-To-End Validation

### Goal
Validate the rebuilt architecture against the approved workflows.

### Behavior Delivered
The rebuilt project is ready to replace the old implementation.

### Modules/Files Likely Created Or Changed
- Integration tests.
- Final docs updates.
- Minor fixes across new crates.

### Tests Required
- `scan` with mixed managers.
- `plan` with update/current/delayed/blocked/skipped/error items.
- `apply` batch with pins and native shortcut.
- `apply --interactive` with pin changes, forced candidates, alternate versions, and cancellation.
- Release metadata failure blocks only the item.
- Manager-selected target plans preserve uv, Mise, and Brew selected targets.
- Manager-selected target advisory latest metadata does not replace selected targets.
- Missing command and unsupported platform behavior.
- Exit code behavior.

### What Must Not Be Included Yet
- No scope expansion.
- No new UX concepts.
- No hypothetical abstraction cleanup.

### Architectural Risks
Late discovery that a native shortcut does not match typed plan semantics.

### Stop Conditions
Stop when all approved workflows pass and behavior differences are intentional, reviewed, and documented.

### Review Checklist
- New architecture is layered.
- Domain model is typed.
- TUI state is presentation-only.
- Managers are isolated concrete adapters.
- Release and target-age lookup is explicit and testable.
- Manager-selected target planning is first-class and does not rely on trimmed timelines or hidden manager branches.
- Config pin persistence timing matches the approved decision.
