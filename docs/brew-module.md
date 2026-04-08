# Brew module (internal)

`src/brew.rs` owns all brew-upgrade planning logic and the CLI surface (`Cli`).

## Interface

- Public to crate only: `run() -> anyhow::Result<()>`
- No other public API.
- `src/main.rs` only calls `brew::run()`.

## Flow

1. Parse CLI (`--dry-run`, `--min-release-age`, `--max-parallel-checks`, `--no-update`)
2. Optional `brew update --quiet`
3. Read outdated set (`brew outdated --json=v2`)
4. Read package metadata (`brew info --json=v2 ...` single call)
5. Read tap metadata (`brew tap-info --json --installed`)
6. Build plan:
   - local git age check first
   - GitHub API fallback on local failure
   - classify as upgrade / delayed / skipped
7. Print only plan lines
8. If not dry-run: run `brew upgrade --formula ...` and `brew upgrade --cask ...`

## Design choices

- Keep implementation simple and self-contained in one module.
- Keep output stable and uniform.
- Prefer local git over API (faster/cheaper, no rate-limit).
- Bound parallel checks via `--max-parallel-checks`.
- Keep API fallback parallelism lower (`clamp(1, 4)`) to reduce burst/rate issues.

## Performance notes

Worked:
- Two-phase local->API pipeline: improves robustness and API pressure control.
- Bounded parallelism: avoids overloading GitHub fallback.
- Single `brew info` call (formula+cask together): lower subprocess overhead.

Did not materially improve (reverted):
- Batched git-per-tap scan via `git log --name-only` path scanning.
  - Too expensive/noisy on this workload; kept simpler per-package local git check.
