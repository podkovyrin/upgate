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
- `src/managers/go.rs`
- `src/managers/gem.rs`
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
- `--max-parallel-checks <n>` (default `6`)
- `--managers <list>` where list is comma-separated manager IDs (default: all managers)
- `--set <manager.key=value>` / `-S <manager.key=value>` (repeatable config overrides)
- `--plain` (force plain output: no color, ASCII arrow)
- `--no-color` (disable ANSI color styling)
- `--verbose` (show additional metadata segments such as source and delayed-latest note)

Configuration file:
- `$XDG_CONFIG_HOME/upnow/config.toml` (fallback `~/.config/upnow/config.toml`)
- Per-manager `mode` values are configured in TOML (`off`, `plan`, `apply`).
- Per-manager `min_release_age` values are configured in TOML.
- Brew-only `no_update` is configured under `[brew].no_update` (default `false`).
- CLI `--set`/`-S` overrides have higher precedence than file config.

Default manager set: all registered managers (`brew`, `bun`, `cargo`, `npm`, `yarn`, `mise`, `pipx`, `pnpm`, `uv`, `go`, `gem`) with default modes:
- `brew`, `bun`, `cargo`, `npm`, `yarn`, `mise`, `pipx`, `pnpm`, `uv`, `go`: `apply`
- `gem`: `off`

Behavior notes:
- `plan` is non-mutating.
- `apply` performs updates.
- Selected managers run in a fixed internal order:
  `brew`, `bun`, `cargo`, `npm`, `yarn`, `mise`, `pipx`, `pnpm`, `uv`, `go`, `gem`.
- Manager execution is gated by per-manager `mode`:
  - `off`: skip always
  - `plan`: run only in `plan`
  - `apply`: run in both `plan` and `apply`
- `--managers` does not bypass `mode`; override with `--set <manager>.mode=...`.

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
- Manager execution remains sequential (registry order).
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

Rendered prefix style:
- `+ Update`
- `~ Delayed`
- `- Skipped`
- `! Error`

Base line shape:
- `<status-prefix> [<manager>] <name> v<from> -> v<to>`
- Unicode arrow `→` is used for non-plain terminal output.
- ASCII arrow `->` is used in plain output.

Metadata display policy:
- Default output hides metadata segments.
- `--verbose` shows metadata segments:
  - `(source: <source>)`
  - update delayed-latest annotation when present:
    - `(latest v<latest> delayed: <age> < <required>)`

Version labels:
- Numeric versions get `v` prefix in output.
- Non-numeric tokens (e.g. `*`) are printed as-is.
- In colored output, the target version `v<to>` is bold.
- Only changed target-version parts are highlighted in blue (while remaining bold).

Suppression:
- `skipped_no_change` (`already at selected target`) is not printed.

Spinner behavior:
- Manager spinner is shown on interactive terminal `stderr`.
- Spinner is suppressed in plain output and non-interactive `stderr`.
- Outcome lines are emitted while spinner rendering is suspended to avoid interleaving artifacts.

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
- Uses config `[brew].min_release_age` (default `12h`).

Planning flow:
1. Optional `brew update --quiet` (skipped when config `[brew].no_update = true`).
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
- Uses config `[cargo].min_release_age` (default `7d`).

Planning semantics:
- Target is highest eligible version (`age >= min_release_age`) with constraint `target >= current`.

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
- Uses config `[npm].min_release_age` (default `7d`).

Planning semantics:
- Target is highest eligible version (`age >= min_release_age`) with constraint `target >= current`.

Planning flow:
1. `npm outdated -g --json`.
2. Per package: `npm view <name> time --json`.
3. Parse version timestamps and choose semver-max eligible target.

Apply flow:
- Batch command: `npm -g update --min-release-age <days>`.
- `<days>` is derived from `[npm].min_release_age` and must be whole days (validated on apply path).
- Batch apply failure emits one synthetic `*` error outcome.

Concurrency:
- Planning per-package resolution is parallelized.
- Pool size: `clamp(1, --max-parallel-checks, 6)`.

### yarn (`src/managers/yarn.rs`)

