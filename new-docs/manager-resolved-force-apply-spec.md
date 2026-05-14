# Manager-Resolved Force Apply Spec

## Purpose

This spec defines the architecture changes needed to let users intentionally update a real tool row that was not selected by normal gates, including rows where upnow does not know the final target version.

The core decision is that apply selection is not always a target-version selection. Sometimes the user selects an update action and delegates final target choice to the manager at execution time.

This document is binding for the implementation phase. The implementation may refactor existing types freely. There is no internal backwards-compatibility requirement.

## Problem

The current model assumes operable rows have a resolved target version. That works for exact installs and for manager-selected candidates with known target evidence, but it fails for two valid user intents:

- Force a known candidate blocked by upnow gates, such as a too-fresh `mise` or `uv` candidate.
- Run a manager-selected update command for a real tool even when upnow cannot resolve or display the final target version.

The current `ExecutionEligibility::ResolverNativeOnly` also mixes command shape with policy constraint. Resolver-native is an execution shape. Whether the command includes `--before`, `--exclude-newer`, or no age limiter is an execution constraint, not a different command intent.

## Decisions

### Replace `SelectedTarget`

Replace `SelectedTarget` with:

```rust
pub enum SelectedUpdate {
    Recommended,
    ForcePlannedCandidate,
    Exact { target_version: VersionText },
    ManagerResolved,
}
```

Meanings:

- `Recommended`: execute the plan's recommended update exactly as normal apply would.
- `ForcePlannedCandidate`: execute the candidate that planning already knows about, bypassing the gate that made it ineligible, when the manager has typed support for that bypass.
- `Exact`: execute a specific target version chosen from typed exact target options.
- `ManagerResolved`: execute the manager's selected update action for this tool and let the manager resolve the final target at execution time.

Do not preserve `SelectedTarget` as a compatibility alias. Rename all domain, execution, presentation, TUI, and tests to the new concept.

### Separate Command Shape From Target Knowledge

Execution command intents remain command-shape oriented:

```rust
Exact(item)
NativeSelected(item)
GroupedNative(items)
NativeGlobal(items)
ResolverNative(item)
ResolverNativeGlobal(items)
```

Do not add a separate intent for "resolver native with age bypass." Use `ResolverNative(item)` and carry bypass/target knowledge on the resolved item.

### Represent Targetless Execution

`ResolvedExecutionItem` must not require `target_version: VersionText` for every selected update. Replace it with a typed target value, for example:

```rust
pub enum ResolvedExecutionTarget {
    Known(VersionText),
    ManagerResolved,
}
```

or an equivalent model. Do not fake unknown targets with `"-"`, empty strings, current version, or advisory latest.

Execution reports and progress rows must also handle `ManagerResolved` without inventing a version. Presentation can display `manager-resolved`, `manager selected`, or another deliberate label, but it must be derived from the typed target state.

### Refactor Execution Eligibility

Replace `ExecutionEligibility` variants that encode mixed concepts with command-shape capabilities. The implementation should be direct and explicit, not framework-like.

The domain must be able to answer these questions per item:

- Can this item execute an exact target?
- Can this item execute selected native update for this tool?
- Can this item execute selected resolver-native update for this tool?
- Can this item participate in grouped native update?
- Can this item participate in native global update?
- Can this item participate in resolver-native global update?
- Can selected execution bypass min release age?
- Can selected execution run with a manager-resolved target?

A simple struct is preferred if it keeps the model clear:

```rust
pub struct ExecutionSupport {
    pub exact: bool,
    pub native_selected: bool,
    pub native_global: bool,
    pub grouped_native: bool,
    pub resolver_native_selected: ResolverNativeSupport,
    pub resolver_native_global: bool,
}

pub struct ResolverNativeSupport {
    pub selected: bool,
    pub min_age_constraint: MinAgeConstraintSupport,
    pub manager_resolved_target: bool,
}

pub enum MinAgeConstraintSupport {
    NotApplicable,
    Required,
    Optional,
}
```

