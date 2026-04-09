# Internal Manager Parallelism Roadmap (Draft)

## 1) Decision recap

Current direction:

- **No manager-level parallel apply** (keep apply sequential across managers).
- **No manager-level parallel plan (for now)**.
- Focus on **internal per-manager parallelism in plan logic** first.

Rationale:

- Lower implementation risk than cross-manager scheduling.
- Preserves reliability and deterministic behavior at the manager boundary.
- Captures most practical speedups (per-package/per-tool metadata resolution is the slow part).

---

## 2) Scope and non-goals

### In scope

- Parallelizing per-item *planning* checks inside managers.
- Keeping manager bootstrap/discovery and apply behavior intact.
- Keeping output contract unchanged.

### Out of scope

- Parallel apply (explicitly deferred).
- Cross-manager parallel scheduler.
- Persistent logging system (planned separately).
- Any change to delay policy semantics.

---

## 3) Cross-cutting design rules

These rules apply to every stage.

### 3.1 Deterministic output

Even with internal concurrency, output order should remain stable:

1. Build jobs in deterministic order (`Vec<Job>`).
2. Process jobs in parallel.
3. Store results by original index.
4. Emit outcomes sequentially in original order.

This avoids interleaved/noisy output and keeps diffs/tests stable.

### 3.2 Bounded concurrency

Use:

- `--max-parallel-checks` as global upper bound.
- Per-manager caps for safety (network/process contention):
  - Node managers (`npm`, `yarn`, `pnpm`, `bun`): cap `6`
  - `cargo`: cap `4`
  - `pipx`: cap `4`
  - `uv`: cap `2` (most conservative)
  - `mise` npm latest-age lookups: cap `4`

Effective parallelism:

```text
effective = clamp(cli.max_parallel_checks, 1, manager_cap)
```

### 3.3 Error policy

- Keep current fail-late behavior at per-item level.
- Resolver failures should emit per-item `error` outcomes and continue.
- Manager-level fatal errors remain fatal for that manager run (unchanged semantics).

### 3.4 Keep abstractions lightweight

Follow `docs/implementation-principles.md`:

- Prefer small helper functions over a large shared scheduling framework.
- Manager-native logic remains in manager files.

### 3.5 Apply path unchanged

- Do not parallelize apply.
- Do not alter manager apply command selection.

---

## 4) Shared implementation pattern

Use the existing brew pattern as template:

1. Build `jobs: Vec<Job>`.
2. Run `into_par_iter().enumerate().map(...)` in a bounded Rayon pool.
3. Collect `(index, Resolved)` results.
4. Fill `result_slots: Vec<Option<Resolved>>`.
5. Convert to `Vec<Resolved>` with missing-slot internal error checks.
6. Emit outcomes in sequence.

Suggested small helper (optional, not mandatory):

- `run_indexed_parallel(jobs, threads, resolver) -> Vec<Resolved>`

Only add this if duplication becomes noisy; otherwise keep per-manager explicit code.

---

## 5) Staged implementation plan

## Stage 0 — Baseline + guardrails

### Goals

- Lock in baseline performance and behavior before refactors.
- Add explicit review checklist for determinism and error handling.

### Work

- Record baseline timings with `scripts/profile.sh`:
  - `--dry-run --no-update`
  - and one realistic run without `--no-update`.
- Add/confirm checklist:
  - output order unchanged,
  - statuses/reasons unchanged,
  - no apply behavior changes.

### Exit criteria

- Baseline perf report saved.
- Stage checklist documented in PR description template.

---

## Stage 1 — Node managers planning parallelization (`npm`, `yarn`, `pnpm`, `bun`)

### Goals

- Parallelize per-package time lookups + target resolution.
- Keep manager output deterministic.

### Work

For each manager:

1. Keep initial discovery command sequential (e.g. `outdated`, `global list`).
2. Build indexed jobs from discovered packages.
3. Resolve target in parallel using bounded pool.
4. Emit outcomes in original order.

Manager notes:

- `npm`: parallel `npm view <name> time --json`.
- `yarn`: parallel `yarn info <name> time --json`.
- `pnpm`: parallel `pnpm view <name> time --json`.
- `bun`: parallel `bun pm view <name> time --json --cwd <global>`.

