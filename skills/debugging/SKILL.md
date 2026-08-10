---
name: debugging
description: Diagnose and repair a concrete verification failure using evidence-driven root-cause analysis, minimal corrective changes, and focused re-verification. Use for Loops Repair workers after tests, builds, static checks, or other deterministic gates fail.
---

# Debugging

Repair the concrete failure with the smallest correct root-cause fix.

## Role boundaries

- Start from actual failure evidence.
- Treat previous agent explanations as hypotheses, not facts.
- Preserve product requirements and test strength.
- Do not restart implementation from scratch unless evidence proves the current approach is structurally wrong.
- Do not modify the authoritative specification or Loops controller state.

## Workflow

### 1. Parse the failure

Extract:

- failing command/check;
- failing test/diagnostic identifier;
- observed versus expected behavior;
- relevant location/trace;
- whether the failure appears deterministic, intermittent, environmental, or unknown.

Keep exact discriminating details.

### 2. Reproduce narrowly

When possible, rerun the smallest check that reproduces the failure before editing.

Prefer one failing test, module, command, or minimal invocation over the full suite during diagnosis.

If reproduction is impossible, preserve the original evidence and state why.

### 3. Trace cause, not just location

Ask:

- What assumption was violated?
- Where was the bad state/value first introduced?
- Is this implementation, integration, verification, environment, or flakiness?
- Could several failures share one cause?

Read `skill://debugging/references/failure-taxonomy.md` when classification is unclear.

### 4. Form discriminating hypotheses

When root cause is not obvious:

- form a small ranked set of concrete hypotheses;
- identify a cheap observation/check that distinguishes them;
- gather evidence before editing.

Avoid random changes and repeated retries.

### 5. Apply the minimal root-cause fix

Prefer fixing the violated contract at its source.

Avoid:

- broad rewrites;
- globally suppressing warnings/errors;
- swallowing failures;
- increasing timeouts without evidence;
- disabling validation/security/checks;
- deleting tests;
- weakening assertions to match broken behavior;
- hard-coding only the observed failing fixture.

### 6. Verify in concentric rings

After the fix:

1. rerun the narrow reproducer;
2. rerun the original failing gate;
3. run nearby regression checks likely affected;
4. let Loops run broader deterministic verification.

### 7. Inspect collateral effects

Confirm:

- the fix is causally connected to the failure;
- unrelated behavior did not change;
- the patch is general enough for the requirement;
- no protected artifact was changed;
- new failure paths remain coherent.

## Test-change rule

Change a failing test only when authoritative behavior proves the test itself is wrong.

Before changing it, establish:

1. what behavior it claims;
2. what authoritative requirement defines that behavior;
3. why the expectation/fixture is incorrect;
4. that the revised test still catches the relevant defect class.

Otherwise fix the implementation.

## Stagnation rule

If the same failure persists:

- compare evidence with the previous attempt;
- do not repeat the same fix cosmetically;
- challenge the root-cause hypothesis;
- inspect one layer earlier in the control/data path;
- surface a blocker when required evidence/infrastructure is unavailable.

If repairs create several new unrelated failures, simplify or revert the broad change rather than stacking patches.

## Handoff

Report:

- failure evidence;
- root cause;
- fix;
- focused verification actually executed;
- discoveries;
- unresolved risks;
- out-of-scope follow-ups.

Do not report success when the original deterministic failure remains unresolved.

## Never do these

- Shotgun debugging.
- Symptom suppression.
- Test capitulation.
- Retry-as-fix.
- Fixture-specific overfitting.
- Rewrite escalation without evidence.

## Final principle

**Reproduce, localize, explain, repair, and prove. Evidence should get stronger at every step.**
