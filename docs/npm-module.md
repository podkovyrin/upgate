# NPM module (internal)

`src/npm.rs` owns npm global-upgrade planning logic.

## Interface

- Public to crate only: `run(cli: &Cli) -> anyhow::Result<()>`
- No other public API.
- Called from `src/main.rs` when `--managers` includes `npm`.

## Flow

1. Read outdated globals (`npm outdated -g --json`)
2. For each package, read publish time of target version (`npm view <name>@<latest> time --json`)
3. Apply fixed npm delay policy: `7d`
4. Print only plan lines
5. If not dry-run: run `npm -g update --min-release-age 7`

## Design choices

- Implemented from scratch, isolated from brew internals.
- Uses npm-native min-release-age for actual update execution.
- Keeps output format aligned with brew-style plan lines.

## Performance notes

- Main cost is per-package `npm view` requests.
- No shared optimization logic with brew by design.
- Current behavior prioritizes simplicity and correctness over aggressive batching.
