# UI Parity Migration Plan

## Goal

Migrate CLI output style and TUI UX from the old project at
`/Users/andrei/un-brew/src` into the new implementation at
`/Users/andrei/un-brew/new-workspace` while preserving the approved typed,
layered architecture.

The old project is UX/reference only. Do not copy old models, old workflow
wiring, stringly typed control state, global outcome buffering, manager-owned UI
decisions, TUI-owned apply closures, or legacy selection/pin mutation.

## Inputs

- `/tmp/parity1.md`
- `/tmp/parity2.md`
- `new-docs/architecture-handoff.md`
- `new-docs/implementation-plan.md`
- Current implementation under `new-workspace`
- Old CLI/TUI reference under `src`

## Architecture Rules For This Migration

- Keep durable facts in `upnow-domain`, `upnow-planning`, and
  `upnow-execution`.
- Keep UI view models and final render strings in `upnow-presentation` and the
  existing planning view-model boundary.
- Prefer typed adapters/mappers at architecture boundaries over leaking old
  models.
- Reuse old rendering/layout code only when it can be fed by typed new UI state.
- Do not copy old `ItemOutcome`, `ManagerCtx`, `ManagerPlugin::interactive_apply`,
  `PlannedUpdate`, `ApplyCandidate`, `ApplyCandidateVersion`,
  `apply_spec_base`, `chosen_versions`, or note-parsing flow.
- Do not mix CLI formatting migration and TUI interaction migration unless a
  typed boundary requires it.
- Each phase must be independently reviewable.

## Descisions Already Made
- Exact status mapping for release lookup failures in output rows: `! Error` 
- can verbose release-age notes should reuse `OutcomeNote`.
- Multi-manager batch output should render as one combined table
- Visual regression method for TUI parity: ratatui buffer snapshots

## Proposed Migration Phases

### Phase 1: Typed UI Boundary

Goal:
Establish the safe render/view contract before changing user-visible behavior.

Exact scope:
Add typed presentation inputs and pure formatting primitives for batch output and
shared CLI/TUI labels. Keep existing public rendering behavior intact where
possible.

Behavior delivered:
No CLI or TUI parity change yet. The codebase has typed UI-facing concepts that
later phases can map from domain/planning/execution without introducing old
models.

Files/modules likely touched:
- `new-workspace/crates/upnow-presentation/src/batch.rs`
- `new-workspace/crates/upnow-presentation/src/lib.rs`
- Possible new `new-workspace/crates/upnow-presentation/src/outcome.rs`
- Possible new `new-workspace/crates/upnow-presentation/src/theme.rs`
- Possible small adjustments in `new-workspace/crates/upnow-planning/src/selection_view.rs`

Old code used as reference or reused:
- Reference and selectively reuse pure logic from `src/outcome/render.rs`:
  version labels, changed-segment diffing, ANSI width stripping, adaptive column
  sizing.
- Reference `src/ui.rs` for color/plain/TTY behavior.
- Do not reuse `ItemOutcome` or global outcome buffering.

New typed concepts introduced, if any:
- `OutcomeTable`
- `OutcomeRow`
- `OutcomeStatusView`
- `OutcomeVersionsView`
- `OutcomeVisibility`
- `OutcomeNote`
- Explicit `OutputTheme`

Code likely deleted or simplified:
None required in this phase. Existing line renderers can remain as callers until
the table migration phase replaces them.

Tests required:
- Unit tests for version label normalization.
- Unit tests for changed version segment detection.
- Unit tests for ANSI width stripping and table width calculations.
- Unit tests for adaptive column omission.
- Unit tests for `OutputTheme` plain/color/no-color decisions.

Manual verification required:
None beyond tests. This phase should not change terminal output.

What must not be included yet:
- No CLI table output change.
- No TUI visual change.
- No TUI interaction change.
- No planner/domain diagnostic widening unless it is strictly required to define
  the typed boundary.

Stop conditions:
- The implementation needs old `ItemOutcome` or old outcome buffering.
- The implementation requires parsing display notes.
- The implementation creates a generic UI framework or renderer trait with only
  one implementation.
- A new presentation type must live in domain for reasons that are not durable
  facts.

Regression risks:
- Creating a presentation/planning dependency cycle.
- Putting UI-only concepts too low in domain.
- Hiding note strings in typed names without preserving their facts.

