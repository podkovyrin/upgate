## Version Policy Gate Specification

### Purpose

Add an optional **version policy gate** to `upnow` that controls whether prerelease versions are eligible as upgrade targets.

This gate is **independent** of `min_release_age` and is evaluated during candidate selection.

The goal is to let users choose between:

* updating only to stable releases
* following the stability track of an already installed prerelease
* allowing any newer version, including prereleases

This feature is configured **per manager** in `config.toml` and may later support package-level overrides.

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
* `"any"`

If `version_policy` is not set, the version policy gate is **disabled** for that manager and current behavior is preserved.

---

## Example config

```toml
[brew]
no_update = true
pinned = ["aom", "docker"]
version_policy = "stable"

[npm]
pinned = ["npm"]
version_policy = "stable"

[pipx]
version_policy = "same-track"

[mise]
version_policy = "any"
```

---

## Future-compatible extension

The config shape should allow nested overrides later, for example:

```toml
[npm]
version_policy = "stable"

[npm.packages."typescript-nightly"]
version_policy = "any"
```

Package-level overrides are not required for the first version.

---

# Policy Semantics

## Release classes

`upnow` defines a normalized release stability ladder:

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

### `any`

Any newer version is eligible, including prereleases.

This is the least restrictive policy.

---

# Candidate Selection

## Evaluation order

Use **Option A**:

1. Collect newer candidate versions
2. Apply the version policy filter
3. Apply the age gate (`min_release_age`)
4. Choose the newest remaining eligible version

This means version policy is a **filter on candidates**, not a secondary preference rule.

---

## Why this order

This preserves intuitive behavior:

* prereleases blocked by policy are ignored before age logic runs
* age gate applies only to policy-eligible versions
* target selection remains deterministic

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

   * choose the newest age-eligible candidate
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

No new top-level statuses are required.

Existing statuses remain:

* `current`
* `update`
* `delayed`
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

### `skipped`

Used for other pre-existing skip reasons such as:

* pinned
* disabled manager
* missing metadata
* unsupported environment

Version policy alone should not produce `skipped`.

---

# Output Requirements

## Default output

Policy effects should appear as concise explanations where relevant.

Examples:

```text
+ Update [npm] foo v1.2.0 → v1.2.5 (latest v1.3.0-beta.1 blocked by version policy)
```

```text
= Current [pipx] bar v2.0.0rc1 (newer prereleases blocked by version policy: stable)
```

```text
~ Delayed [npm] baz v3.1.0 → v3.1.1 (3d < 7d; latest v4.0.0-beta.2 blocked by version policy)
```

Exact symbols may follow existing style.

---

## Verbose output

`--verbose` may include:

* installed release class
* chosen policy
* latest version overall
* latest policy-eligible version
* latest age-eligible version
* exclusion reason counts

Example:

```text
[npm] foo
  installed: 1.2.0 (final)
  policy: stable
  latest overall: 2.0.0-beta.2 (blocked by version policy)
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
* under `any`, allow them
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
* `any`

If a manager cannot safely implement `same-track`, it should:

* reject that config value for the manager

Recommended approach: support `stable` and `any` wherever possible, and enable `same-track` only where classification is good enough.

---

# Interactive Apply

## Default behavior

In `apply --interactive`:

* only items with status `update` are selected by default
* items excluded by version policy are not selected
* items with `delayed` remain non-updatable under normal selection flow

---

## Override-ready design

One-run override is still a future UX capability, but the resolver model is intentionally designed so it can be added without redesign.

### Recommendation vs final decision

Resolver output is a recommendation, not the final apply decision.

Recommended states:

* `Update { target_version }`
* `DelayedByAge`
* `CurrentNoNewer`
* `CurrentBlockedByPolicy`

The final decision is made by controller/apply flow after user interaction.

### Candidate list for manual selection

Resolver must include evaluation metadata for **all newer candidates**, including ineligible ones.

This lets the UI/controller support the flow:

1. show resolver recommendation
2. let user inspect all newer candidates and reasons
3. allow manual selection of a specific ineligible candidate
4. execute the user-selected target exactly

The selected manual target should not be replaced by a recomputed “newest eligible” value.

### Gate bypass model

Bypass is one-run and gate-specific:

* `version_policy` bypass flag
* `min_release_age` bypass flag

These flags are independent so override can bypass one gate or both gates.

Resolver metadata semantics with bypass:

* top-level summary fields and counts (`latest_*_eligible`, blocked counters, recommendation) use **effective gates** after bypass is applied
* raw non-bypassed gate outcomes remain available per candidate in `evaluations` (`policy_allowed`, `age_allowed`, block reasons/warnings)
* the configured `version_policy` is a run-level input; exposing a duplicated per-candidate `effective_policy` field is optional

Fallback context note:

* when `same-track` degrades to conservative stable fallback, fallback context is surfaced by per-candidate warnings (`InstalledTrackUnknownFallbackStable`) rather than a dedicated run-level fallback field

---

# CLI and Validation

## Config validation

Accepted values for `version_policy`:

* `stable`
* `same-track`
* `any`

Any other value is a config error.

Example error:

```text
Invalid version_policy for [npm]: expected one of "stable", "same-track", "any", got "beta-only"
```

---

## Manager capability validation

If a manager does not support the configured policy safely:

Recommended behavior:

* show a manager-local error or warning
* crash the whole run, validate on startup

Example:

```text
- Error [manager] foo: version_policy "same-track" is not supported by this manager
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

## Example 3: `any`

Config:

```toml
[mise]
version_policy = "any"
```

Installed:

* `1.0.0`

Available:

* `1.1.0-alpha.1`

Result:

* update eligible, subject to age gate

---

## Example 4: no policy configured

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
- Disabled
- Stable
- SameTrack
- Any
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

Use a recommendation enum for resolver output:

```text
RecommendedOutcome:
- Update { target_version }
- DelayedByAge
- CurrentNoNewer
- CurrentBlockedByPolicy
```

And independent one-run gate bypass flags:

```text
GateBypass:
- version_policy: bool
- min_release_age: bool
```

Classifier API should be conservative and non-fallible at this boundary:

* parse/classify success -> return `ReleaseClass`
* parse/classify failure -> return `Unknown`

This avoids accidentally bypassing conservative fallback via error propagation.

---

## Policy predicate

Implement a function conceptually like:

```text
is_candidate_allowed(installed_class, candidate_class, policy) -> decision
```

Semantics:

* `Disabled` → true
* `Stable` → candidate_class == Final
* `Any` → true
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

---

# Recommended UX wording

Use “version policy” consistently in docs and output.

Avoid exposing low-level terms like “prerelease classifier” in user-facing text unless verbose/debug mode is enabled.

Preferred phrases:

* `blocked by version policy`
* `only prereleases available; policy=stable`
* `following prerelease track`
* `policy=same-track`

---

# Summary

This feature adds a per-manager optional `version_policy` gate with three values:

* `stable`
* `same-track`
* `any`

Selection uses **Option A**:

* gather newer versions
* filter by version policy
* then filter by age
* choose the newest remaining candidate

This keeps behavior predictable, works well with your existing delayed-upgrade model, preserves backward compatibility when unset, and leaves room for future package-level overrides and interactive one-run force updates.

Resolver output is modeled as a recommendation plus per-candidate evaluation metadata so interactive flows can support exact manual target selection and one-run gate bypass without redesign.
