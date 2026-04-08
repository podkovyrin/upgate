# Performance Profiling Notes

This project includes a lightweight profiling scaffold to measure end-to-end CLI runtime.

## Quick start

```bash
scripts/profile.sh
```

Default command profiled:

```bash
target/release/brew-delay-upgrade --dry-run --no-update
```

## Compare parallelism levels

```bash
scripts/profile.sh --compare-parallel -- --dry-run --no-update
```

This runs a matrix over `--max-parallel-checks` values: 1,2,4,6,8,12.

## Custom command args

```bash
scripts/profile.sh --runs 10 --warmup 2 -- --dry-run --min-release-age 24h --no-update
```

## Output

- JSON report: `.perf/hyperfine-<timestamp>.json`
- Console summary from hyperfine

## Notes

- Use `--no-update` while profiling internal pipeline changes; otherwise network + brew update noise dominates.
- For realistic user-facing performance, run another profile without `--no-update`.