### Phase 2: Planning Diagnostics

Goal:
Preserve the durable facts needed for CLI notes, TUI row notes, and picker note
parts.

Exact scope:
Extend planning/domain output with typed facts for candidate evaluation, age
evidence, required age, policy warnings/block reasons, latest/advisory versions,
missing metadata, lookup failures, and resolver errors.

Behavior delivered:
Planning outputs enough typed information for presentation to render parity notes
without reconstructing decisions from strings.

Files/modules likely touched:
- `new-workspace/crates/upnow-domain/src/plan.rs`
- `new-workspace/crates/upnow-domain/src/release.rs`
- `new-workspace/crates/upnow-planning/src/evaluate.rs`
- `new-workspace/crates/upnow-planning/src/planner.rs`
- `new-workspace/crates/upnow-planning/tests/evaluate.rs`
- `new-workspace/crates/upnow-planning/tests/planner.rs`

Old code used as reference or reused:
- Reference `src/managers/shared/plan/decision.rs` and
  `src/managers/shared/plan/collect.rs` for the behavior of candidate facts.
- Do not copy old `PlanDecision`, `CandidateVersionMeta`, or note string
  construction.

New typed concepts introduced, if any:
- Typed candidate-evaluation facts, such as candidate version, release age,
  age eligibility, policy eligibility, policy block reason, and policy warning.
- Typed latest-too-fresh and target-too-fresh evidence.
- Typed required-age evidence.
- Typed missing-metadata and lookup-failure detail.
- Typed advisory/latest facts for manager-selected targets.

Code likely deleted or simplified:
- Later mappers should stop trying to derive note intent from coarse `PlanItem`
  variants alone.
- Any duplicated presentation reconstruction of policy/age decisions should be
  removed when the typed facts exist.

Tests required:
- Planner tests for latest-too-fresh notes.
- Planner tests for target-too-fresh notes.
- Planner tests for required-age facts.
- Planner tests for policy-blocked latest versions.
- Planner tests for policy warnings.
- Planner tests for missing metadata and lookup failure detail.
- Planner tests for manager-selected targets preserving selected-target semantics.
- Planner tests proving advisory latest metadata does not replace selected
  manager targets.

Manual verification required:
None beyond tests. This phase should not change terminal output.

What must not be included yet:
- No final note strings.
- No CLI table rendering.
- No TUI visual migration.
- No execution progress UI.

Stop conditions:
- Candidate facts can only be produced by parsing old display text.
- The change requires moving clock-aware age decisions into manager adapters.
- The change requires managers to trim timelines or synthesize UI-only metadata.

Regression risks:
- Over-modeling presentation text as domain state.
- Changing policy/age semantics while trying to expose diagnostics.
- Accidentally changing manager-selected target behavior for Brew, uv, or Mise.

### Phase 3: CLI Table Renderer

Goal:
Replace current line-oriented batch output with old-style adaptive outcome
tables.

Exact scope:
Map `ScanReport`, `UpdatePlan`, and `ExecutionReport` into `OutcomeTable` and
render old-style status labels, manager labels, optional columns, version labels,
version diffs, note text, verbose-only rows, and color/plain behavior.

Behavior delivered:
Batch `scan`, `plan`, and `apply` output uses old-style table formatting instead
of implementation-shaped lines.

Files/modules likely touched:
- `new-workspace/crates/upnow-presentation/src/batch.rs`
- `new-workspace/crates/upnow-presentation/src/lib.rs`
- `new-workspace/crates/upnow-cli/src/lib.rs`
- `new-workspace/crates/upnow-cli/src/main.rs`
- `new-workspace/crates/upnow-presentation/tests/*`
- `new-workspace/crates/upnow-cli/tests/batch_orchestration.rs`
- `new-workspace/crates/upnow-cli/tests/pnpm_batch.rs`

Old code used as reference or reused:
- Reuse/adapt pure rendering code from `src/outcome/render.rs`.
- Reference `src/outcome/types.rs` and `src/outcome/item.rs` only for old UX
  labels and note behavior.
- Reference `src/ui.rs` for color/plain/TTY rules.

New typed concepts introduced, if any:
- No new domain concepts expected beyond phases 1 and 2.
- A local `BatchRenderOptions` may be justified if it carries explicit theme and
  verbosity into render functions.

