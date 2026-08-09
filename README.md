# loops

`loops` is a small autonomous **spec-driven development controller** written in Rust.

You give it a repository containing an authoritative `spec.md`. `loops` turns that specification into a requirement catalog, repeatedly inspects the repository, chooses one coherent work unit, delegates coding to a fresh headless coding-agent worker, runs deterministic verification, repairs failures, and independently reassesses the result until the specification is satisfied or the run is blocked.

The default backend is **Oh My Pi (OMP)** running headlessly through `omp --mode rpc`.

> `loops` is the orchestrator. OMP is the coding-agent engine.

---

## Mental model

The most important thing to understand is that `loops` is **not one long agent conversation**.

A run is controlled by a deterministic Rust state machine. Whenever reasoning or coding is needed, the controller starts a brand-new OMP process for a specific role, gives that worker only the context needed for that step, waits for structured output, then terminates the worker.

```text
                           authoritative
                              spec.md
                                 │
                                 ▼
                    ┌─────────────────────────┐
                    │          loops          │
                    │                         │
                    │ state machine           │
                    │ requirements            │
                    │ verification            │
                    │ evidence                │
                    │ retries / circuit break │
                    │ TUI                     │
                    └────────────┬────────────┘
                                 │
                      fresh OMP processes
                                 │
          ┌──────────┬───────────┼───────────┬──────────┐
          ▼          ▼           ▼           ▼          ▼
      Architect   Reconcile   Implement    Repair     Assess
      read-only   read-only    coding      coding    read-only
```

There is no permanent "master LLM" carrying the whole run in its context window.

State survives between workers through explicit artifacts: the repository, `spec.md`, `.loops/` state, work-unit summaries, and deterministic verification evidence.

---

## Are there subagents?

**Logically yes; technically they are isolated workers, not nested OMP subagents.**

`loops` currently defines five agent roles:

| Role          | Purpose                                                                          | Repository access | When it runs                                                              |
| ------------- | -------------------------------------------------------------------------------- | ----------------- | ------------------------------------------------------------------------- |
| **Architect** | Convert `spec.md` into a stable requirement catalog                              | Read-only         | Initially, and again if `spec.md` changes                                 |
| **Reconcile** | Compare the real repository with every requirement and choose the next work unit | Read-only         | At the beginning of every cycle                                           |
| **Implement** | Implement exactly one work unit                                                  | Read/write/bash   | Once for each selected work unit                                          |
| **Repair**    | Fix a deterministic verification failure                                         | Read/write/bash   | Only after verification fails                                             |
| **Assess**    | Independently verify final completion from scratch                               | Read-only         | Only after reconciliation and final gates say the project may be complete |

These workers are **not** spawned from another LLM conversation. The Rust controller starts separate commands similar to:

```bash
omp --mode rpc --no-session ...
```

Each worker therefore starts with a fresh context.

Workers currently run **sequentially**, not in parallel.

---

## Does every worker use the same model?

**Yes, in v0.1.0 the model configuration is global.**

There is one setting:

```toml
[agent]
model = "openrouter/anthropic/claude-sonnet-4.6"
```

When `model` is configured, every OMP worker is launched with the same:

```bash
--model <configured-model>
```

So Architect, Reconcile, Implement, Repair, and Assess use the **same model**, but they do **not** share context. They receive different:

- system-role instructions;
- tool permissions;
- task input;
- structured output schema.

If `agent.model` is omitted, `loops` does not pass `--model`; each fresh OMP process resolves the normal model configured for OMP.

The thinking level is also currently global:

```toml
[agent]
thinking = "high"
```

### Why start with one model?

The MVP deliberately keeps model selection simple so that failures are easier to reason about. If Architect behaves differently from Implement, the difference is caused primarily by role, context and tools rather than by an additional model variable.

A future version can add per-role model routing, for example a stronger model for Architect/Assess and a cheaper model for implementation, but **that is not implemented today**.

---

# Exact execution flow

A normal `loops run` is a hierarchy of loops rather than a fixed linear script.

At a high level:

