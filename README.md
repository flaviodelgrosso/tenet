<div align="center">

<img width="2066" height="761" alt="loops" src="https://github.com/user-attachments/assets/83d2961d-be43-4e8e-a68c-5f4958cafdbb" />

<br />

[![CI](https://github.com/flaviodelgrosso/loops/actions/workflows/ci.yml/badge.svg)](https://github.com/flaviodelgrosso/loops/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Status: MVP](https://img.shields.io/badge/status-MVP-orange)](#mvp-read-this-first)

### Give an agent a task and it can write code

### Give **Loops** a spec and it keeps asking whether the software is actually done

**A deterministic control loop for autonomous, spec-driven software development.**

</div>

---

Coding agents are getting very good at changing code.

That is not the same thing as being good at **finishing software autonomously**.

Long-running agent workflows have a different problem:

- context accumulates;
- assumptions become stale;
- plans drift away from the repository;
- failed tests become another paragraph in a conversation;
- the agent that wrote the code is often the same agent deciding whether it worked;
- and eventually something says _“done”_.

**Loops is an experiment in moving that responsibility out of the conversation.**

The model writes code.

**The controller owns the process.**

```text
spec
 │
 ▼
understand what must be true
 │
 ▼
inspect the repository
 │
 ▼
choose the next bounded work
 │
 ▼
fresh coding-agent session
 │
 ▼
isolated Git worktree
 │
 ▼
run real verification
 │
 ├── failed ──► repair with actual failure evidence ──┐
 │                                                  │
 └── passed ──► integrate ──► inspect again ◄───────┘
                              │
                              ▼
                     independent assessment
                              │
                              ▼
                            DONE?
```

There is no immortal master conversation that has to remember everything.

Agent context is disposable.

**The repository, specification, evidence, and controller state are not.**

---

## The idea

Loops starts from a simple observation:

> **Autonomous coding is partly an intelligence problem, but it is also a control problem.**

A strong model can already solve surprisingly difficult bounded coding tasks.

But an autonomous engineering system also needs to answer questions like:

- What are we actually trying to satisfy?
- What is already implemented?
- What should happen next?
- Which work can safely run?
- Did the change really pass the project's checks?
- Did the fix break something that was already working?
- Are we repeating ourselves?
- Should we retry, repair, stop, or reassess?
- Who decides that the repository is finished?

Those decisions should not live only inside model context.

Loops puts them in a Rust controller.

```text
probabilistic workers
        │
        ▼
┌──────────────────────────────┐
│            LOOPS             │
│                              │
│ requirements                 │
│ state transitions            │
│ dependency validation        │
│ worktree isolation           │
│ verification                 │
│ retries                      │
│ integration                  │
│ evidence                     │
│ stopping conditions          │
└──────────────────────────────┘
        │
        ▼
      repository
```

Agents reason.

Agents implement.

Agents repair.

Agents assess.

But **agents do not get to redefine the process just because the conversation drifted there**.

---

## Why fresh agents?

A long-running coding conversation has memory.

That sounds like an advantage.

Sometimes it is.

It can also become baggage.

An agent may carry forward:

- an obsolete mental model of the codebase;
- a plan that made sense three changes ago;
- assumptions it never re-checked;
- compressed summaries of previous failures;
- confidence inherited from its own earlier reasoning.

Loops deliberately uses fresh sessions for different roles.

The next worker does not need to believe the previous worker.

It can inspect what actually exists.

```text
previous reasoning    → disposable
repository changes    → durable
verification results  → durable
requirements          → durable
controller state      → durable
```

This is the central bet behind the project:

> **Keep engineering state explicit. Keep model context replaceable.**

Whether that architecture ultimately beats simpler agent loops is something that still needs to be measured.

Loops does **not** claim that result today.

---

## What happens during a run

A run is a sequence of bounded engineering loops.

### 1. Architect

The specification is turned into explicit requirements.

Not a vague todo list.

A concrete catalog of what the repository is supposed to satisfy.

### 2. Reconcile

A fresh worker compares those requirements with the repository **as it exists now**.

It proposes the work that remains.

Loops validates that work before execution.

The plan is therefore allowed to change as the code changes.

### 3. Implement

A coding worker receives a bounded unit of work.

Implementation happens inside an isolated Git worktree rather than treating the canonical checkout as scratch space.

### 4. Verify

The controller executes real project commands.

```bash
cargo test
make ci
./scripts/acceptance.sh
```

Whatever your project considers meaningful evidence.

An agent saying _“tests pass”_ is not evidence when Loops can run the tests itself.

### 5. Repair

If deterministic verification fails, the failure becomes input to a fresh repair attempt:

```text
changed repository
+
failing command
+
exit code
+
stdout / stderr
        │
        ▼
   repair worker
```

Retries are bounded.

Loops is designed to block rather than spend forever rediscovering the same failure.

### 6. Integrate

Verified candidate work is integrated into the repository through the controller.

Then the repository is examined again.

Because after code changes, yesterday's plan is only a hypothesis.

### 7. Assess

When reconciliation believes the work is complete and deterministic gates pass, another fresh worker performs an independent assessment.

The worker that wrote the implementation does not get the final vote by default.

---

## `DONE` is a state, not a sentence

This is one of the most important ideas in Loops.

A coding model can always emit:

```text
Everything is implemented successfully.
```

Loops does not treat that sentence as completion.

Conceptually, completion looks more like:

```text
requirements accounted for
        +
no remaining reconciled work
        +
deterministic verification passes
        +
independent assessment finds no remaining gap
        +
controller policies pass
        =
DONE
```

Even then, `DONE` does **not** mean mathematically proven correct.

It means:

> the repository satisfied the evidence and assessment mechanisms available to this run.

If your tests are weak, your evidence is weak.

If your specification is ambiguous, the result can still be wrong.

If the model misses something, assessment can still be wrong.

Loops cannot remove uncertainty from software engineering.

The goal is to make that uncertainty **visible, bounded, and harder to hand-wave away**.

---

# MVP: read this first

**Loops is an MVP.**

It is real software and the core control loop exists.

It is also young, experimental software that is changing quickly.

Today, the project already contains the foundations for:

- spec-driven requirement analysis;
- repository reconciliation;
- separate Architect, Reconcile, Implement, Repair, and Assess sessions;
- ACP-based coding-agent execution;
- isolated Git worktrees for coding workers;
- deterministic project verification;
- bounded repair attempts;
- bounded project cycles;
- persistent controller state;
- evidence capture;
- dependency-aware scheduling;
- controlled integration;
- protected controller-owned paths;
- role-specific model configuration;
- interactive terminal UI;
- headless execution for automation and logs.

That is the promising part.

Here is the equally important part.

## What Loops has **not** proven

There is not yet enough comparative evaluation to claim that Loops is:

- more reliable than a strong single-agent workflow;
- better than a simple Ralph-style loop;
- cheaper;
- faster;
- safer for unattended production development;
- or less likely to produce false completion.

Those are hypotheses.

They need benchmarks.

They need failed runs.

They need ugly repositories.

They need adversarial specs.

They need real evidence.

That is where this project should earn its claims.

## Expect rough edges

If you try Loops today, expect bugs.

Particularly around the places where real systems get messy:

- different ACP agents and their capabilities;
- model/configuration compatibility;
- unusual Git repository states;
- worktree lifecycle edge cases;
- terminal rendering and streaming output;
- cancellation and recovery;
- incomplete or misleading verification suites;
- agent output that violates the expected structure;
- workflows nobody has tested yet.

Do not point it at an important repository, walk away, and assume autonomous perfection.

Use Git.

Use backups.

Review the result.

Give it strong acceptance tests.

And when it breaks, open an issue.

**The current product is an engineering experiment, not an autonomous software factory.**

That distinction matters.

---

## Why this could become interesting

The interesting future for coding agents is probably not just:

```text
bigger model
+
bigger context window
+
longer conversation
```

It may also require better machinery around the model.

Software engineering already has machinery:

- version control;
- CI;
- tests;
- dependency graphs;
- transaction boundaries;
- schedulers;
- logs;
- state machines;
- review;
- rollback;
- observability.

Loops asks:

> **What happens if autonomous coding starts looking more like an engineering system and less like one very long chat?**

If the answer is “nothing, simpler loops work just as well,” that is useful to learn.

If the answer is that explicit control substantially reduces false completion, regressions, drift, or wasted retries, that is much more interesting.

The project exists to find out.

---

## Agent-neutral by design

Loops communicates with coding agents through the **[Agent Client Protocol (ACP)](https://agentclientprotocol.com/)**.

The controller should not need to become a different product every time a better coding model appears.

Bring an ACP-compatible agent.

Loops provides the surrounding process.

```text
                         ┌───────────────────┐
                         │       Loops       │
                         │                   │
spec ───────────────────►│ controller        │
repo ───────────────────►│ verification      │
                         │ state + evidence  │
                         └─────────┬─────────┘
                                   │
                                  ACP
                                   │
                 ┌─────────────────┼─────────────────┐
                 ▼                 ▼                 ▼
              Agent A           Agent B        custom ACP
```

A project selects an agent source.

Different Loops roles can then use fresh sessions and role-specific model preferences while preserving the same controller semantics.

---

# Quick start

## 1. Build Loops

Loops is currently a Rust project distributed from source.

```bash
git clone https://github.com/flaviodelgrosso/loops.git
cd loops

make install
```

Or work directly from the repository during development.

A current stable Rust toolchain is required.

---

## 2. Initialize a project

From the repository you want Loops to work on:

```bash
loops init
```

This creates the project configuration and Loops state directory.

Loops runs require an existing Git repository with at least one commit and a clean canonical working tree. Worker changes execute in isolated worktrees and are integrated back into that canonical checkout.

---

## 3. Write the specification

Tell Loops what should be true when the work is finished.

For example:

```markdown
# Product specification

Build a JSON API for bookmarks.

## Requirements

- Users can create a bookmark with a URL and title.
- URLs must be unique.
- Users can list bookmarks ordered by creation time.
- Users can delete bookmarks.
- Invalid URLs return a 400 response.
- The API must have integration tests.
- `cargo test` must pass.
```

The spec is not a prompt asking an agent to “please implement this.”

It is the reference against which the repository keeps being reconsidered.

The spec path is configurable in `loops.toml`.

---

## 4. Configure an agent

Explore ACP Registry agents:

```bash
loops agents list
loops agents search <query>
loops agents select <id>
```

Check the configured runtime:

```bash
loops agents doctor
```

Loops also supports an advanced custom ACP command in `loops.toml`.

---

## 5. Configure real verification

This part matters more than almost anything else.

```toml
[verification]
require_project_gate = true
commands = [
  "cargo test",
  "./scripts/acceptance.sh",
]
```

The stronger your verification, the stronger the evidence Loops can use.

A perfectly orchestrated agent cannot compensate for a test suite that proves nothing.

---

## 6. Run

```bash
loops run
```

For non-interactive execution:

```bash
loops run --headless
```

For less output:

```bash
loops run --headless --quiet
```

For more worker diagnostics:

```bash
loops run --headless --verbose
```

State is persisted, so execution can be resumed:

```bash
loops resume
```

---

# The terminal UI

By default Loops provides an interactive terminal interface for watching the controller rather than staring at an opaque agent stream.

The important views are about **process**, not just tokens:

- current worker activity;
- requirement progress;
- evidence;
- verification;
- state transitions;
- timeline.

Useful controls:

```text
Tab                 switch view
Home / g            top
Up / Down, j / k    scroll
End / G             follow
PageUp / PageDown   faster scroll
q / Ctrl-C          stop / exit
```

For servers, CI, SSH sessions, or log capture, use headless mode instead.

```bash
loops run --headless > loops.log
```

---

# Configuration

A project is controlled through `loops.toml`.

A simplified example:

```toml
version = 1
spec_file = "spec.md"

max_cycles = 25
max_repair_attempts = 3
stagnation_limit = 3

[agent]
completion_retries = 2
turn_timeout_secs = 900

[verification]
require_project_gate = true
commands = ["make ci"]
timeout_secs = 120

[execution]
max_parallel_workers = 1

[integration]
strategy = "cherry_pick"
verify_each_candidate = true
```

These limits are intentional.

An autonomous system needs a way to say:

```text
this is not making progress
```

instead of converting more tokens into more confidence.

---

## Useful commands

```bash
loops init
```

Initialize Loops in a project.

```bash
loops run
loops resume
```

Start or continue autonomous development.

```bash
loops status
loops status --json
```

Inspect persisted controller state.

```bash
loops verify
loops verify --json
```

Run the configured deterministic verification **without invoking an LLM**.

```bash
loops agents list
loops agents search <query>
loops agents select <id>
loops agents setup
loops agents doctor
loops agents login
```

Discover, configure, install, inspect, and authenticate ACP agents.

---

# What Loops is not

Loops is **not**:

### A coding model

It does not compete with coding agents.

It orchestrates them.

### A magic correctness machine

Passing tests can still mean shipping the wrong thing.

### A security sandbox

Git worktree isolation protects workflow boundaries.

It does not make arbitrary model-generated commands safe.

### A replacement for specifications

If the desired behavior is unclear, the controller cannot manufacture product truth.

### Proven better than simpler loops

Not yet.

That claim should be earned with data.

### Production-ready

Also not yet.

---

# The project principles

A few rules shape Loops.

### The repository outranks the conversation

If a previous worker believed something that the repository contradicts, inspect the repository again.

### Agent claims are hypotheses

If the controller can obtain deterministic evidence, obtain it.

### Context should be cheap to throw away

Important engineering state should not exist only in hidden conversational memory.

### Planning is provisional

A roadmap should change when repository reality changes.

### Failure should become structured input

A failing command, exit code, stdout, and stderr are more useful than “something went wrong.”

### Completion should be difficult to fake

The implementation worker should not be able to end the entire process by confidently declaring success.

### Autonomous execution needs stopping conditions

Infinite persistence is not intelligence.

Sometimes the correct controller action is:

```text
BLOCKED
```

---

# Where Loops needs to go

The most valuable next steps are not more impressive demos.

They are better evidence.

Important directions include:

**Evaluation.**
Run the same tasks through single-agent, simple-loop, and Loops workflows. Measure completion, false-DONE rate, regressions, cost, wall time, and variance.

**Replayability.**
Make complete autonomous runs inspectable as structured episodes rather than terminal archaeology.

**Requirement evidence.**
Make it obvious why each requirement is considered satisfied and what deterministic evidence supports that conclusion.

**Regression obligations.**
Do not allow later work to quietly invalidate earlier progress.

**Better failure routing.**
A code defect, broken environment, flaky test, and misunderstood requirement should not all produce the same retry behavior.

**Safer concurrency.**
Parallel agents are useful only when independence is established and integration remains controlled.

**Engineering memory.**
Preserve useful repository knowledge without recreating one giant permanent conversation.

**CI and pull-request workflows.**
Turn a run into an evidence-backed merge-readiness report.

The long-term goal is not “more agents.”

It is a better autonomous engineering process.

---

# Contributing

Loops is early enough that experiments are unusually valuable.

Useful contributions include:

- bug reports with reproducible runs;
- weird repositories that break assumptions;
- ACP compatibility fixes;
- verification improvements;
- failure cases;
- benchmark tasks;
- benchmark infrastructure;
- run observability;
- Git/worktree edge cases;
- controller correctness;
- documentation;
- negative results.

Especially negative results.

If a simpler architecture consistently beats Loops, that is something this project should discover rather than hide.

See [CONTRIBUTING.md](CONTRIBUTING.md).

---

# A note on trust

Autonomous development tools should be judged by what they can demonstrate, not by how confidently they describe themselves.

Loops is built around that idea.

The project should be held to the same standard.

Right now there is an interesting architecture, a functioning MVP, and a lot left to prove.

That is enough.

---

<div align="center">

## Agents write the code

## **Loops makes them prove the work.**

If that idea is useful to you, try it, break it, measure it, and help make the loop harder to fool.

[Report a bug](https://github.com/flaviodelgrosso/loops/issues) · [Contribute](CONTRIBUTING.md) · [MIT License](LICENSE)

</div>
