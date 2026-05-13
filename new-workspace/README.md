# upnow Rebuild Workspace

This directory contains the rebuilt `upnow` crates. The repository root now owns
the active Cargo workspace; the old `/src` tree is behavioral reference only.

## Local Checks

Run these commands from the repository root:

```sh
cargo check --workspace
cargo test --workspace
```
