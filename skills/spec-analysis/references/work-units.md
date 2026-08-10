# Work-unit shaping

A Loops work unit is the smallest coherent implementation slice worth giving to a fresh coding worker.

## Good work units

A good work unit:

- advances one or more related unresolved requirements;
- has a single coherent objective;
- can be independently verified;
- contains observable acceptance criteria;
- fits the current repository architecture;
- avoids speculative future design.

## Too small

Avoid tasks such as:

- rename one variable;
- create an empty file;
- add a dependency without the behavior using it;
- write a test disconnected from implementation.

These create orchestration overhead without meaningful product progress.

## Too large

Split work when a unit combines unrelated behaviors that can fail or be accepted independently.

Do not split merely because several files are involved.

## Dependency ordering

Prefer work that unlocks later requirements. If two gaps are independent, prefer the one with clearer acceptance evidence or greater product leverage.

## Acceptance criteria

Write acceptance criteria in observable terms. Prefer:

- behavior;
- externally visible state;
- deterministic checks;
- explicit error semantics.

Avoid prescribing internal classes/functions/modules unless the spec requires them.