Code likely deleted or simplified:
- Current `scan npm`, `installed alpha 1.0.0`, `plan npm`,
  `update alpha 1.0.0 -> 1.2.0`, and `apply npm` line renderer paths.
- Tests that assert temporary line-oriented output should be replaced with table
  golden expectations.

Tests required:
- Golden tests for scan tables.
- Golden tests for plan tables with update/current/delayed/blocked/skipped/error
  rows.
- Golden tests for apply tables with success, failure, mutation-skipped, and no
  selected updates.
- Tests for current/no-newer rows hidden by default and shown under verbose.
- Tests for manager-level rows without fake item/version cells.
- Tests for version label normalization and changed-segment highlighting.
- Tests for color, `--plain`, `--no-color`, `NO_COLOR`, `TERM=dumb`, and non-TTY
  output behavior where practical.

Manual verification required:
- Run representative `scan`, `scan --verbose`, `plan`, and `apply` commands with
  fake or safe sources.
- Verify stdout contains only table output.
- Verify `--plain` and non-TTY output are readable.

What must not be included yet:
- No manager spinners.
- No TUI selection migration.
- No TUI progress migration.
- No new machine-readable mode unless separately approved.

Stop conditions:
- Renderer must inspect raw old models.
- Renderer must reconstruct policy/age decisions from display text.
- Table rendering requires changing planning or execution semantics.

Regression risks:
- Breaking CLI stdout/stderr separation.
- Breaking scripts that temporarily depended on line-oriented output.
- Rendering multi-manager output in a shape that differs from approved parity.

### Phase 4: CLI Terminal Affordances

Goal:
Restore stderr-only terminal behavior after stable table rendering exists.

Exact scope:
Add manager spinners, mutation-mode notices, spinner suspension while rendering,
and TTY/plain gating. Keep this as terminal plumbing, not application state.

Behavior delivered:
Interactive terminal users get old-style progress affordances for batch commands
without polluting non-TTY stdout.

Files/modules likely touched:
- `new-workspace/crates/upnow-cli/src/lib.rs`
- `new-workspace/crates/upnow-cli/src/main.rs`
- `new-workspace/crates/upnow-presentation/src/theme.rs` or equivalent
- Possible new terminal helper module under `upnow-presentation`

Old code used as reference or reused:
- Reference `src/ui.rs` for spinner and terminal suppression behavior.
- Reference `src/app/mod.rs` for where spinners and mutation notices appeared.

New typed concepts introduced, if any:
None expected. This should remain terminal plumbing.

Code likely deleted or simplified:
- Any temporary stdout notices that duplicate old stderr-only behavior.
- Any helper that makes renderers aware of manager execution flow.

Tests required:
- Tests or fakes proving spinners are disabled for plain/non-TTY output.
- Tests proving mutation notices go to stderr only.
- Tests proving stdout remains table-only.
- Existing exit-code tests should continue to pass.

Manual verification required:
- Run a batch command in a TTY and verify spinner behavior.
- Pipe output and verify no spinner/control characters appear.
- Verify `--plain`, `--no-color`, `NO_COLOR`, and `TERM=dumb` behavior manually
  if automated TTY coverage is limited.

What must not be included yet:
- No TUI work.
- No progress TUI.
- No global outcome buffering.
- No manager-emitted UI output.

Stop conditions:
- Implementation requires reintroducing old global output suppression as
  application state.
- Implementation requires managers to own terminal rendering.

Regression risks:
- Flaky terminal cleanup.
- Stderr noise in scripted runs.
- Spinner state leaking across manager failures.

### Phase 5: TUI Selection Data Parity

Goal:
Widen typed selection input before changing the selection TUI visuals.

Exact scope:
Expose typed row notes, default visibility, typed target options, candidate note
parts, violation flags, and typed per-manager planning status to the selection
presentation layer.

Behavior delivered:
The current selection reducer can represent the old TUI's visible rows, notes,
forced candidates, alternate targets, and loading/error placeholders without old
selection models.

Files/modules likely touched:
- `new-workspace/crates/upnow-planning/src/selection_view.rs`
- `new-workspace/crates/upnow-presentation/src/tui/selection_state.rs`
- `new-workspace/crates/upnow-presentation/src/tui/selection.rs`
- `new-workspace/crates/upnow-planning/tests/selection_view.rs`
- `new-workspace/crates/upnow-presentation/tests/selection_state.rs`
- `new-workspace/crates/upnow-presentation/tests/interactive_selection.rs`

