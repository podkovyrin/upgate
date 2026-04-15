# Configuration

`upnow` reads config from the XDG config location:

1. `$XDG_CONFIG_HOME/upnow/config.toml` (when `XDG_CONFIG_HOME` is set and non-empty)
2. `~/.config/upnow/config.toml` (fallback)

If the config file does not exist, built-in defaults are used.

If the config file exists but contains invalid TOML, `upnow` fails with an error.

## Source of truth in code

- `src/app/cli.rs`, `src/app/mod.rs` (CLI flags, validation, orchestration, exit behavior)
- `src/config/mod.rs`, `src/config/load.rs`, `src/config/model.rs`, `src/config/overrides.rs`, `src/config/path.rs`, `src/config/pins.rs` (config parsing/defaults/validation, CLI overrides, pin persistence)
- `src/manager/registry.rs` and `src/manager/context.rs` (manager registry, policy/context construction, pending interactive pins)
- `src/interactive/mod.rs` and `src/interactive/apply.rs` (interactive dialogs and apply/pin flow)

## Format

Config supports a global `[upnow]` section and manager sections keyed by manager ID.

```toml
[upnow]
# scan_old_age_threshold = "365d"

[brew]
# mode = "apply"
# min_release_age = "12h"
# no_update = false

[bun]
# mode = "apply"
# min_release_age = "7d"

[cargo]
# mode = "apply"
# min_release_age = "7d"

[npm]
# mode = "apply"
# min_release_age = "7d"
# pinned = ["typescript", "eslint"]

[yarn]
# mode = "apply"
# min_release_age = "7d"

[mise]
# mode = "apply"
# min_release_age = "7d"

[pipx]
# mode = "apply"
# min_release_age = "7d"

[pnpm]
# mode = "apply"
# min_release_age = "7d"

[uv]
# mode = "apply"
# min_release_age = "7d"

[go]
# mode = "apply"
# min_release_age = "7d"

[gem]
# mode = "off"
# min_release_age = "7d"

[dotnet]
# mode = "off"
# min_release_age = "7d"
```

Global keys:

- `scan_old_age_threshold` (default `365d`) — used by `upnow scan` in `--verbose` output to mark old releases.

Known manager IDs in the built-in registry:

- `brew`, `bun`, `cargo`, `npm`, `yarn`, `mise`, `pipx`, `pnpm`, `uv`, `go`, `gem`, `dotnet`

## CLI overrides

You can override config values from the command line with repeatable `--set` (`-S`) flags:

```bash
upnow plan -S brew.no_update=true -S npm.min_release_age=14d
```

Format:

- `<section>.<key>=<value>`
- `<section>` is either a manager ID (for manager keys) or `upnow` (for global keys)

Supported keys:

- `mode` for all managers (`off`, `plan`, `apply`)
- `min_release_age` for all managers
- `no_update` for `brew` only
- `upnow.scan_old_age_threshold` (global scan threshold)

Note: `pinned` is config-backed but not currently writable via `--set`; it is updated by interactive apply in this iteration.

Unknown managers, unknown keys, or malformed values fail fast with an error.

`mode` semantics:
- `off`: manager never runs
- `plan`: manager runs in `upnow plan` and `upnow scan`
- `apply`: manager runs in `upnow plan`, `upnow scan`, and `upnow apply`

Safety behavior:
- `--managers` does not bypass `mode`; use `--set <manager>.mode=...` to override.

## Interactive apply and pins

`upnow apply --interactive` prompts per manager with all upgradable items selected by default.
You can deselect items; deselected items are persisted into that manager's `pinned` list in config.

Pinned items are skipped in subsequent runs.
Interactive pin persistence updates only the manager `pinned` key and keeps unrelated config keys as-is.

For `npm`, `bun`, and `mise`, apply uses a hybrid strategy:
- Global command is used only when all upgradable items are selected and the manager `pinned` list is empty.
- Otherwise, selective per-item updates are used so manager-local pins are honored.

`--interactive` requires interactive `stdin` and `stdout` TTY; otherwise the command fails.

## Output flags (not config-backed)

These output controls are CLI flags only (not config file keys):

- `--plain` — force plain output (no color, ASCII arrow)
- `--no-color` — disable ANSI color styling
- `--verbose` — show additional metadata segments in output lines
- `--debug-commands` — persist command debug logs (stdout/stderr/timing) under XDG state
- `--show-commands` (alias: `--print-commands`) — print each command to stderr before execution

Example:

```bash
upnow plan --plain
upnow apply --verbose --no-color
upnow plan --show-commands
upnow plan --debug-commands
```

## Duration format

`min_release_age` values use these units:

- `s` seconds
- `m` minutes
- `h` hours
- `d` days

Examples: `"12h"`, `"7d"`, `"90m"`.

## npm apply constraint

For npm apply, `min_release_age` must be a whole number of days (for example `"7d"`, `"14d"`).

This is validated during manager policy setup whenever npm is selected.
If `[npm].min_release_age` is not a whole-day value (for example `"12h"`), `upnow` fails with an explanatory config error before npm runs.