```text
spec.md
   │
   ▼
ARCHITECT                 only when requirement catalog is missing/stale
   │
   ▼
┌──────────────────── PROJECT LOOP ────────────────────┐
│                                                     │
│  RECONCILE                                          │
│      │                                              │
│      ├── gaps ──► IMPLEMENT ──► VERIFY              │
│      │                              │               │
│      │                              ├─ PASS ─────┐  │
│      │                              │            │  │
│      │                              └─ FAIL      │  │
│      │                                  │        │  │
│      │                               REPAIR      │  │
│      │                                  │        │  │
│      │                               VERIFY      │  │
│      │                                  │        │  │
│      └──────────────────────────────────┴────────┘  │
│                                                     │
│          next cycle → RECONCILE repository again   │
└─────────────────────────────────────────────────────┘
   │
   │ reconciliation says complete
   ▼
FINAL DETERMINISTIC GATES
   │
   ├── fail ──► REPAIR ──► verify again
   │
   ▼
ASSESS                     fresh independent worker
   │
   ├── gaps ──────────────► PROJECT LOOP again
   │
   ▼
DONE
```

The detailed flow is below.

---

## Step 0 — Initialize controller state

Before starting agent work, `loops`:

1. creates `.loops/` if necessary;
2. creates/loads `.loops/config.toml`;
3. ensures `spec.md` exists;
4. ensures `.loops/` is ignored by Git;
5. optionally initializes Git according to config;
6. acquires the run lock so two controllers cannot mutate the same project simultaneously;
7. creates a unique run id;
8. creates run logs under `.loops/runs/<run-id>/`;
9. loads the persisted controller state.

The LLM is not involved in this step.

---

## Step 1 — Architect the specification

The Architect exists to turn prose in `spec.md` into a machine-trackable catalog.

It receives the complete product specification and must return requirements such as:

```json
{
  "requirements": [
    {
      "id": "REQ-001",
      "title": "Rust CLI application",
      "description": "...",
      "acceptanceCriteria": [
        "The project builds with Cargo",
        "The produced executable is named linekit"
      ]
    }
  ]
}
```

The controller enforces stable sequential IDs:

```text
REQ-001
REQ-002
REQ-003
...
```

The Architect is **read-only**. It is not supposed to design implementation tasks or change the codebase.

Its output is stored in:

```text
.loops/requirements.json
```

The catalog also contains a hash of `spec.md`.

### Does Architect run every cycle?

No.

If `.loops/requirements.json` exists and its spec hash still matches the current `spec.md`, the existing catalog is reused.

If `spec.md` changes during development, `loops` detects the new hash, invalidates completed work-unit claims, and runs a fresh Architect again.

---

## Step 2 — Reconcile the repository

Every project cycle starts with a fresh **Reconcile** worker.

The reconciler receives:

- the complete requirement catalog;
- up to the five most recent completed work-unit summaries;
- read-only access to the actual repository.

The completed-work-unit summaries are explicitly treated as **claims, not evidence**. The reconciler is instructed to inspect source code, tests and configuration itself.

For every requirement it returns:

```text
satisfied
partial
missing
```

plus concrete evidence and remaining gaps.

Example:

```json
{
  "id": "REQ-004",
  "status": "partial",
  "evidence": ["src/main.rs defines the stats command"],
  "gaps": ["Unicode characters are still counted as bytes"]
}
```

If work remains, Reconcile must choose **exactly one next work unit**:

```json
{
  "id": "WU-004",
  "title": "Complete Unicode-aware stats",
  "objective": "Make character counting conform to REQ-004 and add tests",
  "requirementIds": ["REQ-004", "REQ-006"],
  "acceptanceCriteria": ["..."],
  "suggestedChecks": ["cargo test stats"]
}
```

The controller validates that the work unit only references known requirements.

The latest reconciliation is persisted as:

```text
.loops/roadmap.json
```

### Why reconcile after every work unit?

Because the task list is not the source of truth. The **repository and the product spec are**.

After each change, a new context inspects the result again. This lets the controller discover:

- an implementation that did not fully satisfy its target requirement;
- side effects that satisfied other requirements;
- new gaps exposed by the implementation;
- incorrect completion claims from a previous worker.

---

## Step 3 — Implement one work unit

If Reconcile finds gaps, `loops` starts a fresh **Implement** worker.

The Implement worker receives:

- exactly one `WorkUnit`;
- the full requirement catalog;
- coding tools.