Old code used as reference or reused:
- Reference `src/interactive/tui/selection/model.rs` for visibility and keyboard
  behavior.
- Reference `src/interactive/tui/selection/view.rs` for note and picker data
  needs.
- Do not copy `chosen_versions`, `ApplyCandidate`, or index-based state.

New typed concepts introduced, if any:
- Widened `SelectionView`.
- Widened `SelectionRow` with `default_visibility` and `notes`.
- `TargetOption`.
- `CandidateNotePart`.
- Typed manager planning status if precomputed planning needs an equivalent
  loading/error/done display.

Code likely deleted or simplified:
- Replace `forced_candidate_available` plus `alternate_exact_targets` with typed
  `TargetOption` values.
- Replace string `target_picker_options()` with typed picker options.
- Remove any selection display logic that infers violations from final strings.

Tests required:
- Include/skip selection policy tests must remain stable.
- Tests for recommended updates being selected/deselected by bulk `a`/`n`.
- Tests proving bulk `a`/`n` do not select force-candidate rows.
- Tests for forced candidates not mutating `UpdateSelectionPolicy`.
- Tests for alternate exact targets using typed target options.
- Tests for row visibility with current, delayed, blocked, skipped, resolver
  error, and selected hidden rows.
- Tests for typed note parts and violation flags.

Manual verification required:
None beyond reducer/view-model tests. Visual verification belongs to later
phases.

What must not be included yet:
- No ratatui visual port.
- No modal picker visual port.
- No execution progress UI.
- No config persistence inside TUI.

Stop conditions:
- The reducer needs old `chosen_versions`.
- The view model needs old `ApplyCandidate` or `ApplyCandidateVersion`.
- The TUI needs to mutate `UpdatePlan`.
- The TUI needs to persist config or run manager commands.

Regression risks:
- Accidentally changing persisted selection-policy semantics.
- Treating row indices as durable identity.
- Making force selection behave like selection policy mutation.

### Phase 6: TUI Selection Visual Shell

Goal:
Port the old fullscreen selection hierarchy onto the new typed state.

Exact scope:
Add old-style frame, tabs, footer keycaps, separators, scrollbars, table sizing,
theme, note column, selected-row highlighting, compact-terminal handling, and
version diff styling for selection rows.

Behavior delivered:
Interactive selection visually resembles the old fullscreen TUI while still
returning typed `PlanSelection` through the existing reducer flow.

Files/modules likely touched:
- New `new-workspace/crates/upnow-presentation/src/tui/components/*`
- Possible new `new-workspace/crates/upnow-presentation/src/tui/layout.rs`
- Possible new `new-workspace/crates/upnow-presentation/src/tui/text.rs`
- Possible new `new-workspace/crates/upnow-presentation/src/tui/theme.rs`
- `new-workspace/crates/upnow-presentation/src/tui/selection.rs`
- `new-workspace/crates/upnow-presentation/src/tui/mod.rs`

Old code used as reference or reused:
- Reuse/adapt visual helpers from `src/interactive/tui/components/`.
- Reuse/adapt layout helpers from `src/interactive/tui/layout.rs`.
- Reuse/adapt text helpers from `src/interactive/tui/text.rs`.
- Reuse/adapt theme from `src/interactive/tui/theme.rs`.
- Reference `src/interactive/tui/selection/view.rs` for final layout.

New typed concepts introduced, if any:
- `TuiTheme`.
- Local visible-row cache and tab offset state if needed for rendering.
- No new durable domain concepts expected.

Code likely deleted or simplified:
- Current basic bordered `Tabs`, `Table`, and footer drawing in
  `tui/selection.rs`.
- Any duplicated current row rendering that conflicts with shared TUI
  components.

Tests required:
- Ratatui buffer/snapshot tests for table rows where practical.
- Unit tests for tab overflow calculations.
- Unit tests for truncation and version diff spans.
- Tests for small-terminal placeholder behavior.

Manual verification required:
- Run interactive selection in a normal terminal.
- Verify All tab and per-manager tabs.
- Verify footer keycaps.
- Verify view-all mode.
- Verify narrow and tiny terminal behavior.
- Verify color and plain mode if supported by the terminal theme.

