# spec

## Project

`upnow` is a multi-manager CLI that prints and applies delayed global package/tool upgrades.

This file is the manager-spec source of truth for current behavior.

## Source of truth in code

- `src/main.rs`
- `src/manager.rs`
- `src/managers/mod.rs`
- `src/managers/brew.rs`
- `src/managers/bun.rs`
- `src/managers/cargo.rs`
- `src/managers/npm.rs`
- `src/managers/yarn.rs`
- `src/managers/mise.rs`
- `src/managers/pipx.rs`
- `src/managers/pnpm.rs`
- `src/managers/uv.rs`
- `src/outcome.rs`
- `src/util/process.rs`
- `src/util/timefmt.rs`
- `src/util/timeparse.rs`
- `src/util/durationparse.rs`
- `src/util/parallel.rs`

## CLI contract

Commands:
- `plan` (default when omitted)
- `apply`

Global options:
- `--dry-run` / `-n`
- `--min-release-age <duration>` (default `12h`)
- `--max-parallel-checks <n>` (default `6`)
- `--no-update`
- `--managers <list>` where list is comma-separated values from:
  - `brew`, `npm`, `yarn`, `mise`, `pipx`, `pnpm`, `bun`, `cargo`, `uv`

Default manager set: all managers (`brew`, `bun`, `cargo`, `npm`, `yarn`, `mise`, `pipx`, `pnpm`, `uv`).

Behavior notes:
- `plan` forces effective dry-run mode.
- Selected managers run in a fixed internal order:
  `brew`, `bun`, `cargo`, `npm`, `yarn`, `mise`, `pipx`, `pnpm`, `uv`.

## Architecture (current)

- Manager-native parsing/selection/apply logic remains in manager modules.
- Shared utility modules are used for narrow cross-cutting concerns:
  - subprocess execution/formatting (`util/process`)
  - human age formatting (`util/timefmt`)
  - RFC3339 parse to unix seconds (`util/timeparse`)
  - duration parsing (`util/durationparse`)
  - indexed internal parallel job execution helpers (`util/parallel`)
- Outcome formatting contract is centralized in `src/outcome.rs`.

Internal planning parallelism (current):
- Manager execution remains sequential (fixed manager order).
- Apply remains sequential within each manager (no parallel apply orchestration).
- Several managers parallelize per-item planning checks internally, then emit outcomes in original deterministic order.
- Effective per-manager planning concurrency is bounded by both CLI value and manager cap.

## Output contract

Outcomes are printed as text lines.

Statuses:
- `update`
- `delayed`
- `skipped`
- `error`

General forms:
- Update:
  - `<manager>: <name> v<from> -> v<to> (source: <source>)`
  - delayed-latest annotation variant:
    - `<manager>: <name> v<from> -> v<to> (source: <source>; latest v<latest> delayed: <age> < <required>)`
- Delayed (no eligible release):
  - `<manager>: <name> v<current> -> v<current> (delayed, no eligible release >= current within <required> window, source: <source>)`
- Skipped:
  - `<manager>: <name> v<from> -> v<to> (skipped, <reason>, source: <source>)`
- Error:
  - `<manager>: <name> v<from> -> v<to> (error, <reason>, source: <source>)`

Version labels:
- Numeric versions get `v` prefix in output.
- Non-numeric tokens (e.g. `*`) are printed as-is.

Suppression:
- `skipped_no_change` (`already at selected target`) is not printed.

Batch-error sentinel:
- Managers with single batch apply commands can emit synthetic error outcome lines using `name="*"` and versions `* -> *`.

## Failure/exit behavior (current)

- Handled per-item failures emit `error` outcomes and continue within that manager loop.
- Fatal manager-level failures still return an error from `run(...)`.
- A fatal manager-level failure aborts subsequent manager execution in the current run.
- Process exit today:
  - `0` when run completes without fatal manager-level error (including handled per-item errors)
  - `1` on fatal manager/runtime error
  - invalid CLI usage uses clap defaults (typically `2`)

## Manager behavior

### brew (`src/managers/brew.rs`)

Delay policy:
- Uses CLI `--min-release-age` (default `12h`).

Planning flow:
1. Optional `brew update --quiet` (skipped when `--no-update`).
2. `brew outdated --json=v2`.
3. `brew info --json=v2 <formulae+casks>`.
4. `brew tap-info --json --installed`.
5. Local git commit age check (`origin/<branch>`, `origin/HEAD`, `FETCH_HEAD`, `HEAD`).
6. GitHub commits API fallback if local git fails.
7. Classify each item.

Classification:
- upgrade / delayed / skipped
- age-check command failures are emitted as structured `error` outcomes.

Apply flow:
- Formulae: `brew upgrade --formula ...`
- Casks: `brew upgrade --cask ...`

Concurrency:
- Local checks pool size: `max(1, --max-parallel-checks)`.
- API fallback pool size: `clamp(1, 4)`.

### cargo (`src/managers/cargo.rs`)

Delay policy:
- Fixed `7d`.

Planning semantics:
- Target is highest eligible version (`age >= 7d`) with constraint `target >= current`.

Planning flow:
1. `cargo install --list`.
2. Per crate: `cargo search <name> --limit 1` (latest context).
3. Per crate: `https://crates.io/api/v1/crates/<name>` timeline.
4. Parse non-yanked versions and choose semver-max eligible target.

Apply flow:
- Per eligible crate: `cargo install --force <name>@<target>`.
- Per-crate apply failures emit `error` and continue.

Concurrency:
- Planning per-crate resolution is parallelized.
- Pool size: `clamp(1, --max-parallel-checks, 4)`.

