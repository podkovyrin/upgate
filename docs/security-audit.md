# Security Audit Feature Spec

This document is the implementation contract for the MVP security-audit
feature. It extends the architecture in `docs/architecture.md`.

## Goal

`upnow` should use OSV.dev to report and gate known vulnerabilities for globally
installed developer tools.

The MVP uses OSV because it aggregates the advisory sources needed for the
supported ecosystems and exposes a no-auth batch API.

Security audit affects:

- `scan --verbose`: audit installed versions and show short vulnerability notes.
- `plan`: audit update targets and block unsafe supported targets.
- `apply`: use the same audited plan as `plan`; execution must not re-decide
  audit.

## Non-Goals

- Do not build manager-specific audit clients.
- Do not query non-OSV vulnerability APIs in the MVP.
- Do not audit unsupported tools by guessing ecosystems.
- Do not add CPE matching, binary inspection, SBOM generation, transitive
  dependency scanning, or source-tree scanning.
- Do not make presentation code create audit outcomes.
- Do not allow manager adapters to decide whether audit blocks an update.

## Layering

Add a shared audit layer:

- `upnow-domain`: typed audit identities, queries, results, finding summaries,
  and plan/scan audit facts.
- `upnow-audit`: OSV client, batch splitting, de-duplication, process-local
  cache, response parsing, and audit service concurrency.
- `upnow-planning`: derives update-target audit queries and applies audit as a
  gate.
- `upnow-cli`: owns one audit service instance per command run and orchestrates
  manager discovery, audit lookup, and planning.
- `upnow-managers`: emit optional typed audit identities for installed tools.
- `upnow-presentation`: render short audit notes and TUI audit details from
  typed plan/scan facts.

Managers must not call OSV, parse OSV responses, decide vulnerability severity,
decide blocking behavior, or format audit output.

## Domain Model

The exact Rust names may vary, but the implementation should preserve these
concepts.

### Audit Subject

An audit subject is the package identity used by OSV:

```text
AuditSubject {
  ecosystem: OsvEcosystem,
  package_name: AuditPackageName,
}
```

It is attached to installed tool facts when the manager can identify the OSV
package without guessing.

Unsupported tools have no audit subject. Unsupported is not an error.

### Audit Query

An audit query is an audit subject plus a concrete version:

```text
AuditQuery {
  subject: AuditSubject,
  version: VersionText,
}
```

The service de-duplicates queries by `(ecosystem, package_name, version)`.

### Audit Result

Planning and presentation need typed results, not raw OSV JSON:

```text
AuditLookupResult =
  Unsupported
  Clean
  Vulnerable { findings: Vec<AuditFinding> }
  LookupFailed { detail: String }
```

`Unsupported` means no audit subject exists. It should normally be represented
by absence of a query/result rather than a failed lookup.

### Audit Finding

Each finding should include enough detail for short notes and TUI details:

```text
AuditFinding {
  id: String,              # OSV id, e.g. GHSA-...
  aliases: Vec<String>,    # CVE/GHSA aliases when present
  summary: Option<String>,
  severity: Option<String>,
  references: Vec<String>,
}
```

Display code should prefer a compact identifier list in normal notes. Details
can show the summary and links.

## Supported Audit Subjects

Managers emit audit subjects only when mapping is explicit and stable.

Expected MVP mappings:

- `npm`: `npm`
- `pnpm`: `npm`
- `bun`: `npm` for npm-compatible global packages
- `cargo`: `crates.io`
- `pipx`: `PyPI`
- `uv`: `PyPI` when the selected/installed package maps to a PyPI package
- `gem`: `RubyGems`
- `go`: `Go`
- `dotnet`: `NuGet`
- `brew`: `GIT` when Homebrew package JSON provides a non-empty `repo_url`
  or `urls.head.url`; the OSV package name is that repository URL
- `mise`: only when registry backend data gives an unambiguous supported OSV
  ecosystem/package, such as `npm:`, `pipx:`, `uvx:`, `cargo:`, `go:`, or
  `gem:`. A single `github:owner/repo` backend maps to `GIT` package
  `https://github.com/owner/repo.git`

Expected unsupported or partial mappings:

- `brew`: unsupported when Homebrew package JSON does not provide `repo_url`
  or `urls.head.url`. Formula/cask names and stable source archive URLs are
  distributions of tools, not reliable OSV package identities.
- `mise` backends such as `ubi`, `aqua`, `asdf`, or ambiguous backend lists are
  unsupported unless a precise OSV ecosystem/package is available.

Do not infer ecosystems from display names, homepages, source URLs, cask tokens,
or free-form manager metadata.

## Behavior

### Scan

