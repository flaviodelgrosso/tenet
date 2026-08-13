<div align="center">

# loops

<img
src="https://github.com/user-attachments/assets/471ab52d-866b-4a5e-bdd9-8f1ecaeb4940"
alt="loops logo"
width="320"
/>

<br />

[![CI](https://github.com/flaviodelgrosso/loops/actions/workflows/ci.yml/badge.svg)](https://github.com/flaviodelgrosso/loops/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-000000?logo=rust\&logoColor=white)](https://www.rust-lang.org/)
[![Status: MVP](https://img.shields.io/badge/status-MVP-orange)](#status-mvp)
[![Coding Agents](https://img.shields.io/badge/coding-agents-blueviolet)](#)

**A deterministic control loop for autonomous, spec-driven software development with coding agents.**

</div>

Give `loops` a spec and it will work toward satisfying it through isolated coding-agent workers, repository-backed state, deterministic verification, repair loops, and an independent final assessment.

The important distinction is this:

> **Agents do the work. The controller owns the process.**

<img width="1792" height="1094" alt="tui" src="https://github.com/user-attachments/assets/0484b206-170f-4c2c-b508-26f0ae77a60b" />

`loops` is a small Rust controller for autonomous, spec-driven development.

It does not rely on one long-running agent conversation to remember the project, decide what to do next, modify the code, judge its own work, and eventually declare itself finished.

Instead, it runs a state machine that repeatedly:

1. inspects the repository against an authoritative specification,
2. selects one coherent next unit of work,
3. gives that work to a fresh coding-agent worker,
4. runs real build, test, lint, and project verification commands,
5. repairs deterministic failures when possible,
6. reconciles the repository against the spec again,
7. and requires an independent assessment before considering the run complete.

The production runtime speaks the **[Agent Client Protocol (ACP)](https://agentclientprotocol.com/)**. Choose an agent through its canonical Registry identity, or use an advanced custom ACP command.

> **Bring your agent. Loops supplies the engineering process.**
> Workflow state, transitions, verification, retries, evidence, and completion rules are owned by the Rust controller rather than left entirely to the agent.

---

## Status: MVP

`loops` is currently an **MVP**.

The core control loop is implemented and usable, including:

- spec-to-requirement analysis,
- repository reconciliation,
- isolated implementation workers,
- deterministic verification,
- repair loops,
- persistent state and evidence,
- stagnation and cycle limits,
- independent final assessment,
- per-role model and thought-level configuration,
- TUI and non-interactive execution.

What has **not** been proven yet is more important:

`loops` does not yet have enough comparative evaluation to claim that this architecture is more reliable, cheaper, or more effective than strong single-agent workflows, Ralph-style loops, or other coding-agent orchestrators.

That is the main hypothesis this project now needs to test.

If you use `loops` today, treat it as experimental engineering tooling rather than a production-ready autonomous software factory.

---

## The idea in one picture

```mermaid
flowchart LR
    A["`authoritative
.loops/spec.md`"] --> B["`loops

state machine
requirement catalog
deterministic verify
evidence + audit trail
retries / circuit break
TUI`"]

    B --> C["`single ACP boundary`"]
    C --> D1["`Registry ACP agent`"]
    C --> D2["`another Registry ACP agent`"]
    C --> D3["`custom ACP command`"]
```

The diagram shows interchangeable ACP launch choices, not multiple agents running in one project at once. A project configures exactly one source; each role receives a fresh session through the same ACP boundary.

There is no permanent "master LLM" carrying the entire run in conversational memory.

State survives between workers through explicit artifacts:

- the repository,
- `.loops/spec.md`,
- `.loops/` controller state,
- the requirement catalog,
- work-unit summaries,
- reconciliation evidence,
- deterministic verification reports.

Each worker starts with fresh context.

That isolation is intentional.

The repository and controller state are durable. Agent context is disposable.

---

## The hypothesis

Long-running autonomous coding has a control problem as much as it has a model problem.

A single agent can be very effective at implementing a bounded task. Over longer runs, however, several failure modes become increasingly important:

- conversational context grows,
- assumptions become stale,
- earlier reasoning becomes hard to inspect,
- task plans diverge from repository reality,
- failed verification gets summarized instead of used directly,
- and the same agent that wrote the change is often asked to judge whether it succeeded.

`loops` experiments with a different structure.

### Disposable context, durable state

Architect, Reconcile, Implement, Repair, and Assess run in separate ACP sessions with fresh context.

Workers do not inherit a long conversational history from previous workers.

What survives is explicit state written to disk.

This does not guarantee better results, but it makes dependencies between iterations more visible and reduces reliance on hidden conversational memory.

### Agents propose; deterministic code controls transitions

An agent saying "tests pass" is not enough.

`loops` runs actual processes and checks actual exit codes.

Builds, tests, linters, typecheckers, acceptance scripts, and other configured commands provide deterministic evidence used by the controller.

Deterministic verification is still only as good as the checks available in the project. A green test suite does **not** prove that the specification has been implemented correctly.

The goal is narrower:

> never substitute an agent's claim for evidence that the controller can obtain directly.

### Reconcile against the repository

After a verified change, `loops` does not blindly advance to the next item in an old plan.

A fresh Reconcile worker inspects the current repository against the full requirement catalog and decides what remains.

The roadmap is treated as a current interpretation of the repository, not as immutable truth.

### Independent assessment at the finish line

The worker that implemented the code does not certify overall completion.

When deterministic gates pass and reconciliation reports no remaining work, a fresh, read-only Assess worker evaluates the repository against the requirement catalog.

Assess is an additional skeptical check, not an oracle.

It is still a model and can still be wrong.

Its purpose is to reduce self-certification by separating implementation context from final evaluation.

### Fail closed

Invalid structured output, protected-file mutation, repeated stagnation, exhausted repair attempts, or failed deterministic gates block the run rather than silently converting uncertainty into success.

---

## What actually happens

A `loops run` is a hierarchy of loops rather than a linear generated task list:

```text
.loops/spec.md
   │
   ▼
ARCHITECT                 when the requirement catalog is missing/stale
   │
   ▼
┌──────────────────── PROJECT LOOP ─────────────────────┐
│                                                      │
│  RECONCILE                                           │
│      │                                               │
│      ├── gaps ──► IMPLEMENT ──► VERIFY               │
│      │                            │                   │
│      │                            ├─ PASS ─────┐      │
│      │                            │            │      │
│      │                            └─ FAIL      │      │
│      │                                │        │      │
│      │                             REPAIR       │      │
│      │                                │        │      │
│      │                             VERIFY       │      │
│      │                                │        │      │
│      └────────────────────────────────┴────────┘      │
│                                                      │
│          next cycle → inspect the repo again          │
└──────────────────────────────────────────────────────┘
   │
   │ reconciliation reports complete
   ▼
FINAL DETERMINISTIC GATES
   │
   ├── fail ──► REPAIR ──► verify again
   │
   ▼
ASSESS
   │
   ├── gaps ──► PROJECT LOOP
   │
   ▼
DONE
```

---

## The five roles

| Role          | Purpose                                                                         | Repo access     | Runs when                                                      |
| ------------- | ------------------------------------------------------------------------------- | --------------- | -------------------------------------------------------------- |
| **Architect** | Converts `.loops/spec.md` into a stable `REQ-NNN` requirement catalog           | Read-only       | Initially and when the spec changes                            |
| **Reconcile** | Compares the repository against all requirements and selects the next work unit | Read-only       | At the beginning of every project cycle                        |
| **Implement** | Implements one bounded work unit                                                | Read/write/bash | Once per selected work unit                                    |
| **Repair**    | Responds to deterministic verification failures                                 | Read/write/bash | After verification fails                                       |
| **Assess**    | Independently evaluates final repository state against the requirements         | Read-only       | After reconciliation and deterministic gates report completion |

These are separate ACP sessions, not nested agent subagents.

Each worker receives a deliberately limited environment:

```text
fresh ACP worker
    + role prompt
    + built-in role procedure
    + relevant work-unit context
    + repository
```

Workers currently execute **sequentially**.

That is deliberate.

Parallel execution introduces coordination, merge, dependency, and verification problems that are not solved by simply spawning more agents. Parallelism may be added later where work units can be shown to be sufficiently independent.

---

## Worker procedures

`loops` creates an isolated per-worker procedure environment under:

```text
.loops/runtime/<run>/<role>/skills
```

Agent-native extensions and global configuration are not implicitly inherited.

`loops` ships role procedures rather than language-specific expertise:

| Role                 | Built-in procedure |
| -------------------- | ------------------ |
| Architect, Reconcile | `spec-analysis`    |
| Implement            | `implementation`   |
| Repair               | `debugging`        |
| Assess               | `spec-assessment`  |

A `code-review` procedure is also included for a possible future Review role, but Review is not currently part of the state machine.

A **role prompt** defines worker identity, permissions, protected files, and a strict output schema. When an ACP MCP server is actually attached, it can additionally offer the optional `loops_yield` tool.

A **built-in procedure** describes the workflow expected from that role.

A **work unit** describes the concrete task.

The repository provides current reality.

---
## Structured completion, not prose parsing

The portable completion baseline is one whole-response JSON value: no markdown or surrounding prose. `loops` validates that value against the role's schema and then deserializes it into the controller's typed result.

This strict JSON path is required for every worker and is used whenever MCP is unavailable or the agent does not call the completion tool. Invalid output is retried according to `completion_retries`; continued invalid output fails the worker.

When an ACP agent advertises client MCP support, Loops attaches a per-worker `loops_yield` server through the official ACP SDK. The tool accepts the same role schema, accepts exactly one valid result, and becomes the worker result; malformed, schema-invalid, or duplicate yields are rejected. Agents that do not support or use MCP—including adapters that ignore supplied MCP servers—continue through the portable strict-JSON path.

This does not make the model deterministic.

It makes the boundary between probabilistic model behavior and deterministic controller state explicit.

---

## Verification

`loops verify` and internal verification gates execute the configured project commands.

Additional hard gates can be configured:

```toml
[verification]
commands = [
  "cargo test --all-features",
  "./scripts/acceptance.sh"
]
```

Every attempt — successful or failed — is persisted under:

```text
.loops/evidence/
```

including information such as:

- command,
- exit code,
- stdout,
- stderr.

### Verification has limits

Deterministic verification is one of the strongest parts of the control loop, but it should not be confused with proof of correctness.

An agent can produce software that passes the existing tests while still violating the intended behavior.

A weak project test suite produces weak evidence.

For serious autonomous delivery, projects should provide strong acceptance gates that reflect product behavior rather than only implementation health.

Useful examples include:

- integration tests,
- acceptance tests,
- contract tests,
- browser or UI checks,
- security scanners,
- architectural constraints,
- performance thresholds,
- project-specific validation scripts.

Improving verification quality is one of the most important directions for the project.

---

## Repair

A deterministic verification failure does not become "one more message" in the Implement worker's existing conversation.

Instead, `loops` starts a fresh Repair worker.

That worker receives the current repository plus the actual verification failure.

For example:

```text
repository after implementation
        +
actual failing command
        +
stdout / stderr
        +
exit code
        ↓
fresh Repair worker
```

Repair attempt #2 does not need Repair attempt #1's reasoning.

It sees the repository as Repair #1 left it and the newest deterministic evidence.

Repair is bounded by `max_repair_attempts`.

When attempts are exhausted, the run blocks rather than retrying indefinitely.

---

## Guardrails against runaway loops

Autonomous execution needs explicit stopping conditions.

`loops` currently includes:

### Protected-file snapshotting

Controller-owned files such as:

- `.loops/spec.md`,
- `loops.toml`,
- `.loops/state.json`,
- and related state,

are protected around coding workers.

Unexpected modification can cause restoration and block the run.

This protects controller state from accidental mutation.

It is **not** a security sandbox.

### Stagnation detection

If Reconcile repeatedly proposes effectively the same work unit without requirement progress, a circuit breaker eventually trips.

Default:

```toml
stagnation_limit = 3
```

### Hard cycle ceiling

A project run cannot loop forever.

Default:

```toml
max_cycles = 25
```

### Repair ceiling

Repeated deterministic failures stop after a bounded number of repair attempts.

Default:

```toml
max_repair_attempts = 3
```

These limits exist to turn runaway behavior into an explicit blocked state instead of unlimited token and compute consumption.

---

## What `DONE` means

`DONE` is a controller state, not a sentence emitted by a coding agent.

Conceptually:

```text
status = done only when:

  1. a valid requirement catalog exists

  2. Reconcile reports every requirement satisfied
     and selects no next work unit

  3. final deterministic verification passes
     on the integrated repository

  4. a fresh read-only Assess worker independently
     reports no remaining requirement gaps

  5. configured Git cleanliness policy passes
```

> **Agent claims are hypotheses. Evidence determines controller transitions.**

Even then, `DONE` should be interpreted correctly:

it means the repository satisfied the evidence and assessment mechanisms currently available to `loops`.

It does not mean formal proof that the software is correct.

---

## TUI

`loops` owns the terminal while ACP workers execute headlessly in the background.

The Ratatui interface exposes three main views:

- **Run** — worker activity and requirement progress
- **Evidence** — requirement evidence and current gaps
- **Timeline** — state transitions, worker boundaries, and verification events

Keyboard controls:

```text
Tab                 switch view
Home / g            jump to top
Up / Down, j / k    scroll
End / G             follow live output
PageUp / PageDown   scroll faster
q / Ctrl-C          stop active run / exit when idle
```

When a run completes, the TUI remains available for inspection.

For CI and non-interactive environments:

```bash
loops run --no-tui
```

---

## Project state

Controller state is stored on disk and intended to remain human-inspectable.

```text
project/
├── loops.toml
└── .loops/
    ├── spec.md
    ├── state.json
    ├── requirements.json
    ├── roadmap.json
    ├── evidence/
    └── runs/<run-id>/
        ├── events.jsonl
        ├── worker-events.jsonl
        └── transcript.log
```

### `.loops/spec.md`

The human-authored product contract.

### `requirements.json`

The Architect worker's normalized requirement catalog, tied to the current spec.

### `roadmap.json`

The latest reconciliation or assessment view of requirement status, evidence, and gaps.

### `state.json`

Controller state such as:

- phase,
- cycle,
- current work unit,
- completed work units,
- blocked/done status.

### `evidence/`

Deterministic verification reports.

### `runs/`

Per-run event streams and transcripts for inspection and debugging.

`resume` is deliberately equivalent to `run`.

Continuation is reconstructed from repository and `.loops/` state rather than from a preserved conversational session.

---

## Getting started

### Requirements

Bring an ACP-compatible coding agent. `loops` supplies the control loop; it does not supply an agent account, credentials, or a coding-agent runtime.

You also need:

- a Rust toolchain to install `loops` from this checkout,
- your project's own build/test tooling,
- Git, strongly recommended.

### Install and run

Install Loops:

```bash
cargo install --path loops-cli
```

Initialize the project:

```bash
loops init
```

This creates:

```text
loops.toml
.loops/
  └── spec.md
```

Choose a Registry agent, or use the custom ACP command created by `loops init`.

To choose a Registry agent, remove the `[agent.custom]` block from `loops.toml`, then:

```bash
loops agents list
loops agents select <registry-agent-id>
```

Authenticate only if the selected agent asks you to; follow that agent's normal authentication flow. Loops neither collects those credentials nor performs sign-in on the agent's behalf.

Write the specification:

```bash
$EDITOR .loops/spec.md
```

Run:

```bash
loops run
```

Or without the TUI:

```bash
loops run --no-tui
```

Inspect controller state:

```bash
loops status --json
```

Run deterministic verification without invoking an agent:

```bash
loops verify --json
```

---

## Configuration

Example `loops.toml`:

```toml
#:schema https://raw.githubusercontent.com/flaviodelgrosso/loops/main/schemas/config.schema.json

version = 1
spec_file = ".loops/spec.md"
max_cycles = 25
max_repair_attempts = 3
stagnation_limit = 3

[agent]
turn_timeout_secs = 900
completion_retries = 2

# Registry source, written by `loops agents select <id>`:
id = "registry-agent-id"

[agent.preferences.default]
thought_level = "medium"

[agent.preferences.roles.architect]
thought_level = "xhigh"

# Instead of `agent.id`, an unregistered ACP command is also valid:
# [agent.custom]
# command = "omp"
# args = ["acp"]

[verification]
require_project_gate = true
commands = []
timeout_secs = 120
max_output_bytes = 65536

[git]
init = true
auto_commit = false
require_clean_tree = false
```

Registry selection is data-driven. `loops agents list` reads the Registry and caches its metadata; `loops agents select <id>` records the authoritative `agent.id`. At launch, Loops resolves that entry's declared distribution and launch arguments. It refreshes Registry metadata when it can, reuses a valid cached index when offline, and can fall back to a cached resolved launch when a refresh is unavailable.

Package distributions are resolved automatically. A Registry binary is different: `loops run` uses only a previously installed, checksum-verified binary. Installing one is an explicit, machine-changing action:

```bash
loops agents setup <registry-agent-id> --yes
```

The advanced `[agent.custom]` block launches any ACP-compatible process with ordered arguments and optional environment values. `omp acp` above is simply one unregistered custom-command example; it has no special integration. `agent.id` and `agent.custom` are mutually exclusive, so configure exactly one before `loops run`.

Generated `loops.toml` files include:

```toml
#:schema https://raw.githubusercontent.com/flaviodelgrosso/loops/main/schemas/config.schema.json
```

Editors and language servers with TOML/JSON Schema integration can use the repository-hosted schema for:

- completion,
- hover documentation,
- validation,
- value suggestions.

Editors without schema support treat the directive as a normal comment.

---

## Security boundary

Fresh ACP sessions provide **context isolation**, not a sandbox. The selected agent owns its authentication flow, credentials, and any agent-level permission or sandbox policy; Loops does not collect credentials or make authentication or security decisions for it.

ACP is an interoperability protocol, not a security boundary for arbitrary local tool execution. Process isolation, filesystem permissions, containers, OS sandboxes, and network controls are separate controls provided by the environment in which you run the agent.

Implement and Repair workers can receive real shell access. Run autonomous workloads in an environment whose blast radius you accept, such as an isolated container, VM, restricted development environment, dedicated worktree, or minimally privileged account.

Protected-file snapshotting protects controller integrity from accidental changes; it is not a replacement for real sandboxing.

---

## MVP boundaries

The current implementation intentionally leaves several problems unsolved.

Not implemented yet:

- parallel or fleet execution of independent work units,
- branch/worktree isolation per coding worker,
- cost and token accounting,
- alternative non-ACP protocols,
- OS-level sandbox management,
- remote worker execution,
- interactive steering of an in-flight worker,
- full production-grade policy and permission isolation.

Some of these may turn out not to belong in `loops` itself.

For example, sandboxing may be better handled by an execution environment around `loops` rather than deeply embedded into the orchestrator.


---

## What still needs to be proven

This is currently the most important section of the project.

The architecture is implemented.

Its claimed advantages are **not yet sufficiently measured**.

The project needs comparative evaluation against simpler approaches.

A useful benchmark should hold the underlying model and task set as constant and compare approaches such as:

```text
A. single coding-agent session

B. simple retry / Ralph-style loop

Across real software tasks.

Useful metrics include:

- requirement completion rate,
- hidden acceptance-test pass rate,
- deterministic gate pass rate,
- false-completion rate,
- regressions introduced,
- number of repair attempts,
- recovery rate after initial failure,
- human interventions,
- total tokens,
- cost per successful task,
- wall-clock time,
- behavior as task duration increases.

The most important question is not:

> Did an agent generate a plausible implementation?

It is:

> **Did the resulting repository independently satisfy the intended requirements without human correction?**

Until there is meaningful evidence here, statements that `loops` is more reliable, cheaper, or more effective than simpler agent workflows should be treated as hypotheses.

---

## Design principles

1. **The spec is authoritative.**
   Completing a generated task is not equivalent to satisfying the product requirement.

2. **Context is disposable.**
   Durable state should live in inspectable artifacts rather than depend entirely on an ever-growing conversation.

3. **Repository reality beats stale plans.**
   Reconcile again after verified changes instead of assuming the original task decomposition remains correct.

4. **LLMs propose; deterministic code controls transitions.**
   Models can reason and act without owning the workflow state machine.

5. **Use real failure evidence.**
   When verification fails, give Repair the actual failure rather than asking it to reconstruct one from a summary.

6. **Separate implementation from final assessment.**
   The worker that changed the repository should not be the only mechanism deciding whether the project is complete.

7. **Fail closed.**
   Invalid output, deterministic failures, unexpected controller-state mutation, stagnation, and exhausted retries should produce explicit failure states.

8. **Keep the controller understandable.**
   More agents and more orchestration are not automatically better.

9. **Measure before claiming superiority.**
   Architectural intuition is not a substitute for comparative evaluation.

---

## Why Rust?

The orchestration layer is deliberately boring.

It needs to:

- own state transitions,
- spawn and supervise processes,
- validate structured outputs,
- run deterministic commands,
- persist evidence,
- enforce bounded retries,
- restore protected state,
- and make failure conditions explicit.

Rust is a good fit for that kind of controller.

The intelligence remains in the workers.

The orchestration layer should aim to be predictable.

---

## Contributing

Contributions are genuinely welcome.

`loops` is early enough that challenging its assumptions is at least as valuable as adding features.

Areas where contributions would be particularly useful:

- evaluation harnesses,
- comparative benchmarks,
- adversarial test cases,
- hidden acceptance-test strategies,
- sandboxing approaches,
- worktree/container isolation,
- additional deterministic verification,
- real-world repository case studies,
- ACP conformance coverage,
- cost/token telemetry,
- stagnation and recovery analysis,
- documentation corrections,
- negative results.

If an architectural assumption looks wrong, open an issue.

If you find a case where a simpler loop consistently beats `loops`, document it.

If an agent can trick the completion mechanism, reproduce it.

If a requirement can be marked satisfied while the product is clearly wrong, that is valuable information.

The goal is not to defend the architecture.

The goal is to find out where it actually works.

---

## The bet

`loops` is intentionally small.

The hypothesis is not that adding more agents automatically produces better software.

It is that a deterministic controller around fresh, narrowly scoped coding agents can make long-running autonomous development more:

- observable,
- inspectable,
- recoverable,
- bounded,
- and verifiable.

Whether that structure also makes autonomous delivery **more reliable, more cost-effective, or more successful than simpler coding-agent loops remains to be demonstrated empirically.**

That is what this project is here to test.