This is illustrative, not mandatory. The required behavior is that resolver-native command shape and age-gate bypass support are typed separately.

Avoid generic registries, manager families, or hidden metadata-key control flow.

## Apply Shapes

The implementation must support these per-item shapes where the manager supports them:

1. Exact target:

   ```text
   manager update tool@version
   ```

2. Resolver-native with configured age gate:

   ```text
   manager update tool --before 7d
   ```

   or manager equivalent such as `--exclude-newer`.

3. Resolver-native without the age gate:

   ```text
   manager update tool
   ```

4. Native selected manager-resolved update:

   ```text
   manager update tool
   ```

   This is the non-resolver-native equivalent for managers that have a selected update command but no exact target for this selection.

Global and grouped forms remain allowed only when typed selection semantics prove they are equivalent to the selected plan.

## Selection Resolution Rules

Execution selection must resolve as follows.

### `Recommended`

Allowed only for normal `PlanItem::Update`.

Resolution:

- Use exact, native selected, grouped native, native global, resolver-native selected, or resolver-native global according to existing command-shape rules.
- Do not bypass min release age.
- Do not use `ManagerResolved` target unless the plan's recommended item is explicitly manager-resolved.

### `ForcePlannedCandidate`

Allowed for real plan items that contain a known candidate or manager-selected target blocked by upnow gates.

Examples:

- `PlanItem::Delayed` because target age is below `min_release_age`.
- `PlanItem::Blocked` by version policy when the selected forced action is explicitly allowed by manager support.

Resolution:

- If exact execution is supported, resolve to `Exact` with known target and `bypass_min_release_age = true` when that target failed the age gate.
- If resolver-native selected execution is supported and min-age constraint is optional, resolve to `ResolverNative` with known target and `bypass_min_release_age = true`.
- If native selected execution is supported and the manager can update the selected tool without an exact target, resolve to `NativeSelected` with either known candidate target or `ManagerResolved`, depending on the item's known target state.
- Otherwise reject as non-executable.

Do not use string parsing or display notes to infer forceability.

### `Exact`

Allowed only when the item has typed exact target support and the target option was produced by planning/domain data.

Resolution:

- Resolve to `Exact`.
- Set `bypass_min_release_age = true` only if the selected exact target is known to fail the age gate.
- Reject when exact execution is unsupported.

### `ManagerResolved`

Allowed when the row is a real installed/update item and the manager exposes typed selected update support that does not require a known target version.

Resolution:

- Prefer `ResolverNative` when the manager-selected resolver is authoritative for this manager.
- Use `NativeSelected` when the manager has a selected native update command.
- Use target `ResolvedExecutionTarget::ManagerResolved`.
- Set `bypass_min_release_age = true` when the action is force-applying a row blocked only by upnow's min-release-age gate and the manager command can omit the age limiter.
- Reject when selected manager-resolved execution is unsupported.

`ManagerResolved` must never be available for placeholders, loading rows, manager-level errors, resolver errors with no executable installed item, or rows that exist only as presentation artifacts.

## Planning Changes

Planning must preserve executable manager facts even when target metadata is missing or no final target version can be resolved.

Current wrong rule:

```text
No resolved target version => row is not operable
```

New rule:

```text
A row is operable when it represents a real installed/update item and the manager exposes a typed execution shape for the selected action, even if the final target is manager-resolved at execution time.
```

Planning should distinguish:

- No target and no executable selected update path: non-operable.
- No target but manager supports selected manager-resolved update: operable via `SelectedUpdate::ManagerResolved`.
- Known target blocked by age/policy gates: operable via `ForcePlannedCandidate` if supported.
- Known target with exact alternatives: operable via `Exact`.

If current `PlanItem` variants cannot carry this without ambiguity, refactor them. Do not add side-channel metadata to presentation models.

Acceptable plan representation options include:

