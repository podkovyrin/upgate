# upnow Specification (High Level)

## Purpose

`upnow` helps users keep globally installed developer tools up to date across multiple package managers, while avoiding very new releases until they have "aged" past a configured threshold.

It supports three workflows:

- **plan**: show what would be updated
- **apply**: perform updates
- **scan**: show currently installed versions

---

## Supported managers

Built-in managers:

- `brew`, `bun`, `cargo`, `npm`, `yarn`, `mise`, `pipx`, `pnpm`, `uv`, `go`, `gem`, `dotnet`

Default execution modes:

- **apply**: `brew`, `bun`, `cargo`, `npm`, `yarn`, `mise`, `pipx`, `pnpm`, `uv`, `go`
- **off**: `gem`, `dotnet`

Users can change modes in config or with CLI overrides.

---

## Core behavior

- `plan` is non-applying (preview of upgrades).
- `apply` executes upgrades for selected managers/items.
- `scan` is non-mutating and focuses on installed versions.
- Managers can be selected with `--managers`, but per-manager `mode` still controls whether they run.
- Missing tools or unsupported environments are reported as skipped instead of crashing the whole run.

---

## Delayed-upgrade model

Each manager has a `min_release_age` setting.

`upnow` prefers upgrade targets that are old enough to satisfy that threshold. This helps reduce risk from freshly published releases.

At a high level:

- if an eligible newer version exists → **update**
- if only too-new versions exist → **delayed**
- if already up to date → **current**

---

## Interactive apply

`upnow apply --interactive` allows users to choose which upgrades to apply.

- All upgradable items are selected by default.
- Deselected items are saved as manager-local pins.
- Future runs skip pinned items until unpinned.

Interactive mode requires a TTY.

---

## Output model

Item outcomes are reported using statuses:

- `current`
- `update`
- `delayed`
- `skipped`
- `error`

`--verbose` reveals additional metadata (for example, age context).

Output formatting can be adjusted with:

- `--plain`
- `--no-color`
- `--verbose`

---

## Logging and diagnostics

- Mutating operations are logged.
- `--debug-commands` enables detailed command-level diagnostics.
- `--show-commands` prints commands before execution.
