## Version Policy Gate Specification

### Purpose

Add an optional **version policy gate** to `upgate` that controls whether prerelease versions are eligible as upgrade targets.

This gate is **independent** of `min_release_age` and is evaluated during candidate selection.

The goal is to let users choose between:

* updating only to stable releases
* following the stability track of an already installed prerelease

This feature is configured **per manager** in `config.toml`.

---

## Goals

* Let users block prerelease upgrades by policy
* Support a safe “follow current track” workflow for users already on prereleases
* Preserve current behavior when the feature is not configured
* Keep the model manager-agnostic across SemVer-like and PEP 440 ecosystems
* Work cleanly with the existing delayed-upgrade model

---

## Non-goals

* Exact normalization of every versioning scheme in existence
* Complex user-defined match expressions for prerelease tags
* Per-package policy overrides in the first version
* Interactive per-item policy editing

---

# Config Model

## Per-manager configuration

Add a new optional field to each manager section:

```toml
[npm]
version_policy = "stable"
```

Supported values:

* `"stable"`
* `"same-track"`

If `version_policy` is not set, no version policy filtering is applied.

---

## Example config

```toml
[brew]
no_update = true
version_policy = "stable"

[npm]
version_policy = "stable"

[npm.selection]
mode = "include"
except = ["npm"]

[pipx]
version_policy = "same-track"
```

---

# Policy Semantics

## Release classes

`upgate` defines a normalized release stability ladder:

1. `dev`
2. `alpha`
3. `beta`
4. `rc`
5. `final`

Managers or registries map their version strings into one of these classes where possible.

Examples:

* PEP 440

  * `.devN` → `dev`
  * `aN` → `alpha`
  * `bN` → `beta`
  * `rcN` → `rc`
  * no prerelease segment → `final`

* SemVer-like tags

  * `-alpha`, `-alpha.1` → `alpha`
  * `-beta`, `-beta.2` → `beta`
  * `-rc`, `-rc.1` → `rc`
  * unknown prerelease labels may be classified best-effort or treated conservatively as non-final prereleases

If a candidate version cannot be classified reliably but is clearly a prerelease, it must not be treated as `final`.

---

## Policy values

### `stable`

Only `final` versions are eligible.

Behavior:

* installed stable → may update only to newer stable versions
* installed prerelease → may update only to newer stable versions

This is the conservative policy.

---

### `same-track`

Eligible target versions must be at least as stable as the installed version.

Rule by installed version class:

* installed `final` → eligible targets: `final`
* installed `rc` → eligible targets: `rc`, `final`
* installed `beta` → eligible targets: `beta`, `rc`, `final`
* installed `alpha` → eligible targets: `alpha`, `beta`, `rc`, `final`
* installed `dev` → eligible targets: `dev`, `alpha`, `beta`, `rc`, `final`

This policy is intended for users already on prereleases who want to continue forward without widening to less stable lanes.

---

# Candidate Selection

## Evaluation order

Use **Option A**:

1. Collect newer candidate versions
2. Apply the version policy filter
3. Apply the age gate (`min_release_age`)
4. Choose the remaining eligible version with the newest publish timestamp,
   using parsed version order only as a tie-breaker

This means version policy is a **filter on candidates**, not a secondary preference rule.

---

## Why this order

This preserves intuitive behavior:

* prereleases blocked by policy are ignored before age logic runs
* age gate applies only to policy-eligible versions
* target selection remains deterministic while avoiding abandoned higher-version
  prereleases when a more recently published eligible release exists

---

## Selection algorithm

For each installed item:

1. Determine installed version
2. Gather all newer available versions
3. If `version_policy` is unset:

   * do not filter by version policy
4. Else:

   * classify installed version
   * classify each candidate version
   * filter candidates according to policy
5. Among remaining candidates:

   * split into:

     * age-eligible
     * too-new
6. If at least one age-eligible candidate exists:

   * choose the age-eligible candidate with the newest publish timestamp
   * if publish timestamps tie, choose the highest parsed version
   * item status = `update`
7. Else if policy-eligible but only too-new candidates exist:

   * item status = `delayed`
8. Else if newer versions exist but none pass version policy:

   * item status = `current` with policy explanation
9. Else:

   * item status = `current`

---

# Interaction with `min_release_age`

Version policy and age are separate gates.

Example:

* installed: `1.2.0`
* available:

  * `1.3.0-beta.1` published 10 days ago
  * `1.2.5` published 2 days ago
* config:

  * `version_policy = "stable"`
  * `min_release_age = "7d"`

Result:

* `1.3.0-beta.1` is excluded by version policy
* `1.2.5` is excluded by age
* outcome = `delayed`

Reason:

* newer stable exists but is too new
* prerelease latest is blocked by policy

---

