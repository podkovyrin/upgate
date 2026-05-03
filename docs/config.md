# upnow Configuration (User-Level)

## Config file location

`upnow` reads configuration from:

1. `$XDG_CONFIG_HOME/upnow/config.toml`
2. fallback: `~/.config/upnow/config.toml`

If the file is missing, built-in defaults are used.
If the file exists but is invalid TOML, `upnow` exits with an error.

Configuration precedence is:

1. built-in defaults
2. config file values
3. CLI arguments

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
version_policy = "stable"
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

### `version_policy`

Optional prerelease eligibility policy for update target selection.

Supported values:

- `stable`: only final releases are eligible
- `same-track`: follow the installed stability track (never move to a less stable lane)

If unset, no version policy filtering is applied. The external migration
behavior for older `version_policy = "any"` configs is intentionally not
specified here.

Supported by:

- `brew`, `bun`, `cargo`, `dotnet`, `go`, `npm`, `pipx`, `pnpm`, `yarn`
- `gem` supports `stable` only

`gem` supports only `version_policy = "stable"`. This follows RubyGems'
native behavior: prereleases require explicit opt-in and are not selected by
normal update flows. For Gem, stable policy is target-safety oriented and does
not guarantee reporting prerelease-only newer versions as blocked.

Not supported by:

- `mise`, `uv`

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
upnow plan -S brew.no_update=true -S npm.min_release_age=14d -S npm.version_policy=stable
```

Format:

- `<section>.<key>=<value>`
- `<section>` is `upnow` or a manager ID

Notes:

- Overrides take precedence over file config.
- Unknown sections/keys or invalid values cause an error.
- `--managers` selects managers and runs those explicit selections even when
  their default or configured `mode` is `off`.
- `--set <manager>.mode=...` is a direct CLI value override and takes
  precedence over the implicit mode opt-in from `--managers`.

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
