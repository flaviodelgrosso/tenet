# Evidence rubric

Use this rubric when deciding whether a requirement is actually satisfied.

## Evidence strength

Prefer, in order:

1. deterministic executable evidence directly exercising the requirement;
2. implementation plus focused tests exercising the required behavior;
3. implementation plus static/tool evidence proving the relevant property;
4. implementation inspection when execution is impractical;
5. documentation/comments only when documentation is the requirement.

Multiple weak clues do not automatically equal one strong proof.

## Satisfied

Use `satisfied` only when:

- required behavior exists;
- required edge/failure behavior exists when specified;
- evidence is current;
- evidence actually exercises/proves the intended path;
- no known contradictory evidence exists.

## Partial

Use `partial` when meaningful implementation exists but any required behavior, integration, edge case, or evidence is incomplete.

## Missing

Use `missing` when the required behavior is absent, stubbed, disconnected, or contradicted by current implementation.

## Blocked

Use `blocked` when external infrastructure, unavailable input, or a structural dependency prevents establishing or implementing the requirement.

## Ambiguous

Use `ambiguous` only when the authoritative spec does not determine externally meaningful behavior.

## Challenge questions

Before accepting satisfaction ask:

- Could this test pass without exercising the required behavior?
- Could the implementation be a stub or happy-path-only version?
- Does the evidence cover explicit failure/boundary cases?
- Did a recent change make the evidence stale?
- Am I relying on a previous agent's claim rather than repository evidence?
- Would a fresh assessor reach the same conclusion from the cited artifacts/results?
