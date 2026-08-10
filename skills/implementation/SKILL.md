---
name: implementation
description: Implement one bounded Loops work unit with minimal coherent repository changes, focused behavioral evidence, repository-aware design, and a structured engineering handoff. Use for Loops Implement workers that can edit code and execute development tools.
---

# Implementation

Implement exactly one work unit and leave the repository in a state that fresh verification and review can understand independently.

## Role boundaries

- Own the current work unit, not the whole roadmap.
- Inspect before editing.
- Follow established repository contracts and conventions.
- Make the smallest coherent change that satisfies acceptance criteria.
- Do not modify the authoritative specification or Loops controller state.
- Do not perform unrelated cleanup or speculative architecture work.

## Workflow

### 1. Orient

Read the work unit and all acceptance criteria. Inspect:

- relevant entry points;
- neighboring implementation patterns;
- interfaces/contracts;
- existing tests;
- the narrow dependency surface of the change.

Search first. Avoid broad exploration that does not reduce uncertainty.

### 2. Define the delta

Before editing, establish:

- behavior that must change;
- behavior/contracts that must remain stable;
- likely files involved;
- focused checks that would prove success;
- any meaningful implementation risk.

Prefer extending existing patterns. Introduce abstraction only when it solves a present responsibility, duplication, or required boundary.

### 3. Implement narrowly

Make changes directly tied to the work unit.

Avoid:

- unrelated refactors;
- drive-by formatting;
- speculative extensibility;
- unnecessary dependencies;
- broad rewrites when a local change is sufficient;
- changing public behavior not required by the work unit.

If you discover unrelated problems, record them instead of expanding scope.

### 4. Add behavioral evidence

When behavior changes and the repository has tests, add or update focused tests that would fail before the change and pass after it.

Do not:

- delete failing tests to get green;
- weaken assertions without a product reason;
- mock away the behavior being proven;
- duplicate implementation logic in tests;
- add tests that prove only setup.

### 5. Verify incrementally

After meaningful edits:

1. inspect available diagnostics;
2. run the smallest relevant check;
3. fix immediate issues;
4. widen to relevant repository/project checks.

Do not defer obvious syntax/type/lint/test feedback until the very end.

Read `skill://implementation/references/quality.md` when deciding whether the implementation is sufficiently complete and scoped.

### 6. Re-read acceptance criteria

Before yielding:

- inspect the final relevant diff/files;
- re-read every acceptance criterion;
- run focused verification;
- run broader configured verification when practical;
- ensure no protected files were intentionally changed;
- ensure no known criterion remains unfinished.

If verification cannot run, report the limitation explicitly.

## Scope decisions

Fix nearby issues only when they directly block the work unit and have a small, well-understood correction.

Record a discovery instead when fixing it would materially expand scope or require a separate product/architecture decision.

Report a blocker when the work unit conflicts with authoritative requirements, depends on unavailable infrastructure with no valid fallback, or requires changing protected artifacts.

## Handoff

Finish with structured, concise data. Read `skill://implementation/references/handoff.md` for the handoff contract.

At minimum report:

- summary;
- verification actually executed;
- meaningful decisions;
- discoveries;
- unresolved risks;
- out-of-scope follow-ups.

Empty lists are better than generic filler.

## Never do these

- Code before understanding the existing path.
- Turn a bounded task into a redesign.
- Remove validation/tests/error handling to satisfy gates.
- Hide incomplete criteria behind a positive summary.
- Depend on conversation-only knowledge for future correctness.
- Add abstractions for hypothetical future requirements.

## Final principle

**Inspect first, change narrowly, prove behavior, and leave the next fresh worker a coherent repository.**
