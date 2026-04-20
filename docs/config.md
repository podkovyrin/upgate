# upnow Configuration (User-Level)

## Config file location

`upnow` reads configuration from:

1. `$XDG_CONFIG_HOME/upnow/config.toml`
2. fallback: `~/.config/upnow/config.toml`

If the file is missing, built-in defaults are used.
If the file exists but is invalid TOML, `upnow` exits with an error.

---

## Configuration shape

Configuration has:

- global section: `[upnow]`
- manager sections: `[brew]`, `[npm]`, etc.

Example:

```toml
[upnow]
scan_old_age_threshold = "365d"

[brew]
mode = "apply"
min_release_age = "12h"
no_update = false

[npm]
mode = "apply"
min_release_age = "7d"
pinned = ["typescript"]
```

---

## Global settings

### `[upnow].scan_old_age_threshold`

- Default: `365d`
- Used in verbose scan output to mark older releases.

---

## Per-manager settings

### `mode`

Controls when a manager is allowed to run:

- `off`: never run
- `plan`: run in `plan` and `scan`
- `apply`: run in `plan`, `scan`, and `apply`

### `min_release_age`

Defines how old a release must be before `upnow` considers it eligible for upgrade.

Common defaults:

- `brew`: `12h`
- most others: `7d`

### `pinned`

Optional list of package/tool names to skip for that manager.

### `no_update` (brew only)

Optional Homebrew-specific behavior toggle.

---

## Known built-in manager IDs

- `brew`, `bun`, `cargo`, `npm`, `yarn`, `mise`, `pipx`, `pnpm`, `uv`, `go`, `gem`, `dotnet`

---

## CLI overrides (`--set` / `-S`)

You can override config values at runtime:

```bash
upnow plan -S brew.no_update=true -S npm.min_release_age=14d
```

Format:

- `<section>.<key>=<value>`
- `<section>` is `upnow` or a manager ID

Notes:

- Overrides take precedence over file config.
- Unknown sections/keys or invalid values cause an error.
- `--managers` selects managers, but does not bypass `mode`.

---

## Interactive apply and pins

With `upnow apply --interactive`:

- You choose which updates to apply.
- Deselected items are persisted to that manager’s `pinned` list.

This lets users maintain a stable denylist for selected tools.

---

## Duration format

Duration fields like `min_release_age` and `scan_old_age_threshold` use compact units:

- `s` = seconds
- `m` = minutes
- `h` = hours
- `d` = days

Examples:

- `"12h"`
- `"7d"`
- `"90m"`
