# Agent Instructions

This project values architectural clarity over forward motion. Treat the implementation plan and architecture notes as a contract, not background reading.

## Working Style

- Read the relevant architecture/spec docs before editing.
- Before code changes, state the intended change boundary and files likely to be touched.
- Implement one phase at a time. Do not pull future phases into the current change.
- If the requested change conflicts with the architecture, stop and explain the conflict.
- If a clean implementation seems to require a new architectural layer, stop and propose options before editing.

## Design Constraints

- Prefer direct, explicit code over new abstraction.
- Do not introduce traits, wrappers, generic helpers, registries, or parallel flows unless they are required by the current phase.
- A new abstraction must have a clear reason now, not just anticipated future use.
- Prefer deleting accidental complexity over adapting more code to it.
- Do not preserve internal compatibility with a bad design decision.

## Testing

The test suite is presumed guilty. A test survives or is added only when it protects behavior that would intentionally survive a valid redesign. The burden of proof is on the test, not on the person deleting it.

Allowed reasons to add or keep a test:

- It protects user-visible CLI behavior.
- It protects a public API contract.
- It protects a domain invariant.
- It protects manager behavior that is part of the product contract.
- It protects version policy behavior.
- It protects parsing behavior where malformed or real-world input matters.
- It protects config behavior users rely on.
- It protects important error handling.
- It protects integration behavior that would be expensive or risky to manually validate.
- It is a regression test for a real bug we still care about.

Delete or reject tests that primarily:

- Test private helpers, constructors, getters, setters, or simple data plumbing.
- Assert implementation order, mocks-called behavior, helper behavior, or internal state.
- Encode current module boundaries, internal types, current render helpers, or architecture scaffolding.
- Mirror production logic or prove fixtures rather than product behavior.
- Duplicate behavior already covered by a stronger integration or stable-boundary test.
- Use snapshots/goldens without a clear user contract.
- Require large setup for tiny internal assertions.
- Exist to preserve coverage percentage, document current implementation, or make a refactor feel safer.

Rewrite tests rarely. Rewrite only when the protected behavior is clearly valuable, the current test is coupled to internals, the replacement moves to a stable boundary, the replacement is smaller than the deleted test, and no new test infrastructure is needed. Otherwise delete.

If a test needs a paragraph of justification, it should not exist.

## Reviews

- In code review, prioritize bugs, regressions, architectural drift, missing behavior coverage, and unnecessary abstractions.
- Call out abstractions with only one caller or one implementation.
- Call out helpers that hide unclear ownership or data flow.
- When reviewing recent work, assume deletion is allowed.

## Handoff

When starting a new context or phase, preserve:

- Current goal
- Implemented phases
- Key architecture decisions
- Rejected approaches
- Relevant files
- Current risks
- Next phase

Rejected approaches are important. Do not rediscover or reintroduce them without explaining what changed.
