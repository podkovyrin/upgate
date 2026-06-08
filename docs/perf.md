# Performance Profiling Notes

This project includes a lightweight profiling scaffold to measure end-to-end CLI runtime.

## Quick start

```bash
scripts/profile.sh
```

Default command profiled:

```bash
target/release/upgate -S brew.no_update=true
```

## Compare `--max-parallel-checks-per-manager`

```bash
scripts/profile.sh --compare-parallel -- -S brew.no_update=true
```

Default matrix values:

- `1,2,4,6,8,12`

You can customize values:

```bash
scripts/profile.sh --compare-parallel --parallel-values 1,2,3,4,6,8,10,12 -- -S brew.no_update=true
```

## Labeling and storing run history

Use labels to make result folders easier to compare:

```bash
scripts/profile.sh --label baseline --compare-parallel -- -S brew.no_update=true
scripts/profile.sh --label stage1-npm-pnpm-bun --compare-parallel -- -S brew.no_update=true
```

Artifacts are stored under:

- `.perf/runs/<run-id>/`
  - `hyperfine.json`
  - `hyperfine.csv`
  - `hyperfine.md`
  - `hyperfine.txt` (console output)
  - `meta.env` (git SHA/branch/flags)
  - `upgate-args.txt`
  - `system.txt`
- `.perf/index.tsv` (append-only run index)
- `.perf/runs/latest` (symlink to latest run)

## Custom command args

```bash
scripts/profile.sh --runs 10 --warmup 2 -- --managers brew -S brew.no_update=true
```

## Notes

- The profiling script defaults to `-S brew.no_update=true` to reduce brew update noise in baseline runs.
- For realistic user-facing performance, pass explicit args without this override.
- Avoid passing `--max-parallel-checks-per-manager` directly when using `--compare-parallel`.
- Manager concurrency is configured separately with `[upgate].manager_concurrency`
  or `--manager-concurrency`, so total metadata pressure can exceed the
  per-manager value when multiple managers run at once.
