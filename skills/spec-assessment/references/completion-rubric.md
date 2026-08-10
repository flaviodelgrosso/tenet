# Completion evidence rubric

Use this rubric during final acceptance.

## Verified

A requirement is `verified` when current evidence proves the specified behavior, including explicitly required error/boundary semantics.

Strong evidence often combines implementation inspection with focused executable tests or deterministic checks.

## Insufficient evidence

Use when implementation may exist but proof is too weak, indirect, stale, or does not exercise the required path.

## Not satisfied

Use when behavior is absent, incomplete, stubbed, contradicted by tests/runtime behavior, or incompatible with the authoritative requirement.

## Blocked

Use when required external infrastructure/input/environment prevents establishing completion.

## Ambiguous

Use when the authoritative specification genuinely does not determine required externally meaningful behavior.

Do not interpret ambiguity in whichever way makes the project complete.

## Evidence challenge questions

- What exact artifact implements this requirement?
- What exact evidence proves the required behavior?
- Does the test actually exercise the intended path?
- Are explicit failure/edge cases covered?
- Could this be a stub that still passes current tests?
- Has recent work invalidated the evidence?
- Does the evidence match the authoritative wording rather than a weakened paraphrase?
