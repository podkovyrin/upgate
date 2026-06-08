# upgate Workspace

This directory contains the active `upgate` crates. The repository root owns the
Cargo workspace, and the `upgate-cli` crate provides the `upgate` binary.

## Local Checks

Run these commands from the repository root:

```sh
cargo check --workspace
cargo test --workspace
```

## Documentation

- [High-level spec](docs/spec.md)
- [Architecture contract](docs/architecture.md)
- [Configuration](docs/config.md)
- [Version policy](docs/version-policy.md)
- [Security audit feature spec](docs/security-audit.md)
- [Performance profiling](docs/perf.md)
