# Performance Profiling Notes

This project includes a lightweight profiling scaffold to measure end-to-end CLI runtime.

## Quick start

```bash
scripts/profile.sh
```

Default command profiled:

```bash
target/release/upnow --dry-run --no-update
```

## Compare `--max-parallel-checks`

```bash
scripts/profile.sh --compare-parallel -- --dry-run --no-update
```

Default matrix values:

- `1,2,4,6,8,12`

You can customize values:

```bash
scripts/profile.sh --compare-parallel --parallel-values 1,2,3,4,6,8,10,12 -- --dry-run --no-update
```

## Labeling and storing run history

Use labels to make result folders easier to compare:

```bash
scripts/profile.sh --label baseline --compare-parallel -- --dry-run --no-update
scripts/profile.sh --label stage1-npm-yarn-pnpm-bun --compare-parallel -- --dry-run --no-update
```

Artifacts are stored under:

- `.perf/runs/<run-id>/`
  - `hyperfine.json`
  - `hyperfine.csv`
  - `hyperfine.md`
  - `hyperfine.txt` (console output)
  - `meta.env` (git SHA/branch/flags)
  - `upnow-args.txt`
  - `system.txt`
- `.perf/index.tsv` (append-only run index)
- `.perf/runs/latest` (symlink to latest run)

## Custom command args

```bash
scripts/profile.sh --runs 10 --warmup 2 -- --dry-run --min-release-age 24h --no-update
```

## Notes

- Use `--no-update` while profiling internal pipeline changes; otherwise network + brew update noise dominates.
- For realistic user-facing performance, run another profile without `--no-update`.
- Avoid passing `--max-parallel-checks` directly when using `--compare-parallel`.