### Risk controls

- Keep current parsing and outcome constructors unchanged.
- Keep batch apply behavior untouched.
- Cap effective parallelism to 6.

### Exit criteria

- Functional parity with sequential behavior.
- Noticeable plan latency reduction for 20+ packages.

---

## Stage 2 — `cargo` and `pipx` planning parallelization + HTTP client hygiene

### Goals

- Parallelize per-item remote lookups.
- Reduce network overhead by reusing HTTP clients per run.

### Work

#### `cargo`

- Parallelize per-crate target resolution.
- Inside each job keep existing semantics (search + crates timeline logic).
- Consider reusing one `reqwest::blocking::Client` for crates.io calls in this manager run.

#### `pipx`

- Parallelize per-package PyPI JSON fetch + target resolution.
- Use one shared `reqwest::blocking::Client` per manager run instead of repeated `reqwest::blocking::get`.

### Risk controls

- Cap at 4.
- Preserve PEP440/SemVer decision behavior.

### Exit criteria

- Same outcome semantics on representative package sets.
- Reduced wall-clock time vs Stage 0 baseline.

---

## Stage 3 — `uv` planning parallelization (conservative)

### Goals

- Speed up per-tool dry-run target resolution safely.

### Work

- Keep discovery sequential (`uv tool dir`, installed tools, outdated latest map).
- Parallelize per-tool resolver:
  - `uv pip install --dry-run ... --exclude-newer 7d <requirement>`.
- Keep output emission sequential.

Optional within stage (if safe):

- Parallelize PyPI latest-age lookups for delayed-latest annotations.

### Risk controls

- Cap at 2 initially.
- Watch for uv cache/process contention and noisy failures.

### Exit criteria

- Stable behavior under repeated runs.
- Meaningful speedup on multi-tool setups.

---

## Stage 4 — `mise` targeted parallelization

### Goals

- Improve planning latency for delayed-latest annotation path.

### Work

- Keep `mise upgrade --dry-run --before 7d` and `mise outdated --json` sequential.
- For entries where `latest != planned_target` and `tool` is `npm:*`, parallelize npm latest-age lookups.
- Keep non-`npm:` behavior unchanged (`0s` placeholder age).

### Risk controls

- Cap at 4.
- Preserve current error behavior:
  - per-item lookup errors => per-item `error` outcome.

### Exit criteria

- No semantic drift in emitted outcomes.
- Lower latency on larger mise/npm tool sets.

---

## Stage 5 — Tuning, profiling, and defaults review

### Goals

- Validate final caps/defaults with real data.

### Work

- Re-run profiling matrix (`scripts/profile.sh --compare-parallel ...`).
- Compare before/after across:
  - small setup (few packages),
  - medium setup,
  - large setup.
- Validate no substantial increase in transient error rates.

### Exit criteria

- Final cap table confirmed or adjusted.
- Documented performance summary and recommended settings.

---

## 6) Testing strategy by stage

For each stage:

1. **Unit tests**: parser and selection logic unchanged (keep existing tests, add only where needed).
2. **Behavioral checks**:
   - deterministic output ordering,
   - unchanged status/reason semantics.
3. **Smoke tests**:
   - `plan` with each manager selected independently,
   - mixed-manager sequential run.
4. **Failure-path checks**:
   - resolver command failures produce per-item `error` outcomes,
   - manager continues processing remaining items.

---

## 7) Rollout and rollback

### Rollout

- Implement one stage per PR.
- Avoid mixing multiple managers in one PR except Stage 1 node-family grouping.

### Rollback

If instability is observed in a stage:

- Reduce manager cap to `1` for the affected manager(s) as immediate mitigation.
- Revert only that stage’s manager changes, keep other completed stages.

---

## 8) Open questions (to finalize before implementation)

1. Should manager-specific caps be hardcoded initially or CLI-configurable later?
2. Should `--max-parallel-checks` default remain `6` after full rollout?
3. Do we want a tiny shared helper for indexed-parallel pattern, or explicit per-manager code only?

---

## 9) Acceptance summary

This roadmap is complete when:

- apply remains sequential and unchanged,
- internal plan parallelism is added manager-by-manager,
- output and semantics remain deterministic,
- performance improvements are measurable and documented.
