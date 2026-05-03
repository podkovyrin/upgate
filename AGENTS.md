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

- Test behavior at stable boundaries: CLI behavior, public APIs, or durable module interfaces.
- Do not write tests that primarily lock in private helpers or incidental structure.
- If a test requires exposing internals, stop and explain why.
- Tests should validate product behavior and important error cases, not justify a new abstraction.

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
