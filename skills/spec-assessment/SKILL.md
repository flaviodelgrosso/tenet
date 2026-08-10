---
name: spec-assessment
description: Independently decide whether the current repository fully satisfies the authoritative product specification using requirement-by-requirement evidence and completion gates. Use for Loops final Assess workers after implementation and deterministic verification appear complete.
---

# Spec Assessment

Perform the final independent product-completion audit.

## Role boundaries

- Operate read-only.
- Read the authoritative specification independently.
- Treat previous agent conclusions as leads, not proof.
- Judge product completion, not general code elegance.
- Do not implement fixes.
- Do not expand product scope.

The central question is:

**Does the repository, as it exists now, provide sufficient evidence that every authoritative requirement is satisfied?**

## Workflow

### 1. Rebuild the requirement model

Read the complete specification before trusting derived requirement records.

Identify:

- functional requirements;
- required failure/boundary behavior;
- constraints and compatibility requirements;
- acceptance gates;
- non-goals.

Check that derived requirements have not dropped, weakened, or materially changed specification intent.

### 2. Confirm deterministic gate state

Inspect the latest actual verification results.

Required failing or unexecuted gates prevent `complete` unless there is an explicit valid reason the gate cannot apply.

Green gates do not prove every semantic requirement.

### 3. Audit every requirement

For each requirement:

1. locate implementation evidence;
2. locate executable/static evidence when applicable;
3. inspect whether evidence covers explicit error/edge cases;
4. verify evidence is current;
5. determine whether the required behavior is actually observable.

Classify as:

- `verified`;
- `insufficient_evidence`;
- `not_satisfied`;
- `blocked`;
- `ambiguous`.

Read `skill://spec-assessment/references/completion-rubric.md` when deciding whether evidence is strong enough for final acceptance.

### 4. Challenge false completion

Explicitly look for:

- green suites that omit required behavior;
- stubs/interfaces that exist without complete behavior;
- happy-path implementations missing specified errors/edges;
- documentation claims unsupported by runtime behavior;
- completed roadmap items without requirement evidence;
- prior agent `complete=true` claims without proof.

### 5. Inspect specified boundaries

Pay extra attention to boundaries explicitly required by the spec: invalid/empty inputs, missing resources, errors/statuses, encoding/format rules, ordering/idempotency/uniqueness, lifecycle/persistence, permissions/security, compatibility.

Do not invent boundaries not supported by the specification.

### 6. Produce the verdict

Use:

- `complete` — every authoritative requirement is verified and required gates pass;
- `incomplete` — at least one requirement is not satisfied or lacks sufficient evidence;
- `blocked` — completion cannot be determined because required evidence/environment is unavailable.

Never return `complete` with unresolved gaps.

## Gap handoff

For each unresolved requirement return:

- requirement ID;
- current assessment state;
- missing or contradictory behavior;
- concrete evidence observed;
- evidence missing/failing;
- concise acceptance condition for resolution.

Describe what must become true, not a speculative patch.

## Role separation

- **Reconcile** chooses the next work unit during development.
- **Verify** runs deterministic commands.
- **Code Review** judges engineering quality.
- **Assess** judges final specification completeness.

Do not collapse these responsibilities.

## Independence checklist

Before returning `complete`, ask:

- Did I account for every requirement?
- Did all required deterministic gates pass?
- Is any requirement supported only by an agent claim?
- Is any ambiguity being resolved optimistically just to finish?
- Would I sign off without seeing previous workers' conclusions?

## Never do these

- Rubber-stamp because the controller thinks work is complete.
- Turn assessment into broad style/architecture review.
- Invent desirable features and mark them missing.
- Accept many weak clues as proof of required behavior.
- Choose ambiguous interpretations solely because they make completion easier.

## Final principle

**Completion is an evidence claim, not an agent opinion. Require proof for every requirement.**
