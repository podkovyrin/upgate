# upgate Workspace

This directory contains the active `upgate` crates. The repository root owns the
Cargo workspace, and the `upgate-cli` crate provides the `upgate` binary.

## Local Checks

Run these commands from the repository root:

```sh
cargo check --workspace
cargo test --workspace
```

## Configuration

`upgate` reads user config from `$XDG_CONFIG_HOME/upgate/config.toml`, or
`~/.config/upgate/config.toml` when `XDG_CONFIG_HOME` is not set. If the file is
missing, built-in defaults are used.

Use [config.toml](config.toml) as a copy-paste starting point. It includes all
built-in managers and the hardcoded defaults.

CLI overrides use `-S section.key=value`, for example:

```sh
upgate plan -S npm.min_release_age=14d -S brew.no_update=true
```

## Documentation

- [High-level spec](docs/spec.md)
- [Architecture contract](docs/architecture.md)
- [Version policy](docs/version-policy.md)
- [Security audit feature spec](docs/security-audit.md)
- [Performance profiling](docs/perf.md)
