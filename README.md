<div align="center">

<img width="1530" height="689" alt="logo" src="https://github.com/user-attachments/assets/4c078e90-69f5-44d9-a634-91eefbc4470f" />

<br />

[![CI](https://github.com/flaviodelgrosso/tenet/actions/workflows/ci.yml/badge.svg)](https://github.com/flaviodelgrosso/tenet/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Status: MVP](https://img.shields.io/badge/status-MVP-orange)](#mvp-read-this-first)

### Give an agent a task and it can write code

### Give **Tenet** a spec and it keeps asking whether the software is actually done

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

**Tenet is an experiment in moving that responsibility out of the conversation.**

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

Tenet starts from a simple observation:

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

Tenet puts them in a Rust controller.

```text
probabilistic workers
        │
        ▼
┌──────────────────────────────┐
│            TENET             │
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

Tenet deliberately uses fresh sessions for different roles.

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

Whether that architecture ultimately beats simpler agent workflows is something that still needs to be measured.

Tenet does **not** claim that result today.

---

## What happens during a run

A run is a sequence of bounded engineering cycles.

### 1. Architect

The specification is turned into explicit requirements.

Not a vague todo list.

A concrete catalog of what the repository is supposed to satisfy.

### 2. Reconcile

A fresh worker compares those requirements with the repository **as it exists now**.

It proposes the work that remains.

Tenet validates that work before execution.

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

An agent saying _“tests pass”_ is not evidence when Tenet can run the tests itself.

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

Tenet is designed to block rather than spend forever rediscovering the same failure.

### 6. Integrate

Verified candidate work is integrated into the repository through the controller.

Then the repository is examined again.

Because after code changes, yesterday's plan is only a hypothesis.

### 7. Assess

When reconciliation believes the work is complete and deterministic gates pass, another fresh worker performs an independent assessment.

The worker that wrote the implementation does not get the final vote by default.

### Controller-enforced repository invariants

Architect, Reconcile, and Assess run in disposable detached worktrees at the exact canonical revision they inspect. Their worktrees are always discarded. Tenet also compares canonical `HEAD` and canonical status before and after each read-only worker; any unexpected change fails the run. ACP permission requests from these roles are rejected rather than automatically granted.

Implement and Repair run in leased detached worktrees. Before Tenet accepts a candidate, every added, modified, deleted, renamed, generated, mode-changed, or symlink path must match the current work unit's declared scope. A worker can request wider authority only by returning a `scope_expansion` discovery for a later reconciliation; the current out-of-scope candidate is rejected.

Candidates are committed before verification. Suggested checks and configured project gates run in disposable worktrees at that immutable commit, so relative command effects cannot alter the commit that is integrated. Protected paths are compared as repository objects, including recursive directories, file contents, executable mode, symlink targets, additions, and deletions. Configured protected paths must be normalized repository-relative paths.

Current Reconcile output is authoritative for scheduling. Historical completed-work records are evidence/context only and never suppress a work unit emitted by the current reconciliation. A specification hash change replaces the catalog context and invalidates completion and discovery history from the previous catalog.

Implement and Repair discoveries are persisted with catalog hash, repository revision, work-unit ID, role, cycle, deterministic fingerprint, and lifecycle status. Active discoveries are supplied to the next reconciliation once, then marked consumed; deterministic duplicates are not accumulated indefinitely.

Canonical advancement uses a durable integration journal. Verification evidence and a `prepared` transaction are persisted before fast-forward. Cancellation is checked during verification and immediately before canonical advancement. After Git advances, the journal moves through `git_committed` and `state_committed`; startup reconciles an incomplete journal against actual `HEAD`, recovers when `HEAD` is the intended revision, abandons a prepared transaction when `HEAD` is still old, and fails closed for any third revision.

Persisted `State` is the observable projection used by the CLI/TUI. The integration journal is the recovery-critical transaction record. Serialized leases and candidates describe the active attempt; they are not treated as sufficient recovery proof. State loading rejects impossible terminal/phase combinations and idle states with active work.

The cancellation boundary is the canonical fast-forward: cancellation observed before that operation prevents advancement. If the atomic Git operation completed before cancellation became observable, journal recovery records the completed advancement deterministically.


---

## `DONE` is a state, not a sentence

This is one of the most important ideas in Tenet.

A coding model can always emit:

```text
Everything is implemented successfully.
```

Tenet does not treat that sentence as completion.

Conceptually, `DONE(R)` requires all of the following for one exact canonical revision `R`:

```text
valid authoritative specification and catalog
        +
current reconciliation: every requirement satisfied, with nonblank evidence
        +
current reconciliation: no remaining work
        +
clean canonical revision R and no stale history suppressing work
        +
deterministic verification of an immutable checkout of R passes
        +
fresh Assess inspects a disposable checkout of R
        +
Assess: every requirement satisfied with evidence, no gaps, no work
        +
canonical HEAD is still R
        +
no active lease, candidate, or integration transaction
        +
required workspace cleanup, evidence logging, and state persistence succeed
        =
DONE(R)
```

Even then, `DONE` does **not** mean mathematically proven correct.

It means:

> the repository satisfied the evidence and assessment mechanisms available to this run.

If your tests are weak, your evidence is weak.

If your specification is ambiguous, the result can still be wrong.

If the model misses something, assessment can still be wrong.

Tenet cannot remove uncertainty from software engineering.

The goal is to make that uncertainty **visible, bounded, and harder to hand-wave away**.

---

# MVP: read this first

**Tenet is an MVP.**

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

## What Tenet has **not** proven

There is not yet enough comparative evaluation to claim that Tenet is:

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

If you try Tenet today, expect bugs.

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

Tenet asks:

> **What happens if autonomous coding starts looking more like an engineering system and less like one very long chat?**

If the answer is “nothing, simpler tenet work just as well,” that is useful to learn.

If the answer is that explicit control substantially reduces false completion, regressions, drift, or wasted retries, that is much more interesting.

The project exists to find out.

---

## Agent-neutral by design

Tenet communicates with coding agents through the **[Agent Client Protocol (ACP)](https://agentclientprotocol.com/)**.

The controller should not need to become a different product every time a better coding model appears.

Bring an ACP-compatible agent.

Tenet provides the surrounding process.

```text
                         ┌───────────────────┐
                         │       Tenet       │
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

Different Tenet roles can then use fresh sessions and role-specific model preferences while preserving the same controller semantics.

---

# Quick start

## 1. Build Tenet

Tenet is currently a Rust project distributed from source.

```bash
git clone https://github.com/flaviodelgrosso/tenet.git
cd tenet

make install
```

Or work directly from the repository during development.

A current stable Rust toolchain is required.

---

## 2. Initialize a project

From the repository you want Tenet to work on:

```bash
tenet init
```

This creates the project configuration and Tenet state directory.

Tenet runs require an existing Git repository with at least one commit and a clean canonical working tree. Worker changes execute in isolated worktrees and are integrated back into that canonical checkout.

---

## 3. Write the specification

Tell Tenet what should be true when the work is finished.

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

The spec path is configurable in `tenet.toml`.

---

## 4. Configure an agent

Explore ACP Registry agents:

```bash
tenet agents list
tenet agents search <query>
tenet agents select <id>
```

Check the configured runtime:

```bash
tenet agents doctor
```

Tenet also supports an advanced custom ACP command in `tenet.toml`.

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

The stronger your verification, the stronger the evidence Tenet can use.

A perfectly orchestrated agent cannot compensate for a test suite that proves nothing.

---

## 6. Run

```bash
tenet run
```

For non-interactive execution:

```bash
tenet run --headless
```

For less output:

```bash
tenet run --headless --quiet
```

For more worker diagnostics:

```bash
tenet run --headless --verbose
```

State is persisted, so a later invocation can reconcile the current repository again:

```bash
tenet resume
```

Resume does not trust serialized in-flight leases or candidates as recovery proof. Recovery-critical canonical advancement is handled separately by the integration journal described above.

---

# The terminal UI

By default Tenet provides an interactive terminal interface for watching the controller rather than staring at an opaque agent stream.

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
tenet run --headless > tenet.log
```

---

# Configuration

A project is controlled through `tenet.toml`.

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
tenet init
```

Initialize Tenet in a project.

```bash
tenet run
tenet resume
```

Start or continue autonomous development.

```bash
tenet status
tenet status --json
```

Inspect persisted controller state.

```bash
tenet verify
tenet verify --json
```

Run the configured deterministic verification **without invoking an LLM**.

```bash
tenet agents list
tenet agents search <query>
tenet agents select <id>
tenet agents setup
tenet agents doctor
tenet agents login
```

Discover, configure, install, inspect, and authenticate ACP agents.

---

# What Tenet is not

Tenet is **not**:

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

### Proven better than simpler tenet

Not yet.

That claim should be earned with data.

### Production-ready

Also not yet.

---

# The project principles

A few rules shape Tenet.

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

# Where Tenet needs to go

The most valuable next steps are not more impressive demos.

They are better evidence.

Important directions include:

**Evaluation.**
Run the same tasks through single-agent, simple-loop, and Tenet workflows. Measure completion, false-DONE rate, regressions, cost, wall time, and variance.

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

Tenet is early enough that experiments are unusually valuable.

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

If a simpler architecture consistently beats Tenet, that is something this project should discover rather than hide.

See [CONTRIBUTING.md](CONTRIBUTING.md).

---

# A note on trust

Autonomous development tools should be judged by what they can demonstrate, not by how confidently they describe themselves.

Tenet is built around that idea.

The project should be held to the same standard.

Right now there is an interesting architecture, a functioning MVP, and a lot left to prove.

That is enough.

---

<div align="center">

## Agents write the code

## **Tenet makes them prove the work.**

If that idea is useful to you, try it, break it, measure it, and help make the loop harder to fool.

[Report a bug](https://github.com/flaviodelgrosso/tenet/issues) · [Contribute](CONTRIBUTING.md) · [MIT License](LICENSE)

</div>
