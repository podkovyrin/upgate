# spec

## Project

`upnow` is a multi-manager CLI that prints and applies delayed global package/tool upgrades.

This file is the manager-spec source of truth for current behavior.

## Source of truth in code

- `src/main.rs`
- `src/app/cli.rs`
- `src/app/mod.rs`
- `src/config/mod.rs`
- `src/config/load.rs`
- `src/config/model.rs`
- `src/config/overrides.rs`
- `src/config/path.rs`
- `src/config/pins.rs`
- `src/manager/mod.rs`
- `src/manager/context.rs`
- `src/manager/plugin.rs`
- `src/manager/registry.rs`
- `src/managers/mod.rs`
- `src/managers/common/mod.rs`
- `src/managers/common/apply.rs`
- `src/managers/common/plan/mod.rs`
- `src/managers/common/plan/collect.rs`
- `src/managers/common/plan/decision.rs`
- `src/managers/common/plan/emit.rs`
- `src/managers/common/plan/types.rs`
- `src/managers/common/versioning/mod.rs`
- `src/managers/common/versioning/pep440.rs`
- `src/managers/common/versioning/semver.rs`
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
- `src/managers/dotnet.rs`
- `src/outcome/mod.rs`
- `src/outcome/item.rs`
- `src/outcome/render.rs`
- `src/outcome/types.rs`
- `src/interactive/mod.rs`
- `src/interactive/apply.rs`
- `src/ui.rs`
- `src/util/http.rs`
- `src/util/process.rs`
- `src/util/parallel.rs`
- `src/util/logging.rs`
- `src/util/time/mod.rs`
- `src/util/time/clock.rs`
- `src/util/time/duration.rs`
- `src/util/time/format.rs`
- `src/util/time/parse.rs`

## CLI contract

Commands:
- `plan` (default when omitted)
- `apply`
- `scan`

Global options:
- `--max-parallel-checks <n>` (default `6`)
- `--managers <list>` where list is comma-separated manager IDs (default: all managers)
- `--set <section.key=value>` / `-S <section.key=value>` (repeatable config overrides)
- `--plain` (force plain output: no color, ASCII arrow)
- `--no-color` (disable ANSI color styling)
- `--verbose` (show additional metadata segments such as source and delayed-latest note)
- `--debug-commands` (persist full per-command debug logs: status, timing, stdout, stderr)
- `--show-commands` / `--print-commands` (print each command before execution)
- `--interactive` (apply-mode only; prompt per manager to select updates)

Configuration file:
- `$XDG_CONFIG_HOME/upnow/config.toml` (fallback `~/.config/upnow/config.toml`)
- Global `[upnow].scan_old_age_threshold` controls scan old-age highlighting (default `365d`).
- Per-manager `mode` values are configured in TOML (`off`, `plan`, `apply`).
- Per-manager `min_release_age` values are configured in TOML.
- Brew-only `no_update` is configured under `[brew].no_update` (default `false`).
- Per-manager `pinned` list can be stored under `[<manager>].pinned`.
- CLI `--set`/`-S` overrides have higher precedence than file config.

Default manager set: all registered managers (`brew`, `bun`, `cargo`, `npm`, `yarn`, `mise`, `pipx`, `pnpm`, `uv`, `go`, `gem`, `dotnet`) with default modes:
- `brew`, `bun`, `cargo`, `npm`, `yarn`, `mise`, `pipx`, `pnpm`, `uv`, `go`: `apply`
- `gem`, `dotnet`: `off`

Behavior notes:
- `plan` does not apply package/tool upgrades. (Note: manager-specific metadata refresh commands can still run; e.g. Homebrew may run `brew update --quiet` unless disabled.)
- `scan` is non-mutating and lists installed package/tool versions across managers.
- `apply` performs updates.
- Manager execution is Unix-only (`cfg(unix)` target family); on non-Unix platforms each selected manager emits a manager-level `skipped` outcome with reason `unsupported platform: requires unix`.
- Before each manager run, command availability is preflight-checked (`probe_command`); missing manager commands emit manager-level `skipped` outcomes and do not fail the process.
- `--interactive` is valid only with `apply` and requires interactive `stdin` + `stdout` TTY; otherwise execution fails.
- Interactive flow:
  - all managers prompt with all upgradable items selected by default and allow deselection
- Deselected/skipped choices are persisted as manager-local pins in config.
- Selected managers run in a fixed internal order:
  `brew`, `bun`, `cargo`, `npm`, `yarn`, `mise`, `pipx`, `pnpm`, `uv`, `go`, `gem`, `dotnet`.
