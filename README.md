# 🚦 upgate

**The gate between available and ready updates**

A new version can be available before it is ready for your system. `upgate`
waits for the configured release age, applies the version policy, and checks
[OSV](https://osv.dev/) when it can identify the exact package. You see the
result and choose what gets updated.

Package managers still find and install updates in their own way. `upgate`
checks the possible updates before it runs their commands. This makes supply
chain attacks harder, but still it cannot prove that a release is safe.

## Install

`cargo install` builds `upgate` from source, so it needs Rust 1.88 or newer.

```sh
cargo install upgate-cli
```

To build the current source:

```sh
git clone https://github.com/podkovyrin/upgate.git
cd upgate
cargo install --path crates/upgate-cli --locked
```

macOS is the main platform today. Linux is supported too, but it has not had the
same amount of real-world testing yet.

## Start here

```sh
upgate
```

This command scans installed tools, builds an update plan, and opens the
interactive picker. The picker shows current tools, available updates, and
anything delayed, blocked, or failed. Choose the updates you want, then run
them from the picker.

For read-only checks and other workflows:

```sh
# List installed tools
upgate scan

# Build a plan without updating anything
upgate plan

# Run only selected package managers
upgate plan --managers npm,cargo

# Preview the commands without running them
upgate apply --dry-run

# Apply the default selection without opening the picker
upgate apply --yolo
```

Add `--verbose` when you need release-age, version-policy, or security details.

## Supported package managers

Defaults and supported features differ by manager.

| Package manager | Default mode | Minimum release age | Version policies | OSV check |
| --- | --- | --- | --- | --- |
| Homebrew | `apply` | 12 hours | `none`, `stable`, `same-track` | When the Git source is known |
| Bun | `apply` | 7 days | `none`, `stable`, `same-track` | npm |
| Cargo | `apply` | 7 days | `none`, `stable`, `same-track` | crates.io |
| npm | `apply` | 7 days | `none`, `stable`, `same-track` | npm |
| mise | `apply` | 7 days | `none` | Known backend or Git source |
| pipx | `apply` | 7 days | `none`, `stable`, `same-track` | PyPI |
| pnpm | `apply` | 7 days | `none`, `stable`, `same-track` | npm |
| uv | `apply` | 7 days | `none` | PyPI |
| Go | `apply` | 7 days | `none`, `stable`, `same-track` | Go |
| RubyGems | `off` | 7 days | `stable` | RubyGems |
| .NET tools | `off` | 7 days | `none`, `stable`, `same-track` | NuGet |

The OSV column shows where package identity comes from. A check runs only when
`upgate` can make an exact mapping. Missing package-manager executables are
skipped.

## Configuration

The config file is `$XDG_CONFIG_HOME/upgate/config.toml`, or
`~/.config/upgate/config.toml` when `XDG_CONFIG_HOME` is not set. Without a
config file, `upgate` uses the defaults above.

```toml
[npm]
min_release_age = "14d"
version_policy = "same-track"

[npm.selection]
mode = "skip"
except = ["typescript"]

[gem]
mode = "plan"
```

Manager mode can be:

- `apply`: include the manager in every command
- `plan`: scan and plan, but do not update
- `off`: skip the manager

A selection rule controls the default package selection. `except` always means
the opposite of `mode`. The example skips npm packages by default, except for
`typescript`.

Release age accepts seconds, minutes, hours, and days, such as `30m`, `12h`, or
`7d`. npm accepts whole days only.

Version policy can be:

- `none`: do not filter prereleases
- `stable`: use final releases only
- `same-track`: stay at least as stable as the installed version

Not every policy fits every manager. Unsupported combinations are rejected.
See the full [config example](config.toml) for every setting.

Use `--set` or `-S` for one run:

```sh
upgate plan --set npm.min_release_age=14d --set brew.no_update=true
```

## Release age and security

A version younger than `min_release_age` is not selected by the normal plan.
When a manager provides version history, an older eligible version can be used
instead. Waiting gives problems time to become visible. It is not proof that a
release is safe.

OSV checks run only for packages with an exact ecosystem and package identity.
For those packages, a vulnerable target or a failed OSV request is blocked by
default. Packages without an OSV mapping are shown without an audit result and
are not blocked.

The interactive picker can offer a forced target for some blocked updates. When
that option is present, you can choose it explicitly.

`upgate scan --verbose` audits installed versions. Normal `scan` does not
contact OSV.

Run `upgate --help` to see all command options.

## License

[MIT](LICENSE)