What must not be included yet:
- No modal target picker parity.
- No live progress TUI.
- No execution changes.
- No TUI-owned domain decisions.

Stop conditions:
- Visual code wants old `PlannedUpdate`, old `ApplyCandidate`, or old note
  strings.
- Visual shell changes require moving selection rules out of reducers.

Regression risks:
- Layout overflow or unreadable notes.
- Keyboard focus and cursor state drifting from visible rows.
- Color/plain mismatch with CLI theme.

### Phase 7: TUI Picker And Keyboard Parity

Goal:
Restore old selection interaction polish without changing ownership boundaries.

Exact scope:
Add old-style modal target picker, note-part styling, wrapping cursor movement,
picker navigation, `r` recommended behavior, Esc cancel behavior, and tab
overflow hints where useful.

Behavior delivered:
Interactive selection supports the old modal picker and keyboard model while
using typed `SelectedTarget` values.

Files/modules likely touched:
- `new-workspace/crates/upnow-presentation/src/tui/selection.rs`
- `new-workspace/crates/upnow-presentation/src/tui/components/modal.rs`
- `new-workspace/crates/upnow-presentation/src/tui/components/table.rs`
- `new-workspace/crates/upnow-presentation/tests/interactive_selection.rs`
- `new-workspace/crates/upnow-presentation/tests/selection_state.rs`

Old code used as reference or reused:
- Reference `src/interactive/tui/selection/events.rs`.
- Reference `src/interactive/tui/selection/model.rs`.
- Reference `src/interactive/tui/selection/view.rs`.
- Do not copy old index-based selected-version state.

New typed concepts introduced, if any:
None expected beyond Phase 5 target options and note parts.

Code likely deleted or simplified:
- Current footer-only target picker display.
- Current non-wrapping cursor and picker movement logic if it differs from the
  approved parity target.

Tests required:
- Picker open/confirm/cancel tests.
- Picker movement wrap tests.
- Recommended target shortcut tests.
- Forced candidate picker tests.
- Alternate exact target picker tests.
- Bulk `a`/`n` regression tests proving only recommended update rows are
  affected.
- Cancel and global quit behavior tests.

Manual verification required:
- Open interactive selection and verify modal picker layout.
- Verify row movement and picker movement.
- Verify `space`, `x`, `a`, `n`, `v`, `Enter`, `r`, `C`, `q`, Esc, Tab, and
  BackTab behavior.

What must not be included yet:
- No live progress TUI.
- No config persistence inside TUI.
- No manager command execution from TUI.
- No mutation of `UpdatePlan`.

Stop conditions:
- TUI needs display-note parsing to identify forced or violation options.
- TUI needs to own execution or persistence.
- Implementation requires broad UI framework abstractions.

Regression risks:
- Wrong selected target emitted.
- Force candidates mutating selection policy.
- Hidden-row cursor bugs after toggling view-all.

### Phase 8: Live Progress TUI

Goal:
Replace post-execution text progress with old-style live progress UX.

Exact scope:
Render typed `ExecutionProgressState` in fullscreen mode with progress rows,
spinner, success/failure/skipped states, manager failure display, summary, and
quit-after-current confirmation.

Behavior delivered:
`apply --interactive` executes confirmed selections with a live progress TUI
instead of returning plain text progress after execution completes.

Files/modules likely touched:
- `new-workspace/crates/upnow-execution/src/progress.rs`
- `new-workspace/crates/upnow-presentation/src/tui/progress.rs`
- `new-workspace/crates/upnow-presentation/src/tui/mod.rs`
- `new-workspace/crates/upnow-cli/src/lib.rs`
- `new-workspace/crates/upnow-execution/tests/progress.rs`
- `new-workspace/crates/upnow-presentation/tests/progress.rs`
- `new-workspace/crates/upnow-cli/tests/interactive_apply.rs`

Old code used as reference or reused:
- Reference visual parts of `src/interactive/tui/progress.rs`.
- Reuse/adapt terminal shell ideas from `src/interactive/tui/terminal.rs`.
- Reuse shared TUI components from Phase 6.
- Do not reuse old `ApplyProgressTask`, apply closures, outcome draining, or
  old failure inference.

New typed concepts introduced, if any:
- Local progress screen state for spinner tick and quit confirmation.
- Possibly typed progress input/control events if needed to connect CLI execution
  to the renderer.