`scan` without `--verbose` is unchanged.

`scan --verbose` audits installed versions for tools with audit subjects.

For each installed tool:

- Unsupported: no audit request, no note.
- Clean: no note.
- Vulnerable: show a short note.
- Lookup failed: do not show noise for unsupported tools. For supported tools,
  a verbose-only warning may be shown, but it must not change scan status unless
  the implementation explicitly chooses to model audit lookup failure as an
  issue.

Scan never blocks or mutates.

### Plan and Apply

Audit is a gate after version policy and release age:

```text
version policy -> release age -> security audit
```

For supported audit subjects:

- Clean target: eligible.
- Vulnerable target: blocked.
- Lookup failed: blocked.

For unsupported tools:

- Audit is skipped silently.
- Existing policy/age behavior is unchanged.

Batch apply and interactive apply must use the same `UpdatePlan` produced by
planning. Execution must not query OSV or re-evaluate audit.

### Planner-Selectable Targets

Planner-selectable managers already provide release timelines. Planning should
choose the newest candidate that passes:

1. installed-version comparison
2. version policy
3. release age
4. security audit, when supported

If the newest policy+age candidate is vulnerable, planning must try older
policy+age candidates in the same timeline before blocking.

If no policy+age candidate has a clean audit result:

- return a blocked plan item;
- preserve diagnostics for the audit-blocked candidates;
- render a short audit note explaining the block.

If a supported candidate could not be audited because the OSV lookup failed, it
is not eligible. If all otherwise eligible candidates fail audit lookup, the item
is blocked.

### Manager-Selected Targets

Manager-selected targets are required for Brew, uv, and Mise. Planning must not
replace the manager-selected target with another version.

For manager-selected targets:

- supported + clean: existing policy/age behavior continues;
- supported + vulnerable: block the selected target;
- supported + lookup failed: block the selected target;
- unsupported: existing policy/age behavior is unchanged.

Audit must not choose an older safe version for manager-selected managers.

## Plan Status and Diagnostics

Add an audit block reason to the plan model. The exact enum layout may vary, but
the plan must distinguish these cases for presentation:

- vulnerable target;
- audit lookup failed for a supported target.

Candidate diagnostics should record per-version audit facts for candidates that
were considered. This is needed for:

- verbose plan notes;
- TUI target picker details;
- explaining why a newer target was not selected;
- testing planner behavior at a stable boundary.

Suggested additions:

```text
CandidateEvaluationFact {
  ...
  audit: Option<CandidateAuditFact>
}

CandidateAuditFact =
  Clean
  Vulnerable { findings }
  LookupFailed { detail }
```

Do not store raw OSV JSON in plan diagnostics.

## OSV Service

Use OSV API endpoint:

```text
POST https://api.osv.dev/v1/querybatch
```

with request shape:

```json
{
  "queries": [
    {
      "version": "1.2.3",
      "package": {
        "name": "example",
        "ecosystem": "npm"
      }
    }
  ]
}
```

The base URL must be overridable for tests, following existing manager metadata
lookup patterns. Suggested environment variable:

```text
UPNOW_OSV_BASE_URL
```

The current HTTP infrastructure only supports GET. Implementation must add a
small POST JSON/text capability to `upnow-infra::HttpClient` and fake HTTP
support for tests.

### Batching

The audit service owns batching. Managers and planning code should pass query
objects, not construct HTTP requests.

Rules:

- De-duplicate globally by `(ecosystem, package_name, version)`.
- Split by query count, not by manager.
- Use a conservative default chunk size, such as 100 queries.
- Keep chunk size configurable internally if needed; do not expose it as user
  config unless there is a product reason.
- Preserve result mapping back to each original query.

### Concurrency

The audit service has its own global concurrency cap.

Suggested MVP config:

```toml
[upnow]
audit_concurrency = 8
```

Rules:

- Default: `8`.
- Valid range: `1..=16`.
- This is independent from manager concurrency.
- The service must enforce the cap globally for the command run, even when
  multiple manager workers request audit concurrently.

Manager workers may submit audit batches as soon as they have manager update
inputs. The shared audit service is responsible for de-duplication, caching, and
request concurrency.

### Caching

Use process-local caching only for the MVP.

Cache key:

```text
ecosystem + package_name + version
```

Cache clean, vulnerable, and lookup-failed results for the duration of the
command run. Do not add persistent cache in the MVP.

## CLI Orchestration

Planning should become explicitly two-phase for audit-supported runs:

```text
manager.update_inputs(...)
planning.derive_audit_queries(inputs, settings)
audit_service.query(...)
planning.finalize_plan(inputs, settings, audit_results)
```

