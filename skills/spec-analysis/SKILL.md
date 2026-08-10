---
name: spec-analysis
description: Analyze an authoritative product specification against a repository, build requirement-to-evidence traceability, identify missing or partial behavior, and select the next minimal coherent work unit. Use for Loops Architect and Reconcile workers whenever requirements must be extracted or re-evaluated against current code.
---

# Spec Analysis

Analyze product intent against repository reality. Produce evidence-backed requirement state and, when work remains, one minimal coherent next work unit.

## Role boundaries

- Operate read-only.
- Treat the authoritative specification as the source of product intent.
- Treat the current repository and deterministic results as implementation evidence.
- Treat previous plans, task status, summaries, and agent claims as derived state, never proof.
- Do not implement fixes.
- Do not expand product scope with preferences or imagined future requirements.

## Workflow

### 1. Read the complete specification

Identify:

- explicit functional requirements;
- required failure and boundary behavior;
- constraints and compatibility requirements;
- acceptance gates;
- non-goals;
- ambiguities that materially change observable behavior.

Separate normative requirements from examples and explanation.

### 2. Normalize requirements

Represent each independently meaningful requirement with:

- stable identifier;
- concise title;
- required behavior/property;
- relevant constraints;
- evidence that would demonstrate satisfaction.

Split only when parts can be implemented or verified independently. Do not fragment prose into trivial requirements.

### 3. Inspect before judging

Map the smallest relevant repository surface:

- entry points;
- implementations;
- interfaces/contracts;
- tests;
- configuration/build setup;
- documentation only when it is part of the requirement.

Search first, then read targeted files. Follow actual call/data paths when needed.

### 4. Trace each requirement to evidence

Classify every requirement as:

- `satisfied`;
- `partial`;
- `missing`;
- `blocked`;
- `ambiguous`.

Require concrete evidence for `satisfied`. A green suite, matching filename, existing function, or previous `complete=true` is not sufficient by itself.

Read `skill://spec-analysis/references/evidence.md` when evaluating evidence quality or when completion is uncertain.

### 5. Select the next gap

When unresolved work exists, prioritize:

1. blockers before dependent features;
2. correctness before polish;
3. explicit acceptance requirements before inferred improvements;
4. the smallest coherent slice that can be independently verified.

Do not preserve stale roadmap items merely because they were planned earlier.

### 6. Shape one work unit

Unless explicitly asked for a full roadmap, return exactly one next work unit containing:

- `id`;
- `title`;
- `objective`;
- `requirementIds`;
- observable `acceptanceCriteria`;
- `suggestedChecks` when useful.

Describe the outcome, not a prescriptive patch, unless implementation technique is itself specified.

Read `skill://spec-analysis/references/work-units.md` when choosing granularity or resolving competing next steps.

## Reconciliation behavior

On every reconciliation:

- inspect current code again;
- prefer current evidence over implementer narrative;
- incorporate deterministic failures as negative evidence;
- invalidate stale evidence after material changes;
- allow newly discovered gaps to replace old plans;
- do not mark a requirement complete because its work unit was completed.

## Ambiguity rule

Do not block on ordinary implementation freedom that can be resolved by repository conventions and minimal design.

Mark `ambiguous` only when plausible interpretations materially change observable behavior, compatibility, security, data semantics, or acceptance.

When ambiguity matters, report:

- exact ambiguous requirement;
- plausible interpretations;
- why the difference matters;
- what information would resolve it.

## Output discipline

Requirement evidence must explain **what proves the behavior**, not merely where related code exists.

When no work remains, report completion only if every requirement has sufficient current evidence and required deterministic gates are compatible with completion.

## Never do these

- Substitute task completion for requirement satisfaction.
- Substitute test names for behavioral evidence.
- Invent missing requirements.
- Create micro-tasks with no independently useful outcome.
- Create mega-work-units combining unrelated behavior.
- Trust previous agent confidence as evidence.

## Final principle

**Specification defines intent. Repository defines reality. Evidence connects the two.**