Code likely deleted or simplified:
- Current interactive path returning `render_progress_state(&report.progress)` as
  the primary UX.
- Plain progress renderer may remain as a test/fallback helper only if useful.

Tests required:
- Execution progress reducer tests for pending/running/succeeded/failed/skipped.
- Tests for manager failure continuation.
- Tests for stop-after-current setting skipped pending rows.
- Tests for quit confirmation controls.
- Tests that selection policy persists after confirmation and before execution.
- Tests that signal interruption maps to exit code 130.
- Tests that execution failures attach to selected items or manager-level rows.

Manual verification required:
- Run interactive apply success flow.
- Run interactive apply with a command failure.
- Run interactive apply with a manager command-construction failure.
- Verify quit-after-current confirmation.
- Verify terminal cleanup after success, failure, cancel, and interruption.

What must not be included yet:
- No new execution model.
- No manager migration.
- No TUI-owned apply closures.
- No old outcome-drain failure mapping.

Stop conditions:
- TUI needs to own manager execution closures.
- Progress failure mapping requires drained `ItemOutcome` rows.
- Progress UI needs manager-private command types.

Regression risks:
- Terminal raw-mode/alternate-screen cleanup failures.
- Cancellation timing races.
- Progress rows drifting from typed execution results.
- Persisting selection policy at the wrong time.

### Phase 9: Parity Cleanup

Goal:
Remove transitional UI paths and lock the reviewed parity surface.

Exact scope:
Delete obsolete line-render helpers/tests, update golden tests, and document any
accepted behavior differences.

Behavior delivered:
The new implementation has one active CLI/TUI parity path, stable tests, and no
temporary migration adapters that obscure ownership boundaries.

Files/modules likely touched:
- `new-workspace/crates/upnow-presentation/src/batch.rs`
- `new-workspace/crates/upnow-presentation/src/tui/*`
- `new-workspace/crates/upnow-cli/tests/*`
- `new-workspace/crates/upnow-presentation/tests/*`
- `new-docs/architecture-handoff.md` only if a handoff update is desired.

Old code used as reference or reused:
Old `/Users/andrei/un-brew/src` remains reference only.

New typed concepts introduced, if any:
None.

Code likely deleted or simplified:
- Temporary line-oriented render helpers.
- Temporary typed adapters with no caller after parity migration.
- Tests that only lock incidental intermediate structure.

Tests required:
- Full workspace test suite.
- CLI golden tests for scan, plan, apply.
- TUI reducer and visual tests.
- Interactive apply tests for selection, persistence, execution, and progress.

Manual verification required:
- CLI smoke for `scan`, `scan --verbose`, `plan`, `apply`.
- Interactive selection smoke.
- Interactive progress smoke.
- Plain/no-color/non-TTY smoke where practical.

What must not be included yet:
- No feature expansion.
- No new managers.
- No unrelated architecture cleanup.

Stop conditions:
- Cleanup would hide an unresolved parity difference.
- Cleanup requires deleting an active typed boundary before its replacement is
  reviewed.

Regression risks:
- Broad golden updates masking behavior changes.
- Removing fallback code before manual TUI verification is complete.

## Recommended First Phase

Start with Phase 1: typed UI boundary plus pure render primitives.

## Why This Phase Should Go First

Phase 1 is the smallest phase that unlocks safe parity work. It creates the
typed adapter surface that CLI and TUI rendering can consume, imports only pure
old rendering behavior, and does not touch command orchestration, manager
adapters, execution, config persistence, or terminal interaction.

It also forces the most important architecture question early: durable facts must
come from domain/planning/execution, while strings stay at the final render
boundary.

## Risks Before Starting

- Current planning drops some candidate-evaluation detail, so full CLI note and
  TUI picker parity is blocked until Phase 2.
- Current CLI returns a single stdout string, while old parity also needs
  stderr-only terminal affordances.
- Current TUI selection state is typed but too thin for notes, target option
  explanations, and modal picker parity.
- The progress TUI is the riskiest later phase because it needs live typed events
  without reviving old closure-owned apply wiring.
- Multi-manager batch output shape still needs a presentation decision before
  golden tests are finalized.

## Decisions Still Needed Before Implementation

- Exact typed display for precomputed planning status in interactive mode.
