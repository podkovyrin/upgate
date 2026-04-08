# Mise module (internal)

`src/mise.rs` owns mise global-upgrade planning logic.

## Interface

- Public to crate only: `run(cli: &Cli) -> anyhow::Result<()>`
- No other public API.
- Called from `src/main.rs` when `--managers` includes `mise`.

## Flow

1. Build plan using mise-native dry-run:
   - `mise upgrade --dry-run --before 7d`
2. Parse dry-run lines:
   - `Would uninstall <tool>@<from>`
   - `Would install <tool>@<to>`
3. Emit normalized plan lines:
   - `mise: <tool> v<from> -> v<to> (source: mise, delay: 7d)`
4. If not dry-run, execute:
   - `mise upgrade --before 7d`

## Design choices

- Implemented from scratch, isolated from brew/npm internals.
- Uses mise-native delay filtering (`--before 7d`) for both plan and apply.
- Keeps output shape aligned with other managers.

## Performance notes

- Planning cost is mostly one mise command invocation and parsing text output.
- No cross-manager shared optimization logic by design.
