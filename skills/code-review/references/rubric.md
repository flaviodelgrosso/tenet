# Engineering review rubric

Use only the categories relevant to the change.

## Correctness

Look for invalid assumptions, boundary defects, missing error paths, inconsistent state transitions, partial implementations, and unintended behavior changes.

## Data integrity

Look for lossy/inconsistent transformations, partial writes, invalid persisted states, ordering/uniqueness/idempotency defects, and compatibility/migration risks.

## Reliability and lifecycle

Look for leaked resources, incomplete cleanup, duplicate side effects on retry, poor failure isolation, unbounded work, and brittle availability/order/timing assumptions.

## Concurrency / async

When relevant, inspect races, deadlocks, ordering dependencies, cancellation, shared mutable state, duplicate execution, and atomicity boundaries.

## Security / trust boundaries

When relevant, inspect untrusted input validation, authorization, injection paths, secret exposure, unsafe parsing/deserialization, privilege mistakes, insecure defaults, and sensitive error/log leakage.

Make security findings concrete; do not raise generic fears.

## API / compatibility

Look for breaking interface changes, inconsistent semantics, surprising defaults, missing validation, changed error contracts, and callers not updated.

## Maintainability

Look for unnecessary complexity, duplicated business rules, hidden side effects, misleading names/comments, dead scaffolding, and coupling that makes correctness harder to preserve.

Do not recommend abstraction merely to reduce line count.

## Performance

Report only plausible material impact: unbounded work, repeated expensive operations, realistic N+1/quadratic behavior, excessive I/O/serialization/copying, or blocking on latency-sensitive paths.

Avoid micro-optimization speculation.

## Tests

Look for missing behavior/regression coverage, tests tied only to implementation details, weakened/deleted assertions, mocks bypassing the behavior, flaky timing/order dependence, and happy-path-only coverage where failure behavior matters.