Do not let CLI duplicate policy, age, or candidate selection logic. If audit
query derivation needs to know which versions survive policy and age, expose
that from `upnow-planning`.

For scan verbose:

```text
manager.scan_inputs_with_release_evidence(...)
CLI derives installed-version audit queries from installed tools with subjects
audit_service.query(...)
build ScanReport with release evidence and audit evidence
```

Non-verbose scan does not query audit.

## Presentation

### Batch Plan/Apply Notes

Use short notes in the existing notes column.

Suggested text:

- Vulnerable target: `vulnerable: GHSA-...`
- Multiple advisories: `vulnerable: GHSA-..., CVE-...`
- Audit lookup failure: `audit unavailable`

Normal plan/apply should show audit notes when audit changes the outcome. Clean
targets need no audit note.

### Scan Notes

In `scan --verbose`, show the same short vulnerability note for installed tools
with findings. Clean and unsupported tools get no audit note.

### TUI

The table row notes should include the short audit note when relevant.

The target picker should expose vulnerability details for the highlighted target
when those details exist:

- advisory ids and aliases;
- summary, if present;
- reference links, if present.

This should render below the version picker rows and above the key footer, or in
an equivalent details area inside the picker. Keep the key footer available.

Audit-blocked targets must not be silently converted into selectable safe
targets. If the implementation displays audit-blocked candidates in the picker,
they must be clearly marked as violations. Do not add a `PlanSelection` bypass
for audit-blocked targets in the MVP unless the architecture is updated to make
security-audit bypass an explicit product feature.

## Error Handling

Plan/apply:

- Supported audit subject + OSV request failure: fail closed for affected
  candidates.
- Unsupported audit subject: no query, no block, no note.
- Malformed OSV response for a batch: treat affected queries as lookup failed.
- OSV response with no vulnerabilities: clean.

Scan verbose:

- Supported audit subject + OSV request failure may be reported as a verbose
  audit warning, but unsupported tools must remain silent.
- Scan must remain non-mutating.

Interruptions should propagate like other manager/network interruptions when
possible. Non-interruption lookup failures become typed audit lookup failures.

## Testing Guidance

Add tests only at stable behavior boundaries.

Useful tests:

- planner-selectable target falls back from vulnerable newer candidate to clean
  older policy+age candidate;
- planner-selectable item blocks when every policy+age candidate is vulnerable;
- supported target blocks when OSV lookup fails;
- unsupported subject does not block and does not add notes;
- manager-selected target is blocked when vulnerable and is not replaced by an
  alternate version;
- scan verbose shows vulnerability notes for installed tools;
- non-verbose scan does not query audit;
- OSV batch response parsing maps results back to the right queries;
- audit service de-duplicates duplicate queries in one command run;
- config rejects invalid `audit_concurrency`.

Avoid tests that assert private helper order, internal chunk boundaries, mock
call counts unrelated to product behavior, or current module boundaries.

## Implementation Phases

### Phase 1: Domain and Architecture Plumbing

- Add typed audit identities/results to `upnow-domain`.
- Attach optional audit subject to installed tool/update facts.
- Add audit diagnostics to plan candidate facts.
- Update config model for `[upnow].audit_concurrency`.

### Phase 2: OSV Service

- Add POST support to `upnow-infra::HttpClient`.
- Add `upnow-audit` with OSV querybatch request/response models.
- Implement de-duplication, process-local cache, chunking, and concurrency.
- Add fake HTTP coverage for OSV parsing and lookup behavior.

### Phase 3: Manager Audit Subjects

- Add audit subject emission for managers with explicit ecosystem mappings.
- Add Brew `GIT` audit subjects only when Homebrew package JSON provides
  `repo_url` or `urls.head.url`.
- Add Mise `GIT` audit subjects only for explicit or single registry
  `github:owner/repo` backends.
- Keep ambiguous Mise backends unsupported.
- Do not add OSV calls or audit decisions to managers.

### Phase 4: Planning Gate

- Add two-phase planning API for audit query derivation and final evaluation.
- Apply audit after policy and age.
- Preserve manager-selected target semantics.
- Add stable-boundary planner tests.

### Phase 5: CLI and Presentation

- Create one audit service per command run.
- Wire audit into verbose scan, plan, batch apply, and interactive apply.
- Add short batch notes.
- Add TUI notes and target-picker details.
- Update user configuration docs.

## Resolved Decisions

- use best fitting names for audit domain types and the `upnow-audit` crate.
- `audit_concurrency` does not need a CLI override in the MVP.
- scan verbose should show supported-subject lookup failures as short notes
- user in TUI can explicit override applying audit-blocked targets.