- Manager execution is gated by per-manager `mode`:
  - `off`: skip always
  - `plan`: run in `plan` and `scan`
  - `apply`: run in `plan`, `scan`, and `apply`
- `--managers` does not bypass `mode`; override with `--set <manager>.mode=...`.

## Architecture (current)

- Manager orchestration is split by concern:
  - `manager/context`: run mode + per-manager runtime context/state
  - `manager/plugin`: plugin trait contract
  - `manager/registry`: plugin registry order + selection + context build
- Manager-native parsing/selection/apply logic remains in manager modules.
- Shared manager helpers are grouped under `managers/common`:
  - `common/plan`: planning decisions, outcome emission, shared plan models
  - `common/apply`: per-item and global apply flow orchestration
  - `common/versioning`: semver/PEP440 timeline parsing and age-window selection
- Shared interactive selection helper applies pinned filtering and per-manager selection.
- Interactive pin persistence updates only the manager `pinned` key while preserving existing config structure/comments.
- Shared utility modules are used for narrow cross-cutting concerns:
  - subprocess execution/formatting (`util/process`)
  - command/session logging (`util/logging`)
  - indexed internal parallel job execution helpers (`util/parallel`)
  - time helpers (`util/time`):
    - clock (`util/time/clock`)
    - duration parsing (`util/time/duration`)
    - human-age formatting (`util/time/format`)
    - RFC3339 parse to unix seconds (`util/time/parse`)
- Outcome contract is split by concern:
  - `outcome/types`: status/reason enums
  - `outcome/item`: `ItemOutcome` constructors/payload
  - `outcome/render`: text rendering policy and line emission

Internal planning parallelism (current):
- Manager execution remains sequential (registry order).
- Apply remains sequential within each manager (no parallel apply orchestration).
- Several managers parallelize per-item planning checks internally, then emit outcomes in original deterministic order.
- Effective per-manager planning concurrency is bounded by both CLI value and manager cap.

## Output contract

Outcomes are printed as text lines.

Statuses:
- `current`
- `update`
- `delayed`
- `skipped`
- `error`

Rendered prefix style:
- `= Current`
- `+ Update`
- `~ Delayed`
- `- Skipped`
- `! Error`

Base line shape:
- `current`: `<status-prefix> [<manager>] <name> v<version>`
- other statuses: `<status-prefix> [<manager>] <name> v<from> -> v<to>`
- Unicode arrow `→` is used for non-plain terminal output.
- ASCII arrow `->` is used in plain output.

Metadata display policy:
- Default output hides metadata segments.
- `--verbose` shows metadata segments:
  - `(source: <source>)`
  - update delayed-latest annotation when present:
    - `(latest v<latest> delayed: <age> < <required>)`
  - scan age annotation when available:
    - `(released: <age>)`
    - `<age>` is highlighted when `age >= [upnow].scan_old_age_threshold`.

Version labels:
- Numeric versions get `v` prefix in output.
- Non-numeric tokens (e.g. `*`) are printed as-is.
- In colored output, the target version `v<to>` is bold.
- Only changed target-version parts are highlighted in blue (while remaining bold).

Suppression:
- `skipped_no_change` (`already at selected target`) is not printed.
- `skipped` with reason `missing_metadata` is hidden in default output and shown with `--verbose`.

Spinner behavior:
- Manager spinner is shown on interactive terminal `stderr`.
- Spinner is suppressed in plain output and non-interactive `stderr`.
- Outcome lines are emitted while spinner rendering is suspended to avoid interleaving artifacts.

Batch-error sentinel:
- Managers with single batch apply commands can emit synthetic error outcome lines using `name="*"` and versions `* -> *`.

Command visibility:
- `--show-commands` (alias `--print-commands`) prints each command to stderr before execution.
- Command display is independent from spinner rendering (spinner is suspended while printing).

## Runtime logging

Log root:
- macOS: `~/Library/Logs/upnow`
- other platforms: `$XDG_STATE_HOME/upnow/logs` (fallback `~/.local/state/upnow/logs`)

Session layout:
- One session directory per run: `<epoch-seconds>-<pid>`
- One log file per manager bucket: `<manager>.log` (for example `brew.log`, `npm.log`)

Policy:
- Mutating commands are always logged (start + finish, with captured stdout/stderr).
- `--debug-commands` logs all commands (including non-mutating checks) with stdout/stderr and timing.
- Command spawn failures are logged.