# Status Model

Plan item statuses include:

* `current`
* `update`
* `delayed`
* `blocked`
* `skipped`
* `error`

Version policy effects are reported through reason text and metadata.

---

## Status interpretation

### `update`

There is a newer candidate that passed version policy and age checks.

### `delayed`

There are newer candidates that passed version policy, but all are still too new.

### `current`

Either:

* installed version is effectively current under policy, or
* newer versions exist but all are blocked by version policy

### `blocked`

Used when the item cannot be safely resolved, such as missing release metadata.

### `skipped`

Used for other pre-existing skip reasons such as:

* not selected by selection policy
* disabled manager
* unsupported environment

Version policy alone should not produce `skipped`.

---

# Output Requirements

## Default output

Policy effects should appear as concise explanations where relevant. This
section extends the general output contract in `docs/spec.md`; it should not
expose raw resolver internals in normal output.

Examples:

```text
+ Update [npm] foo v1.2.0 -> v1.2.5 (v1.3.0-beta.1 blocked by policy)
```

```text
= Current [pipx] bar v2.0.0rc1 (v2.1.0 blocked by policy)
```

```text
~ Delayed [npm] baz v3.1.0 -> v3.1.1 (too fresh: 3d < 7d) (v4.0.0-beta.2 blocked by policy)
```

Exact symbols may follow existing style.

---

## Verbose output

`--verbose` may include policy evidence that is useful for debugging or
auditing a decision:

* installed release class
* chosen policy
* latest version overall
* latest policy-eligible version
* latest age-eligible version
* exclusion reason counts
* per-candidate policy and age eligibility, when available

Example:

```text
[npm] foo
  installed: 1.2.0 (final)
  policy: stable
  latest overall: 2.0.0-beta.2 (blocked by policy)
  latest eligible by policy: 1.3.0
  latest eligible by age: 1.2.5
  result: update
```

---

# Policy Classification

## Installed version classification

The installed version must be classified before evaluating `same-track`.

If the installed version cannot be classified:

* `stable` still works if final/non-final distinction is known
* `same-track` should degrade conservatively

Conservative fallback for `same-track`:

* if installed version is clearly final → treat as `final`
* if installed version class is `UnknownPrerelease` or `Unknown` → fall back to `stable` behavior with a warning

Current decision for this spec: `same-track` must not widen to a potentially less stable lane when installed class ordering is unknown. Fallback is conservative (`stable` semantics), not permissive.

---

## Unknown prerelease labels

For SemVer-like ecosystems, some tags may not map cleanly to `alpha`, `beta`, or `rc`.

Recommended handling:

* known labels map normally
* unknown non-final labels are treated as prereleases
* under `stable`, exclude them
* with no configured policy, do not filter them by version policy
* under `same-track`, allow only when classification is not less stable than the installed version, or conservatively exclude if relative ordering cannot be determined

This avoids accidentally treating custom prerelease labels as stable.

### SemVer heuristic classifier (pragmatic)

Current decision is to use a pragmatic keyword-based SemVer prerelease classifier with token normalization:

* lowercase prerelease identifiers
* split by `.`, then split tokens by `-` and `_`
* inspect leading alphabetic prefixes (`rc1` → `rc`, `exp2` → `exp`)
* evaluate all fragments and keep the least-stable matched class using this precedence:

  1. `dev`: `canary`, `nightly`, `snapshot`, `dev`, `devel`, `development`, `next`, `edge`, `preview`, `experimental`, `exp`
  2. `alpha`: `alpha`, `a`
  3. `beta`: `beta`, `b`
  4. `rc`: `prerelease`, `pre`, `rc`

This means token order must not change the final class (`rc.dev1` and `dev.rc1` both classify as `dev`).

If no known prerelease label matches but the version is still a prerelease, classify as `UnknownPrerelease` (never `Final`).

If parsing/classification fails entirely, classify as `Unknown` rather than throwing a hard classifier error.

---

# Manager-Agnostic Behavior

The policy model is intentionally generic.

Managers may differ in how much prerelease metadata they expose. Each manager implementation may support one of these levels:

### Full support

Installed and available versions can be classified accurately.

### Partial support

Final vs prerelease distinction is reliable, but subclassification (`alpha` vs `beta` vs `rc`) may be best-effort.

### Minimal support

Version policy may only reliably support:

* `stable`

If a manager cannot safely implement `same-track`, it should:

* reject that config value for the manager

Recommended approach: support `stable` wherever possible, and enable `same-track` only where classification is good enough.

### Gem

`gem` supports only `version_policy = "stable"`. This follows RubyGems'
native behavior: prereleases require explicit opt-in and are not selected by
normal update flows. For Gem, stable policy is target-safety oriented and does
not guarantee reporting prerelease-only newer versions as blocked.

