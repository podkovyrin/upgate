# Pipx module (internal)

`src/pipx.rs` implements pipx-managed Python package upgrades.

## Interface

- Public to crate only: `run(cli: &Cli) -> anyhow::Result<()>`
- No other public API.
- Called from `src/main.rs` when `--managers` includes `pipx`.

## Scope (v1)

- `pipx` only.
- `uv` / `pip` tracks are intentionally not included yet.

## Flow

1. Read installed pipx packages: `pipx list --json`
2. For each main package, fetch PyPI metadata: `https://pypi.org/pypi/<name>/json`
3. Compare installed vs latest and read latest publish time
4. Apply fixed delay policy: `7d`
5. Print plan lines:
   - upgrade: `pipx: <name> vX -> vY (source: pypi)`
   - delayed: `pipx: <name> vX -> vY (delayed, A < 7d, source: pypi)`
6. If not dry-run: run `pipx upgrade <name>` for eligible packages

## Design choices

- Isolated module, no shared upgrade logic with brew/npm/mise internals.
- Uses PyPI timestamps directly for delay enforcement.
- Per-package `pipx upgrade` keeps behavior explicit and predictable.