It does **not** receive the previous reconciler conversation.

Its role contract tells it to make the smallest coherent production-quality change needed for that work unit and to add/update tests when behavior changes.

The default coding tool surface is:

```text
read
grep
find
ls
edit
write
bash
```

Before the worker starts, `loops` snapshots controller-owned files such as:

```text
spec.md
AGENTS.md
.loops/config.toml
.loops/state.json
.loops/requirements.json
.loops/roadmap.json
.loops/run.lock
```

After the worker exits, `loops` checks those files again.

If the coding worker changed a protected file, the controller restores it and **blocks the run**.

The Implement worker returns a structured summary containing changed files, tests it ran, notes and a short summary. That summary does not make the work unit complete; deterministic verification does.

---

## Step 4 — Run deterministic verification

After implementation, the controller runs verification **itself**, without an LLM.

This separation is intentional:

```text
LLM says "tests pass"       → not trusted
process exit code == 0      → evidence
```

Verification is fail-fast. As soon as one command fails, the remaining commands for that attempt are not executed.

Auto-detection currently supports common project gates.

### Rust

```bash
cargo build
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

### Go

```bash
go test ./...
```

### Python

When Python project markers and `tests/` exist:

```bash
python -m pytest -q
```

### Node

For scripts present in `package.json`, `loops` detects:

```text
test
typecheck
lint
build
```

and respects pnpm/yarn/bun/npm lockfiles.

### Git

Inside a Git repository:

```bash
git diff --check
```

`git diff --check` alone is intentionally not sufficient when `require_project_gate = true`.

You can add project-specific hard gates:

```toml
[verification]
commands = [
  "cargo test --all-features",
  "./scripts/acceptance.sh"
]
```

Every verification attempt is saved as evidence under `.loops/evidence/`.

---

## Step 5 — Repair failed verification

If a deterministic gate fails, the controller does **not** send another message to the Implement worker.

It creates a completely fresh **Repair** worker.

Repair receives:

- the same work unit;
- the full requirement catalog;
- the exact deterministic verification report, including command, exit code, stdout and stderr;
- coding tools.

Its goal is to repair the root cause without weakening the verification system.

Then the controller runs deterministic verification again.

The loop is:

```text
VERIFY
  │
  ├─ pass → continue project loop
  │
  └─ fail
       │
       ▼
     REPAIR #1      fresh OMP context
       │
       ▼
     VERIFY
       │
       ├─ pass → continue
       └─ fail
            │
            ▼
          REPAIR #2 fresh OMP context
            │
            ▼
          VERIFY
            ...