### npm (`src/managers/npm.rs`)

Delay policy:
- Fixed `7d`.

Planning semantics:
- Target is highest eligible version (`age >= 7d`) with constraint `target >= current`.

Planning flow:
1. `npm outdated -g --json`.
2. Per package: `npm view <name> time --json`.
3. Parse version timestamps and choose semver-max eligible target.

Apply flow:
- Batch command: `npm -g update --min-release-age 7`.
- Batch apply failure emits one synthetic `*` error outcome.

Concurrency:
- Planning per-package resolution is parallelized.
- Pool size: `clamp(1, --max-parallel-checks, 6)`.

### yarn (`src/managers/yarn.rs`)

Delay policy:
- Fixed `7d`.

Planning flow:
1. `yarn global list --depth=0`.
2. Per package: `yarn info <name> time --json`.
3. Parse version timestamps and choose semver-max eligible target (`>= current`, age eligible).

Apply flow:
- Per eligible package: `yarn global add <name>@<target>`.
- Per-package apply failures emit `error` and continue.

Concurrency:
- Planning per-package resolution is parallelized.
- Pool size: `clamp(1, --max-parallel-checks, 6)`.

### pnpm (`src/managers/pnpm.rs`)

Delay policy:
- Fixed `7d`.

Planning flow:
1. `pnpm outdated -g --json`.
2. Per package: `pnpm view <name> time --json`.
3. Parse timestamps and choose semver-max eligible target (`>= current`, age eligible).

Notes:
- Handles both object and array JSON shapes for `pnpm outdated`.
- Treats `ERR_PNPM_NO_IMPORTER_MANIFEST_FOUND` as no-op/empty.

Apply flow:
- Per eligible package: `pnpm add -g <name>@<target>`.
- Per-package apply failures emit `error` and continue.

Concurrency:
- Planning per-package resolution is parallelized.
- Pool size: `clamp(1, --max-parallel-checks, 6)`.

### bun (`src/managers/bun.rs`)

Delay policy:
- Fixed `7d` (`604800` seconds).

Planning flow:
1. `bun outdated -g`.
2. Per package: `bun pm view <name> time --json --cwd <global>`.
3. Parse timestamps and choose semver-max eligible target (`>= current`, age eligible).

Notes:
- Bun executable resolution order:
  - `UPNOW_BUN_BIN`
  - `mise which bun`
  - fallback `bun`
- Missing global manifest/lockfile states are treated as empty/no-op.

Apply flow:
- Batch command: `bun update -g --minimum-release-age 604800`.
- Batch apply failure emits one synthetic `*` error outcome.

Concurrency:
- Planning per-package resolution is parallelized.
- Pool size: `clamp(1, --max-parallel-checks, 6)`.

### mise (`src/managers/mise.rs`)

Delay policy:
- Fixed `7d` via `--before 7d`.

Planning flow:
1. `mise upgrade --dry-run --before 7d`.
2. Parse uninstall/install pairs:
   - `Would uninstall <tool>@<from>`
   - `Would install <tool>@<to>`
3. `mise outdated --json` for latest context.
4. If `latest != planned_target`, annotate delayed latest age.

Latest-age annotation source:
- `npm:` tools use `npm view <pkg>@<latest> time --json`.
- Non-`npm:` tools currently use `0s` as placeholder age.

Error handling:
- `mise outdated --json` failure emits one synthetic `*` error outcome and planning continues without latest-map annotations.
- Per-item latest-age lookup failures emit per-item `error` outcomes.

Concurrency:
- Planning pair extraction stays sequential.
- `npm:` latest-age lookups for delayed-latest annotations are parallelized.
- Pool size for that lookup path: `clamp(1, --max-parallel-checks, 4)`.

Apply flow:
- Batch command: `mise upgrade --before 7d`.
- Batch apply failure emits one synthetic `*` error outcome.

### pipx (`src/managers/pipx.rs`)

Delay policy:
- Fixed `7d`.

Planning semantics:
- Target is highest eligible version (`age >= 7d`) with constraint `target >= current`.
- Uses PEP 440 ordering.

Planning flow:
1. `pipx list --json`.
2. Per package: `https://pypi.org/pypi/<name>/json`.
3. Parse releases/timestamps and choose eligible target.

Apply flow:
- Per eligible package: `pipx upgrade <name>`.
- Per-package apply failures emit `error` and continue.

Concurrency:
- Planning per-package resolution is parallelized.
- Pool size: `clamp(1, --max-parallel-checks, 4)`.

### uv (`src/managers/uv.rs`)

Delay policy:
- Fixed `7d` via `--exclude-newer 7d`.

Planning semantics:
- Matches `pipx`: choose highest eligible release (`age >= 7d`) with `target >= current`.

Planning flow:
1. `uv tool dir`.
2. Enumerate installed tools:
   - `uv tool list --show-version-specifiers`
   - fallback: inspect receipts in tool dir.
3. Per tool resolve target via dry-run resolver:
   - `uv pip install --dry-run -p <tool-python> --upgrade --exclude-newer 7d <requirement>`
4. Parse `+ <tool>==<target>` lines from dry-run plan.
5. `uv tool list --outdated` for latest context.
6. Latest-age annotation: PyPI JSON (`https://pypi.org/pypi/<name>/json`).

Apply flow:
- Per eligible tool:
  - `uv tool install --upgrade --exclude-newer 7d <name>`
- Per-tool apply failures emit `error` and continue.

Concurrency:
- Planning per-tool dry-run resolution is parallelized.
- Pool size: `clamp(1, --max-parallel-checks, 2)`.