- Add an executable blocked/skipped item variant.
- Add an optional `ManagerResolvedUpdateCandidate` to existing blocked/delayed outcomes.
- Generalize candidate target to `PlannedTarget::{Known(VersionText), ManagerResolved}`.

Pick the smallest model that makes invalid states clear. Do not preserve the old target-version-required shape if it causes fake target values or UI special cases.

## Manager Requirements

Each manager must explicitly state which selected update shapes it supports. The next implementation must audit and wire all managers, not only `mise` and `uv`.

### `mise`

Supported:

- Resolver-native selected with age gate:

  ```text
  mise upgrade --before <age> <tool>
  ```

- Resolver-native selected without age gate:

  ```text
  mise upgrade <tool>
  ```

- Resolver-native global with age gate:

  ```text
  mise upgrade --before <age>
  ```

Global force without the age gate must be implemented only if the selected set is exactly equivalent to all forceable manager-resolved candidates and the manager command is confirmed safe. Otherwise use per-item commands.

`mise` should support `ForcePlannedCandidate` for known too-fresh manager-selected targets and `ManagerResolved` for real tool rows where no final target version is known but `mise upgrade <tool>` is valid.

### `uv`

Supported:

- Resolver-native selected with age gate:

  ```text
  uv tool install --upgrade --exclude-newer <age> <tool>
  ```

- Resolver-native selected without age gate:

  ```text
  uv tool install --upgrade <tool>
  ```

`uv` should support `ForcePlannedCandidate` for known too-fresh manager-selected targets and `ManagerResolved` where the tool is real and `uv tool install --upgrade <tool>` is valid.

### `npm`

Currently supports exact and selected native update shapes.

Supported:

- Exact:

  ```text
  npm install -g <package>@<version>
  ```

- Selected native:

  ```text
  npm update -g <package>
  ```

`npm` can support `ManagerResolved` through selected native update when no exact target is known and the package is a real installed global package. Exact remains preferred when a known target is selected or needed for policy/age correctness.

### `bun`

Currently supports exact and native global shapes. Only wire `ManagerResolved` if Bun has a real selected global update command for one package in the current implementation. If not, keep targetless rows non-operable for Bun unless the selected set safely maps to native global.

### `brew`

Currently uses Homebrew-selected targets and native/grouped command shapes.

Supported:

- Native/grouped formula and cask upgrades.

Brew can support manager-resolved selected update when the row maps to a real formula/cask and `brew upgrade <formula>` or `brew upgrade --cask <cask>` is the typed command. It must preserve `no_update`, dependency filtering, formula/cask kind, and grouped command safety.

Do not force Brew through exact target semantics.

### Exact-Only Managers

Current exact-only managers are:

- `cargo`
- `dotnet`
- `gem`
- `go`
- `pipx`
- `pnpm`
- `yarn`

These managers should keep exact execution for known targets. Do not expose `ManagerResolved` unless the implementation explicitly adds and verifies a selected manager-native update command for that manager.

If a manager command exists, add it as a typed manager capability and command builder branch. Do not infer it from package names or old display strings.

## Presentation And TUI

The TUI must use typed operability derived from the plan/view model.

Rows visible through "view all" must be fully operable when they represent real selectable update actions:

- Enter opens details for any visible real row with one or more typed actions.
- Selection toggles or details confirm must produce `SelectedUpdate`, not display-derived state.
- Loading rows, placeholder rows, manager-level errors, and non-executable resolver errors remain non-operable.

Details must show action choices, not only target choices. Examples:

- Recommended update
- Force planned candidate
- Install exact version `<version>`
- Let manager resolve target

The Target column should not use `"-"` for manager-resolved executable rows if that makes them look unavailable. Use a typed display label such as `manager-resolved`, `manager selected`, or `resolver`. Non-operable missing target rows may still display a separate clear value such as `unavailable`.

Selection policy persistence remains package-based and must not encode target choices. `SelectedUpdate` belongs to the current confirmation only.

## Batch Apply