```

Maximum repair attempts are configured with:

```toml
max_repair_attempts = 3
```

Every repair attempt is a new OMP process. Repair #2 does not inherit Repair #1's chat transcript; it sees the repository as Repair #1 left it plus the latest deterministic failure report.

If verification is still failing after the configured attempts, the run becomes blocked.

---

## Step 6 — Record completed work-unit evidence

When verification passes, `loops`:

1. saves the verification report as evidence;
2. records the work unit in `state.completed_work_units`;
3. clears the current work unit;
4. optionally creates a Git commit if `git.auto_commit = true`;
5. starts another project cycle.

Critically, the next operation is **not another implementation prompt**.

It is another fresh Reconcile pass over the actual repository.

---

## Step 7 — Detect stagnation

An autonomous loop can get stuck repeatedly proposing the same work.

`loops` therefore tracks the selected work-unit fingerprint and the number of satisfied requirements.

If the controller repeatedly sees effectively the same next work unit without increasing requirement completion, a stagnation counter grows.

When it reaches:

```toml
stagnation_limit = 3
```

`loops` trips a circuit breaker and blocks the run instead of burning tokens forever.

There is also a hard project-loop limit:

```toml
max_cycles = 25
```

---

## Step 8 — Candidate completion

Eventually a Reconcile worker may report:

```text
all requirements = satisfied
next work unit    = none
complete          = true
```

That still does **not** mean the run is done.

This only moves the controller into the final completion gates.

---

## Step 9 — Final deterministic gates

`loops` executes the project verification suite again over the final repository.

This catches cases where individual work units passed earlier but the integrated project later regressed.

If a final gate fails, `loops` can run the same fresh Repair loop against a synthetic final work unit covering the whole requirement catalog.

Only after the final deterministic gates pass does the controller move to semantic completion assessment.

---

## Step 10 — Independent final assessment

A fresh **Assess** worker is launched.

The assessor gets:

- the complete requirement catalog;
- read-only repository tools.

It does **not** receive:

- Implement conversations;
- Repair conversations;
- Reconcile reasoning;
- a trusted statement that the project is complete.

Its instruction is explicitly skeptical: inspect the repository from scratch and provide evidence for every requirement.

The Assess output uses the same shape as Reconcile:

```text
requirement status + evidence + gaps
complete
optional next work unit
```

If Assess finds a gap, the project is **not** completed. The controller returns to the project loop and lets a later Reconcile select new work.

If Assess confirms every requirement, the final completion contract can succeed.

---

## Step 11 — DONE

`DONE` is controlled by Rust code, not by an agent saying "done".

The run can complete only when all of these conditions hold:

1. the requirement catalog is valid;
2. Reconcile marks every requirement satisfied;
3. Reconcile produces no next work unit;
4. final deterministic verification passes;
5. the independent Assess worker also marks every requirement satisfied;
6. Assess produces no next work unit;
7. optional Git cleanliness policy passes.

Then state becomes:

```text
status = done
phase  = complete
```

This is the central completion rule of `loops`:

> agent claims are hypotheses; repository evidence and deterministic gates decide completion.

---

# Fresh-context behavior

Every agent role starts a command approximately like:

```bash
omp \
  --mode rpc \
  --no-session \
  --thinking high \
  --tools <role-specific-tools> \
  --no-extensions \
  --no-skills \
  --no-rules \
  --append-system-prompt '<role contract>' \
  --yolo
```

If a model is configured, `loops` also adds:

```bash
--model <model>
```

The process is terminated after that worker produces its structured result.

This means:

```text
Reconcile context  ─X─► Implement context
Implement context  ─X─► Repair context
Repair context     ─X─► Assess context
```

The contexts do not flow into each other.

What _does_ flow between stages is explicit state:

```text
filesystem / Git
spec.md
requirement catalog
selected work unit
verification report
completed work-unit metadata
```

This is deliberate loop engineering: keep conversational memory disposable and durable state inspectable.

---

# Structured worker output: `loops_yield`

`loops` does not ask the model to print a JSON code block and then try to scrape it from prose.

For every worker process the controller registers an OMP RPC **host-owned tool** named:

```text
loops_yield
```

The tool has a role-specific JSON Schema.

Examples:

- Architect must yield an `ArchitectOutput`;
- Reconcile/Assess must yield a `ReconcileResult`;
- Implement/Repair must yield a `WorkerSummary`.

The normal worker lifecycle is therefore:

```text
loops starts OMP
     │
     ├─ installs loops_yield(schema)
     │
     ├─ sends role prompt
     │
     ├─ streams LLM text/tool events to the TUI
     │
     └─ worker calls loops_yield({...})
                    │
                    ▼
          Rust deserializes + validates
                    │
                    ▼
                 accepted
```

If the output does not match the schema, the tool call is rejected.

If the model ends its turn without calling `loops_yield`, `loops` sends up to two structured-completion reminders. If it still does not yield a valid result, that worker fails.

---

# OMP RPC and the TUI

The TUI belongs to `loops`; OMP runs headlessly in child processes.

`loops` listens to OMP RPC events including:

```text
message_update
tool_execution_start
tool_execution_end
agent_end
host_tool_call
```

LLM text deltas and coding-tool events are forwarded to the Ratatui interface and to persistent run logs.

The worker does not know about the TUI and the TUI does not become part of the worker's context.

The full-screen interface currently exposes three views:

- **Run** — live worker text/tool activity and requirement progress;
- **Evidence** — current requirement evidence and gaps;
- **Timeline** — state-machine transitions, worker boundaries and verification events.

Keys:

```text
Tab                 switch view
Up/Down, j/k        scroll
PageUp/PageDown     scroll faster
Home/g              top
End/G               follow live output
q or Ctrl-C         stop an active run
Enter/Esc/q         close a completed run
```

For CI or plain logging:

```bash
loops run --no-tui
```

---

# Persistent project state

`loops init` creates:

```text
project/
├── spec.md
└── .loops/
    ├── config.toml
    ├── state.json
    ├── requirements.json
    ├── roadmap.json
    ├── evidence/
    └── runs/
        └── <run-id>/
            ├── events.jsonl
            ├── worker-events.jsonl
            └── transcript.log