### Unsupported

Some managers cannot safely combine this gate with their native target-selection model.

`mise` is unsupported for version policy. It owns target selection through `mise upgrade --before`,
so `upgate` must not replace that with its own exact-version resolver. Any configured
`[mise].version_policy` value is rejected.

`uv` is unsupported for version policy. It keeps its legacy behavior by delegating target
selection to `uv pip install --dry-run --exclude-newer` and applying with
`uv tool install --upgrade --exclude-newer`. Any configured `[uv].version_policy`
value is rejected.

---

# Interactive Apply

## Default behavior

In interactive `apply`:

* only items with status `update` are selected by default
* items excluded by version policy are not selected
* items with `delayed` remain non-updatable under normal selection flow

---

# CLI and Validation

## Config validation

Accepted values for `version_policy`:

* `none`
* `stable`
* `same-track`

Unset means no policy filtering. Older `any` configuration is rejected.

Example error:

```text
Invalid version_policy for [npm]: expected one of "none", "stable", "same-track", got "beta-only"
```

---

## Manager capability validation

If a manager does not support the configured policy safely:

Recommended behavior:

* report a clear manager-local error
* validate before running that manager

Example:

```text
! Error [manager] foo (version_policy "same-track" is not supported by this manager)
```

Prefer a clear explicit error over silently ignoring the policy.

---

# Examples

## Example 1: `stable`

Config:

```toml
[npm]
version_policy = "stable"
min_release_age = "7d"
```

Installed:

* `1.2.0`

Available:

* `1.2.5` published 10d ago
* `1.3.0-beta.1` published 20d ago

Result:

* update to `1.2.5`

Reason:

* beta is blocked by version policy

---

## Example 2: `same-track`

Config:

```toml
[pipx]
version_policy = "same-track"
```

Installed:

* `2.0.0b2`

Available:

* `2.0.0b3`
* `2.0.0rc1`
* `2.0.0`
* `2.1.0a1`

Eligible:

* `2.0.0b3`
* `2.0.0rc1`
* `2.0.0`

Blocked:

* `2.1.0a1`

Reason:

* `alpha` is less stable than current `beta` track

---

## Example 3: no policy configured

Config:

```toml
[npm]
# no version_policy set
```

Result:

* preserve current behavior
* no version policy filtering is applied

---

# Suggested Implementation Notes

## Internal representation

Use an internal enum such as:

```text
VersionPolicy:
- NoPolicy
- Stable
- SameTrack
```

And a normalized release class enum:

```text
ReleaseClass:
- Dev
- Alpha
- Beta
- Rc
- Final
- UnknownPrerelease
- Unknown
```

`UnknownPrerelease` is useful because it is safer than collapsing into `Unknown`.

Policy evaluation feeds typed plan items such as:

```text
- Update
- Delayed
- Current
- Blocked
```

Classifier API should be conservative and non-fallible at this boundary:

* parse/classify success -> return `ReleaseClass`
* parse/classify failure -> return `Unknown`

This avoids accidentally defeating conservative fallback via error propagation.

---

## Policy predicate

Implement a function conceptually like:

```text
is_candidate_allowed(installed_class, candidate_class, policy) -> decision
```

Semantics:

* `NoPolicy` → true
* `Stable` → candidate_class == Final
* `SameTrack` → candidate must be same or more stable than installed
  using ladder: Dev < Alpha < Beta < Rc < Final

In rank form:

```text
allow when candidate_rank >= installed_rank
```

for `same-track`.

---

## Candidate filtering

Implement policy filtering before age filtering.

That is the required behavior for this spec.

After policy and age filtering, target selection is publish-date first, not
highest-version first. This keeps the delayed-upgrade model biased toward
recently maintained releases. A higher-version prerelease that was published
earlier may lose to a lower-version release that was published more recently.
Use `version_policy = "stable"` when prereleases should be excluded entirely.

---

# Recommended UX wording

Use concise policy wording in normal output and TUI notes.

Avoid exposing low-level terms like “prerelease classifier” in user-facing text unless verbose/debug mode is enabled.

Preferred phrases:

* `blocked by policy`
* `only prereleases available; policy=stable`
* `following prerelease track`
* `policy=same-track`

---

# Summary

This feature adds a per-manager optional `version_policy` gate with three values:

* `none`
* `stable`
* `same-track`

Selection uses **Option A**:

* gather newer versions
* filter by version policy
* then filter by age
* choose the publish-date newest remaining candidate, using parsed version order
  as the tie-breaker

This keeps behavior predictable, works well with your existing delayed-upgrade model, and preserves backward compatibility when unset.

Resolver output is modeled as a recommendation plus per-candidate evaluation metadata so interactive flows can support exact manual target selection where the manager can execute an exact target.