Delay policy:
- Uses config `[yarn].min_release_age` (default `7d`).

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
- Uses config `[pnpm].min_release_age` (default `7d`).

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
- Uses config `[bun].min_release_age` (default `7d`).

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
- Batch command: `bun update -g --minimum-release-age <seconds>`.
- `<seconds>` is derived from `[bun].min_release_age`.
- Batch apply failure emits one synthetic `*` error outcome.

Concurrency:
- Planning per-package resolution is parallelized.
- Pool size: `clamp(1, --max-parallel-checks, 6)`.

### mise (`src/managers/mise.rs`)

Delay policy:
- Uses config `[mise].min_release_age` (default `7d`) via `--before <duration>`.

Planning flow:
1. `mise upgrade --dry-run --before <duration-from-config>`.
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
- Batch command: `mise upgrade --before <duration-from-config>`.
- Batch apply failure emits one synthetic `*` error outcome.

### pipx (`src/managers/pipx.rs`)

Delay policy:
- Uses config `[pipx].min_release_age` (default `7d`).

Planning semantics:
- Target is highest eligible version (`age >= min_release_age`) with constraint `target >= current`.
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
- Uses config `[uv].min_release_age` (default `7d`) via `--exclude-newer <duration>`.

Planning semantics:
- Matches `pipx`: choose highest eligible release (`age >= min_release_age`) with `target >= current`.

Planning flow:
1. `uv tool dir`.
2. Enumerate installed tools:
   - `uv tool list --show-version-specifiers`
   - fallback: inspect receipts in tool dir.
3. Per tool resolve target via dry-run resolver:
   - `uv pip install --dry-run -p <tool-python> --upgrade --exclude-newer <duration-from-config> <requirement>`
4. Parse `+ <tool>==<target>` lines from dry-run plan.
5. `uv tool list --outdated` for latest context.
6. Latest-age annotation: PyPI JSON (`https://pypi.org/pypi/<name>/json`).

Apply flow:
- Per eligible tool:
  - `uv tool install --upgrade --exclude-newer <duration-from-config> <name>`
- Per-tool apply failures emit `error` and continue.

Concurrency:
- Planning per-tool dry-run resolution is parallelized.
- Pool size: `clamp(1, --max-parallel-checks, 2)`.

### go (`src/managers/go.rs`)

Delay policy:
- Uses config `[go].min_release_age` (default `7d`).

Planning semantics:
- Scope is global Go tools discovered from effective Go bin directory (`GOBIN`, `GOPATH/bin`, fallback `~/go/bin`).
- Only binaries with usable `go version -m` module/version metadata are managed.
- Target is highest eligible release (`age >= min_release_age`) with constraint `target >= current`.

Planning flow:
1. Discover binaries in Go bin directory.
2. For each binary: `go version -m <binary>`.
3. Parse module and version metadata.
4. Resolve module versions via `go list -m -json -versions <module>`.
5. Resolve per-version release timestamps via `go list -m -json <module>@<version>`.
6. Select semver-max eligible target.

Apply flow:
- Per eligible tool: `go install <path>@<target>` where `<path>` is from `go version -m` `path` field.
- Per-tool apply failures emit `error` and continue.

Concurrency:
- Planning per-tool target resolution is parallelized.
- Pool size: `clamp(1, --max-parallel-checks, 4)`.

### gem (`src/managers/gem.rs`)

Execution mode:
- Default mode is `off`.

Delay policy:
- Uses config `[gem].min_release_age` (default `7d`).

Planning semantics:
- Scope is globally installed Ruby gems from `gem list` intersected with `gem outdated`.
- Default gems are skipped (managed by Ruby runtime packaging).
- Target is highest eligible RubyGems release (`age >= min_release_age`) with constraints:
  - `target >= current`
  - release is not prerelease
  - release `ruby_version` requirement matches current Ruby runtime.

Planning flow:
1. `gem list` (identify default gems).
2. `gem outdated` (candidate outdated gems).
3. Per gem: `https://rubygems.org/api/v1/versions/<gem>.json`.
4. Parse release versions/timestamps and Ruby version requirements.
5. Select semver-max eligible target.

Apply flow:
- Per eligible gem: `gem install <name> -v <target>`.
- Per-gem apply failures emit `error` and continue.

Concurrency:
- Planning per-gem target resolution is parallelized.
- Pool size: `clamp(1, --max-parallel-checks, 4)`.
