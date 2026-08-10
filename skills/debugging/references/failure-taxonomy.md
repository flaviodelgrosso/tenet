# Failure taxonomy

Use this only when it helps narrow diagnosis.

## Build/type/static failure

Investigate incompatible interfaces/contracts, missing dependencies, invalid configuration, stale generated artifacts, or newly exposed unsafe/unreachable patterns.

## Test assertion failure

Trace expected versus actual behavior to the product contract. Determine whether implementation, fixture, expectation, or environment is wrong.

## Runtime failure

Inspect input boundaries, state transitions, resource lifecycle, error propagation, and violated assumptions.

## Integration failure

Check contract mismatches between components before rewriting either side.

## Flaky/intermittent failure

Investigate nondeterminism, ordering assumptions, shared mutable state, timing, resource leakage, eventual consistency, races, and environment variance.

A single passing retry does not prove flakiness is resolved.

## Environment/tooling failure

Distinguish unavailable infrastructure/tooling from product defects. Do not mutate product behavior to mask an external environment problem unless tolerance is itself required.
