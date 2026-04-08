# spec

## Project

`brew-delay-upgrade` is a multi-manager CLI that prints and applies delayed global package/tool upgrades.

This file is the only manager-spec source of truth.

Source of truth in code:
- `src/main.rs`
- `src/brew.rs`
- `src/npm.rs`
- `src/mise.rs`
- `src/pipx.rs`

## CLI contract

Binary options (single shared interface):
- `--dry-run` / `-n`
- `--min-release-age <duration>` (default `12h`)
- `--max-parallel-checks <n>` (default `6`)
- `--no-update`
- `--managers <list>` where list is comma-separated values from:
  - `brew`, `npm`, `mise`, `pipx`

Default manager set: `brew`.

`src/main.rs` only parses CLI and dispatches to selected manager modules.

## Architecture constraints (implemented)

- Manager modules are isolated; no shared manager logic.
- Shared element is the `Cli` interface only.
- Each module owns its own data structures, parsing, and execution flow.

## Output contract

- Normal successful runs print plan lines only.
- Prefixes: `brew:`, `npm:`, `mise:`, `pipx:`.
- Versions are normalized with `v` prefix when numeric.
- Errors are returned as command failure (stderr from process wrappers).

## Manager behavior

### brew (`src/brew.rs`)

Delay policy:
- Uses CLI `--min-release-age` (default `12h`).

Planning flow:
1. Optional `brew update --quiet` (skipped when `--no-update`).
2. `brew outdated --json=v2`.
3. `brew info --json=v2 <formulae+casks>`.
4. `brew tap-info --json --installed`.
5. Local git commit age check (`origin/<branch>`, `origin/HEAD`, `FETCH_HEAD`, `HEAD`).
6. GitHub commits API fallback if local git fails.
7. Classify each item: upgrade / delayed / skipped.

Apply flow:
- Formulae: `brew upgrade --formula ...`
- Casks: `brew upgrade --cask ...`

Concurrency:
- Local checks pool size: `--max-parallel-checks`.
- API fallback pool size: `clamp(1, 4)`.

### npm (`src/npm.rs`)

Delay policy:
- Fixed `7d`.

Planning semantics:
- Target is **highest eligible version** (age >= 7d) with constraint `target >= current`.
- This is not always npm latest.

Planning flow:
1. `npm outdated -g --json`.
2. Per package: `npm view <name> time --json`.
3. Parse all version timestamps, choose target by semver ordering.

Output forms:
- `npm: <name> v<from> -> v<to> (source: npm)`
- If newer latest exists but too new:
  - `npm: <name> v<from> -> v<to> (source: npm; latest v<latest> delayed: <age> < 7d)`
- If no eligible `>= current`:
  - `npm: <name> v<current> -> v<current> (delayed, no eligible release >= current within 7d window, source: npm)`
- If resolved target equals current, no line is emitted.

Apply flow:
- `npm -g update --min-release-age 7`

### mise (`src/mise.rs`)

Delay policy:
- Fixed `7d` via `--before 7d`.

Planning flow:
1. `mise upgrade --dry-run --before 7d`.
2. Parse pairs:
   - `Would uninstall <tool>@<from>`
   - `Would install <tool>@<to>`
3. `mise outdated --json` for latest-version context.
4. If `latest != planned_target`, annotate latest delayed.

Latest-age annotation source:
- For tools with `npm:` prefix, age is computed via `npm view <pkg>@<latest> time --json`.
- For non-`npm:` tools, age is currently represented as `0s` in delayed annotation path.

Apply flow:
- `mise upgrade --before 7d`

### pipx (`src/pipx.rs`)

Delay policy:
- Fixed `7d`.

Planning semantics:
- Target is **highest eligible version** (age >= 7d) with constraint `target >= current`.
- Uses PEP 440 ordering.

Planning flow:
1. `pipx list --json`.
2. Per package: fetch `https://pypi.org/pypi/<name>/json`.
3. Parse releases and timestamps, choose eligible target.

Output forms:
- `pipx: <name> v<from> -> v<to> (source: pypi)`
- If newer latest exists but too new:
  - `pipx: <name> v<from> -> v<to> (source: pypi; latest v<latest> delayed: <age> < 7d)`
- If no eligible `>= current`:
  - `pipx: <name> v<current> -> v<current> (delayed, no eligible release >= current within 7d window, source: pypi)`
- If resolved target equals current, no line is emitted.

Apply flow:
- Per eligible package: `pipx upgrade <name>`