Batch apply should continue to select only recommended updates according to configured selection policy.

Do not automatically force ineligible rows in batch mode because force is an explicit user action. If a future CLI flag adds this behavior, it must produce the same typed `SelectedUpdate` values as the TUI.

## Execution Reports

Reports and progress must represent unknown final targets honestly.

Required behavior:

- Known exact/planned targets display the known target version.
- Manager-resolved executions display a typed manager-resolved target label.
- Do not report current version as target.
- Do not report advisory latest as target unless it is the actual selected exact/planned target.

If post-execution scan later discovers the installed version, that is a separate future feature and must not be faked in this implementation.

## Tests

Add or keep tests only at stable behavior boundaries.

Required coverage:

- Domain/selection rejects selected updates for unknown plan items.
- Execution resolution:
  - `ForcePlannedCandidate` with exact support resolves to exact and bypasses age when appropriate.
  - `ForcePlannedCandidate` with resolver-native optional age constraint resolves to `ResolverNative` and bypasses age.
  - `ManagerResolved` with resolver-native support resolves to `ResolverNative` with `ResolvedExecutionTarget::ManagerResolved`.
  - `ManagerResolved` with native selected support resolves to `NativeSelected`.
  - Unsupported targetless selected update is rejected.
- Manager command construction:
  - `mise` resolver-native normal includes `--before`.
  - `mise` resolver-native forced omits `--before`.
  - `uv` resolver-native normal includes `--exclude-newer`.
  - `uv` resolver-native forced omits `--exclude-newer`.
  - `npm` manager-resolved selected update builds selected native update, if wired.
  - Brew manager-resolved selected update preserves formula/cask command shape, if wired.
- Presentation/TUI:
  - A visible "view all" real row with manager-resolved action opens details.
  - Details can confirm `ManagerResolved`.
  - Details can confirm `ForcePlannedCandidate`.
  - Placeholder/loading/error rows do not open details and cannot be selected.
  - Rows with no known target but typed selected update support are displayed as operable, not as a plain `-` unavailable row.

Do not add tests for private helper plumbing or current view-model field layout.

## Implementation Order

1. Refactor domain selection from `SelectedTarget` to `SelectedUpdate`.
2. Refactor execution target representation so resolved execution items can be `Known(version)` or `ManagerResolved`.
3. Refactor execution eligibility/support so command shape, age-bypass support, and manager-resolved support are separate typed facts.
4. Update planning to preserve forceable manager-resolved update actions instead of dropping them into non-operable missing-target rows.
5. Update execution resolution rules for all `SelectedUpdate` variants.
6. Update manager command builders and manager support declarations.
7. Update presentation view models and TUI details/selection behavior.
8. Update batch apply to continue selecting recommended updates only.
9. Add stable-boundary tests listed above.
10. Run focused tests, then `cargo test --workspace` from `/Users/andrei/un-brew/new-workspace`.

## Rejected Approaches

- Do not preserve `SelectedTarget` and add another side enum.
- Do not represent unknown targets with `"-"`, empty strings, current versions, or advisory latest versions.
- Do not parse displayed target text to decide operability.
- Do not let the TUI decide whether a manager command is valid.
- Do not add one-off `mise` or `uv` branches outside typed manager support.
- Do not add `ResolverNativeWithAgeBypass` as a separate command intent.
- Do not keep `ResolverNativeOnly` if it continues to mix command shape and age constraint.
- Do not make batch apply force ineligible rows by default.
- Do not add compatibility shims for the old selection model.

## Open Verification Points

Before implementation, verify manager command parity against old behavior for any manager where targetless selected update is newly enabled:

- Exact command syntax.
- Selected native command syntax.
- Resolver-native constrained command syntax.
- Resolver-native unconstrained command syntax.
- Global/grouped command safety.
- Whether the manager command updates only the requested tool or can mutate unrelated tools.

If a manager cannot prove selected targetless update is scoped to the requested tool, do not expose `ManagerResolved` for that manager.
