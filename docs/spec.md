# upnow Specification (High Level)

## Purpose

`upnow` helps users keep globally installed developer tools up to date across multiple package managers, while avoiding very new releases until they have "aged" past a configured threshold.

It supports three workflows:

- **plan**: show what would be updated
- **apply**: perform updates
- **scan**: show currently installed versions

---

## Supported managers

Built-in managers:

- `brew`, `bun`, `cargo`, `npm`, `mise`, `pipx`, `pnpm`, `uv`, `go`, `gem`, `dotnet`

Default execution modes:

- **apply**: `brew`, `bun`, `cargo`, `npm`, `mise`, `pipx`, `pnpm`, `uv`, `go`
- **off**: `gem`, `dotnet`

Users can change modes in config or with CLI overrides.

---

## Core behavior

- `plan` is non-applying (preview of upgrades).
- `apply` executes upgrades for selected managers/items.
- `scan` is non-mutating and focuses on installed versions.
- Managers can be selected with `--managers`; explicit CLI manager selection may opt managers into the requested command.
- Missing manager executables are treated as absent managers and omitted from
  normal output. A machine without a specific manager installed should produce
  no output for that manager.
- Unsupported installed manager versions or environments are reported without
  crashing the whole run.

---

## Delayed-upgrade model

Each manager has a `min_release_age` setting.

`upnow` prefers upgrade targets that are old enough to satisfy that threshold. This helps reduce risk from freshly published releases.

At a high level:

- if an eligible newer version exists → **update**
- if only too-new versions exist → **delayed**
- if already up to date → **current**

When multiple eligible newer versions remain after policy and release-age
filtering, planner-selectable managers choose the release with the newest
publish timestamp. Parsed version order is used only as a tie-breaker. This
keeps target selection biased toward recently maintained releases; use
`version_policy = "stable"` to exclude prereleases entirely.

---

## Interactive apply

`upnow apply` opens the interactive selection UI and allows users to choose
which upgrades to apply.

- All upgradable items are selected by default.
- Confirmed selection changes are saved as manager-local selection policy.
- Future runs apply that selection policy until changed.

Interactive mode requires a TTY.

`upnow apply --yolo` skips the interactive UI and applies the default selected
updates non-interactively. `--yes` and `--no-approval` are accepted aliases for
the same behavior.

---

## Output model

Item outcomes are reported as user-facing decisions, not as raw resolver
internals. The output contract has two parts:

- **status**: what happened, or what would happen
- **reason**: why that status was chosen

Reason details should be phrased for users. Internal names such as resolver
enum variants, classifier names, or low-level command plumbing should not appear
in normal output.

Item outcomes use these statuses:

- `current`
- `update`
- `delayed`
- `blocked`
- `skipped`
- `error`

Normal output should answer:

- what will happen, or what happened
- why an item is not being updated
- why the selected update is not the newest known version, when relevant

`--verbose` reveals evidence behind the decision, such as age comparisons,
release metadata, policy details, and candidate eligibility. Verbose output must
not be required to understand whether the tool is updating, delaying, skipping,
or failing an item.

Output formatting can be adjusted with:

- `--plain`
- `--no-color`
- `--verbose`

### Output statuses and reasons

The exact symbols may follow the existing style, but these status meanings and
normal/verbose boundaries are part of the spec.

| Status | Reason / situation | Normal output | Verbose-only detail |
| --- | --- | --- | --- |
| `update` | Eligible newer version selected | Show manager, item, installed version, and selected target. Example: `+ Update [npm] foo v1.2.0 -> v1.2.5` | Candidate source, release age, selected policy, resolver decision |
| `update` | Selected target is eligible, but latest known version is too fresh | Add concise note: `(v1.3.0 too fresh)` | Candidate release age as a separate note, plus candidate eligibility |
| `update` | Selected target is eligible, but latest known version is blocked by version policy | Add concise note: `(v2.0.0 blocked by policy)` | Installed release class, latest policy-eligible version, blocked candidate classification |
| `current` | Scan reports an installed version | In `scan`, show current item. Example: `= Current [npm] foo v1.2.0` | Release age, if available. Old releases may be highlighted according to `scan_old_age_threshold` |
| `current` | No newer version exists in plan/apply | Hide in normal `plan` and `apply` output | May show as `= Current ... (no newer version found)` |
| `current` | Newer versions exist but are blocked by version policy | Show because action was withheld by policy. Example: `= Current [pipx] bar v2.0.0rc1 (v2.1.0 blocked by policy)` | Policy warning, installed/candidate release classes |
| `delayed` | Candidate target exists but is too fresh | Show target and concise age gate: `~ Delayed [brew] jq v1.0.0 -> v1.1.0 (too fresh: 3d < 7d)` | Publish time, metadata source, configured minimum age source |
| `delayed` | Newer versions exist, but no candidate is age-eligible | Show concise note: `~ Delayed [npm] foo v1.2.0 -> v1.3.0 (no eligible release yet; v1.3.0 too fresh)` | Candidate release ages when diagnostics are available |
| `delayed` | Policy and age gates together leave no eligible release | Show `no eligible release yet`, plus relevant policy note when a latest version is blocked by policy | Which candidates failed policy vs age, latest policy-eligible version, latest age-eligible version |
| `blocked` | Missing metadata prevents a safe decision | Show item and concise reason: `x Blocked [mise] foo v1.2.0 -> v1.3.0 (missing release metadata)` | Missing field/source, fallback attempts, command or URL diagnostics |
| `skipped` | Excluded by user/config/interactive selection policy | Show item and target when known: `- Skipped [npm] foo v1.2.0 -> v1.3.0 (not selected)` | Selection source, if available |
| `skipped` | Manager is present but unsupported for this operation | Show manager-level skip without placeholder versions | Probe command and PATH-related diagnostics, when available |
| `skipped` | Unsupported platform/environment | Show manager-level skip without placeholder versions: `- Skipped [brew] (unsupported on this platform)` | Platform details and manager support condition, when available |
| `error` | Command, resolver, or metadata check failed unexpectedly | Show item or manager plus concise failure: `! Error [npm] foo v1.2.0 -> v1.3.0 (failed to query package metadata)` | Command, exit status, stderr summary, retry/fallback context |
| `error` | Invalid or unsupported configuration | Show explicit configuration problem. Example: `! Error [gem] foo (version_policy "same-track" is not supported by this manager)` | Manager capability details and configured source, when available |

### Reason groups

Implementations may use internal reason codes, but they should map to these
user-facing reason groups:

| Reason group | Examples |
| --- | --- |
| Availability | missing command, unsupported platform |
| User intent | not selected |
| Version state | no newer version, eligible update, too fresh, no eligible release |
| Policy | blocked by policy, policy warning, unsupported policy |
| Metadata | missing metadata, stale metadata, unparseable metadata |
| Failure | command failed, network failed, resolver failed |

Normal output should include reason groups that change what the user can expect.
Verbose output should include evidence that remains concise in-line. Release age
may appear as a separate note, such as `11d ago`; threshold comparisons and
policy names are reserved for detailed diagnostics rather than normal plan/TUI
notes.

---

## Logging and diagnostics

- Mutating operations are logged.
- `--log-commands` enables detailed command-level diagnostics.
- `--trace-commands` prints commands before execution; `--show-commands` is an alias.
- `--dry-run` skips mutating commands while still planning what would run.
