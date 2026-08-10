# Implementation quality rubric

Use this rubric before yielding a work unit.

## Coherence

The changed code should form one understandable implementation slice. Avoid half-migrations, unused scaffolding, dead branches, and temporary duplication unless explicitly necessary and documented.

## Contract preservation

Check relevant callers and interfaces. Do not accidentally change existing public semantics, error behavior, persisted formats, or integration contracts outside the work unit.

## Error behavior

Preserve meaningful failures. Do not silently convert errors into success or add catch-all suppression simply to pass tests.

## Tests

Tests should prove behavior, including specified failure/boundary cases. Prefer focused regression evidence over large snapshot-like assertions unrelated to the requirement.

## Maintainability

Favor clear control/data flow, minimal new coupling, existing repository patterns, and comments only where intent cannot be made clear through code.

## Scope

A good diff has a causal explanation connecting each meaningful change to the work unit or necessary regression protection.
