# Configuration

`upnow` reads config from the XDG config location:

1. `$XDG_CONFIG_HOME/upnow/config.toml` (when `XDG_CONFIG_HOME` is set and non-empty)
2. `~/.config/upnow/config.toml` (fallback)

If the config file does not exist, built-in defaults are used.

If the config file exists but contains invalid TOML, `upnow` fails with an error.

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

- `<manager>.<key>=<value>`

Supported keys:

- `mode` for all managers (`off`, `plan`, `apply`)
- `min_release_age` for all managers
- `no_update` for `brew` only
- `upnow.scan_old_age_threshold` (global scan threshold)

Unknown managers, unknown keys, or malformed values fail fast with an error.

`mode` semantics:
- `off`: manager never runs
- `plan`: manager runs in `upnow plan` and `upnow scan`
- `apply`: manager runs in `upnow plan`, `upnow scan`, and `upnow apply`

Safety behavior:
- `--managers` does not bypass `mode`; use `--set <manager>.mode=...` to override.

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