## Failure/exit behavior (current)

- Handled per-item failures emit `error` outcomes and continue within that manager loop.
- Fatal manager-level failures still return an error from `run(...)`.
- Manager-level setup/run errors do not stop subsequent managers; remaining selected managers are still attempted.
- Process exit today:
  - `0` when all selected managers complete without manager-level setup/run errors (including handled per-item errors)
  - `1` when one or more managers fail setup/run
  - `130` when a manager failure comes from a signal-terminated subprocess
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
- Selective command: `npm -g update <name> --min-release-age <days>` (per selected package).
- Global command: `npm -g update --min-release-age <days>`.
- `<days>` is derived from `[npm].min_release_age` and must be whole days (validated during npm manager setup).
- Global apply failure emits one synthetic `*` error outcome.
- Global command is used only when every upgradable package is selected and `[npm].pinned` is empty; otherwise selective per-package apply is used.

Concurrency:
- Planning per-package resolution is parallelized.
- Pool size: `clamp(1, --max-parallel-checks, 6)`.

### yarn (`src/managers/yarn.rs`)

Delay policy:
- Uses config `[yarn].min_release_age` (default `7d`).

Version gate:
- Yarn major version is detected first.
- Yarn 2+ global upgrades are not supported by this manager; it emits a skipped manager-level outcome and exits early for both `plan/apply` and `scan`.

Planning flow (Yarn 1):
1. `yarn global list --depth=0`.
2. Per package: `yarn info <name> time --json`.
3. Parse version timestamps and choose semver-max eligible target (`>= current`, age eligible).

Apply flow (Yarn 1):
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
- Expects object-shaped JSON for `pnpm outdated`.
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
1. `bun pm ls -g --json` (installed global inventory).
2. Per package: `bun pm view <name> time --json --cwd <global>`.
3. Parse timestamps and choose semver-max eligible target (`>= current`, age eligible).

Notes:
- Bun executable resolution order:
  - `UPNOW_BUN_BIN`
  - `mise which bun`
  - fallback `bun`
- Missing global manifest/lockfile states are treated as empty/no-op.

Apply flow:
- Selective command: `bun update -g <name>@<target> --minimum-release-age <seconds>` (per selected package).
- Global command: `bun update -g --minimum-release-age <seconds>`.
- `<seconds>` is derived from `[bun].min_release_age`.
- Global apply failure emits one synthetic `*` error outcome.
- Global command is used only when every upgradable package is selected and `[bun].pinned` is empty; otherwise selective per-package apply is used.

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
- Non-`npm:` tools are currently annotated as `0s` (placeholder).
- For `npm:` tools, if age enrichment is unavailable or per-item age lookup is missing, delayed-latest age currently falls back to `0s`.

Error handling:
- `mise outdated --json` failure emits one synthetic `*` error outcome and planning continues without latest-map annotations.
- Per-item latest-age lookup failures emit per-item `error` outcomes.

Concurrency:
- Planning pair extraction stays sequential.
- `npm:` latest-age lookups for delayed-latest annotations are parallelized.
- Pool size for that lookup path: `clamp(1, --max-parallel-checks, 4)`.

Apply flow:
- Selective command: `mise upgrade --before <duration-from-config> <tool>` (per selected tool).
- Global command: `mise upgrade --before <duration-from-config>`.
- Global apply failure emits one synthetic `*` error outcome.
- Global command is used only when every upgradable tool is selected and `[mise].pinned` is empty; otherwise selective per-tool apply is used.

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
- Per eligible package: `pipx upgrade <name>==<target>`.
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

### dotnet (`src/managers/dotnet.rs`)

Execution mode:
- Default mode is `off`.

Delay policy:
- Uses config `[dotnet].min_release_age` (default `7d`).

Planning semantics:
- Scope is global .NET tools from `dotnet tool list --global --format json`.
- Target is highest eligible NuGet release (`age >= min_release_age`) with constraint `target >= current`.

Planning flow:
1. `dotnet tool list --global --format json`.
2. Per tool: query NuGet registration index/pages (`registration5-gz-semver2`, fallback `registration5-semver1`).
3. Parse catalog versions and `published` timestamps.
4. Select semver-max eligible target.

Apply flow:
- Per eligible tool: `dotnet tool update --global <name> --version <target> --allow-downgrade`.
- Per-tool apply failures emit `error` and continue.

Concurrency:
- Planning per-tool target resolution is parallelized.
- Pool size: `clamp(1, --max-parallel-checks, 4)`.