```

### `spec.md`

Human-authored product contract and authoritative source of scope.

### `requirements.json`

Architect-derived stable requirement catalog, keyed to the hash of `spec.md`.

### `roadmap.json`

The most recent Reconcile/Assess view of requirement status, evidence, gaps and next work unit.

### `state.json`

Controller state including status, phase, cycle, current work unit and completed work units.

### `evidence/`

Deterministic verification reports for work units and repair attempts.

### `runs/<run-id>/`

Audit trail for one invocation, including controller events, worker events and readable transcript.

`resume` is intentionally an alias for `run`: durable state is reconstructed from `.loops/` and the repository rather than from a preserved LLM conversation.

---

# Configuration

`.loops/config.toml` is created by `loops init`.

Default shape:

```toml
version = 1
spec_file = "spec.md"
max_cycles = 25
max_repair_attempts = 3
stagnation_limit = 3

[agent]
command = "omp"
thinking = "high"
auto_approve = true
turn_timeout_secs = 900
read_only_tools = ["read", "grep", "find", "ls"]
coding_tools = ["read", "grep", "find", "ls", "edit", "write", "bash"]
extra_args = []

[verification]
auto_detect = true
require_project_gate = true
commands = []
timeout_secs = 120
max_output_bytes = 65536

[git]
init = true
auto_commit = false
require_clean_tree = false
```

To pin the model used by **all** worker roles:

```toml
[agent]
model = "openrouter/anthropic/claude-sonnet-4.6"
```

If omitted, OMP chooses its normal configured/default model independently in each fresh process.

---

# Install

Requirements:

- Rust toolchain to build/install `loops`;
- `omp` installed and authenticated/configured for at least one model;
- the target project's own build/test tooling;
- Git recommended.

From this repository:

```bash
cargo install --path .
```

Verify:

```bash
loops --version
omp --version
```

---

# Safety and isolation

Fresh processes provide **context/process isolation**, not a security sandbox.

Implement and Repair workers deliberately receive shell access. With the default:

```toml
auto_approve = true
```

OMP is launched with `--yolo` so the autonomous headless process does not wait for interactive approval.

Therefore:

- do not treat prompts as a filesystem/network security boundary;
- do not run autonomous coding on an environment containing credentials or resources the worker must not access;
- use a container, VM, restricted worktree or other OS-level sandbox when stronger isolation is required.

Protected-file snapshot/restore prevents accidental controller-state mutation; it does **not** sandbox arbitrary shell commands.

---

# Current MVP boundaries

v0.1.0 intentionally does **not** implement:

- parallel/fleet workers;
- nested OMP subagent orchestration;
- per-role model selection;
- branch/worktree isolation;
- cost/token accounting;
- multiple coding-agent backends beyond OMP;
- OS sandboxing;
- interactive steering of an active worker;
- remote execution.

The controller talks through an `AgentBackend` trait, so another backend can be added later without rewriting the state machine, verification engine or TUI.

A likely next evolution is per-role agent configuration, for example:

```text
Architect / Assess  → strongest reasoning model
Reconcile           → strong reasoning model
Implement / Repair  → faster coding model
```

but the current implementation intentionally uses one global model configuration.

---

# Design principles

`loops` is built around a few rules:

1. **The spec is authoritative.** Task completion is not product completion.
2. **Contexts are disposable.** Durable memory belongs in files and evidence, not an endlessly growing chat.
3. **One coherent work unit at a time.** Reconcile again after every verified change.
4. **LLMs propose; deterministic code controls transitions.**
5. **Verification output is repair input.** Do not ask a model to guess why a test failed when the real stderr exists.
6. **Completion needs independent evidence.** A fresh assessor gets the last word before the deterministic controller marks the run done.
7. **Fail closed.** Invalid structured output, protected-file mutations, stagnation and exhausted repairs block the run rather than silently proceeding.
