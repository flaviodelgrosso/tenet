<div align="center">

<img width="1530" height="689" alt="Tenet" src="https://github.com/user-attachments/assets/4c078e90-69f5-44d9-a634-91eefbc4470f" />

<br />

[![CI](https://github.com/flaviodelgrosso/tenet/actions/workflows/ci.yml/badge.svg)](https://github.com/flaviodelgrosso/tenet/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Status: Experimental MVP](https://img.shields.io/badge/status-experimental_MVP-orange)](#current-status)

## Coding agents write code

## **Tenet decides whether the repository has earned `DONE`.**

**An evidence-driven control plane for autonomous, spec-driven software development.**

</div>

---

## Current status

> **Tenet is an experimental MVP.**
>
> The core control architecture is implemented.
> Its advantage over simpler coding-agent loops has **not yet been demonstrated experimentally**.

Tenet is built around a research hypothesis:

> **Separating probabilistic coding work from deterministic process authority can reduce false completion, regressions, and long-horizon drift in autonomous software development.**

That hypothesis still needs benchmarks.

Tenet does **not** currently claim to be faster, cheaper, safer, or more reliable than a strong single coding agent or a simple iterative agent loop.

The project exists to find out.

### What exists today

| Capability                                                                    | Status          |
| ----------------------------------------------------------------------------- | --------------- |
| Specification → requirements / acceptance criteria / verification obligations | ✅ Implemented  |
| Architect, Reconcile, Implement, Repair, and Assess roles                     | ✅ Implemented  |
| Fresh role-specific agent sessions                                            | ✅ Implemented  |
| ACP-based agent execution                                                     | ✅ Implemented  |
| Controller-owned completion authority                                         | ✅ Implemented  |
| Controller-executed deterministic verification                                | ✅ Implemented  |
| Revision-scoped requirement evidence graph                                    | ✅ Implemented  |
| Evidence invalidation after relevant changes                                  | ✅ Implemented  |
| Isolated Git worktrees                                                        | ✅ Implemented  |
| Candidate scope enforcement                                                   | ✅ Implemented  |
| Protected controller-owned paths                                              | ✅ Implemented  |
| Dependency-aware scheduling                                                   | ✅ Implemented  |
| Transactional integration and recovery journal                                | ✅ Implemented  |
| Persistent controller state                                                   | ✅ Implemented  |
| TUI and headless execution                                                    | ✅ Implemented  |
| Comparative evaluation harness                                                | 🚧 Planned      |
| Published single-agent / simple-loop / Tenet benchmarks                       | ❌ Not yet      |
| Failure-specific recovery routing                                             | 🚧 Planned      |
| Controller-owned engineering memory                                           | 🚧 Planned      |
| Safe parallel work units                                                      | 🚧 Planned      |
| Host/container security sandbox                                               | ❌ Not provided |
| GitHub PR / merge-readiness workflow                                          | 🚧 Planned      |

---

# The problem

Coding agents are becoming very good at **changing code**.

That is not the same thing as being good at **finishing software autonomously**.

Long-running coding workflows have a different set of failure modes:

- context accumulates;
- assumptions become stale;
- plans drift away from the repository;
- failures get summarized instead of preserved as evidence;
- later changes regress earlier work;
- the agent that wrote the implementation often evaluates its own work;
- weak tests create false confidence;
- and eventually something says _“done.”_

Tenet moves process authority out of the conversation.

```text
                         probabilistic
                            workers
                               │
                               ▼
spec ─────────────────► ┌───────────────┐
repo ─────────────────► │     TENET     │
                        │               │
                        │ requirements  │
                        │ scheduling    │
                        │ verification  │
                        │ evidence      │
                        │ integration   │
                        │ stopping      │
                        └───────┬───────┘
                                │
                                ▼
                            repository
```

Agents still reason.

Agents still write code.

Agents still repair failures.

Agents still inspect the result.

But **agents do not decide what counts as authoritative verification, and they cannot declare the entire run complete by saying that the work is finished.**

---

# The control loop

A Tenet run repeatedly asks one question:

> **Given the specification, the current repository, and the evidence we actually have, what still needs to become true?**

```text
specification
     │
     ▼
 Architect
     │
     ▼
requirements + acceptance criteria + verification obligations
     │
     ▼
 Reconcile ◄──────────────────────────────────────┐
     │                                            │
     ▼                                            │
bounded work unit                                 │
     │                                            │
     ▼                                            │
fresh implementation session                     │
     │                                            │
     ▼                                            │
isolated Git worktree                             │
     │                                            │
     ▼                                            │
immutable candidate commit                        │
     │                                            │
     ▼                                            │
controller-run verification                       │
     │                                            │
     ├── fail ──► bounded Repair ──► verify ──────┤
     │                                            │
     └── pass ──► controlled integration ─────────┘
                                      │
                                      ▼
                              skeptical Assess
                                      │
                                      ▼
                                    DONE?
```

The repository evolves.

The plan is allowed to evolve with it.

The specification and controller-owned evidence remain the reference points.

---

# `DONE` is a state, not a sentence

A coding agent can always produce:

```text
Everything has been implemented successfully.
```

Tenet does not treat that statement as completion.

Conceptually, `DONE(R)` is derived for one exact canonical repository revision `R`.

It requires, among other invariants:

```text
authoritative specification
        +
valid requirement catalog
        +
mandatory acceptance criteria
        +
required verification obligations
        +
controller-observed passing evidence at revision R
        +
no blocking contradictory evidence
        +
no remaining work from current reconciliation
        +
no concrete gap from skeptical assessment
        +
repository-wide deterministic gates pass
        +
canonical HEAD is still R
        +
canonical working tree is clean
        +
no active candidate / lease / integration transaction
        +
state and evidence are persisted successfully
        =
DONE(R)
```

`DONE` does **not** mean mathematically proven correct.

It means:

> **the repository satisfied the specification through the evidence and assessment mechanisms available to this run.**

That distinction matters.

A bad specification can still produce the wrong software.

Weak tests can still produce weak evidence.

An agent can still miss something.

Tenet does not remove uncertainty from software engineering.

It tries to make that uncertainty **explicit, revision-bound, inspectable, and harder to hand-wave away**.

---

# Evidence, not agent confidence

Suppose the specification contains:

```text
REQ-003
Password reset tokens expire after 15 minutes.
```

An acceptance criterion might require:

```text
AC-003-01
A token is accepted before expiry and rejected after expiry.
```

And a verification obligation might bind that requirement to:

```text
VO-003-01
cargo test password_reset_expiry
```

Tenet's authoritative evidence is not:

```text
Agent: "I ran the tests and they pass."
```

It is closer to:

```text
requirement:  REQ-003
criterion:    AC-003-01
obligation:   VO-003-01
revision:     abc123...
program:      cargo
arguments:    test password_reset_expiry
exit_code:    0
observed_by:  controller
```

If a later change touches the dependency scope supporting that evidence, Tenet can mark it stale and require verification again.

The model proposes.

**The controller observes.**

---

# Why not just use a simple loop?

A simple coding-agent loop can be extremely effective:

```text
inspect
  ↓
change
  ↓
test
  ↓
repeat
```

Tenet deliberately adds more machinery:

```text
specification
  ↓
explicit requirements
  ↓
repository reconciliation
  ↓
bounded work
  ↓
isolated candidate
  ↓
controller-run verification
  ↓
revision-scoped evidence
  ↓
controlled integration
  ↓
independent reassessment
  ↓
controller-owned completion
```

The additional complexity is only justified if it produces measurable benefits.

That is an empirical question.

Tenet needs to demonstrate whether this architecture reduces things such as:

- false `DONE`;
- regressions;
- requirement drift;
- repeated failed work;
- unrecoverable repository mutations;
- run-to-run variance;

without imposing unacceptable:

- token cost;
- wall-clock cost;
- model-call overhead;
- orchestration complexity.

Until comparative benchmarks exist, **simpler loops remain a completely reasonable alternative**.

---

# Replaceable context, durable engineering state

Tenet deliberately avoids relying on one permanent master conversation.

Different roles use fresh sessions.

```text
model reasoning          → replaceable
repository changes       → durable
specification            → durable
requirements             → durable
verification results     → durable
controller state         → durable
```

The motivation is straightforward.

A long-lived agent context can accumulate:

- obsolete architecture assumptions;
- old plans;
- compressed versions of previous failures;
- unverified conclusions;
- confidence inherited from its own earlier reasoning.

A fresh worker can instead inspect current repository state.

But this design has a cost: fresh workers may repeatedly rediscover stable facts.

So this is a **hypothesis, not dogma**.

One important future direction is controller-owned engineering memory: structured, revision-aware facts that can survive sessions without recreating one giant permanent conversation.

---

# What happens during a run

## 1. Architect

Architect turns the specification into a typed catalog containing:

- requirements;
- acceptance criteria;
- verification obligations;
- stable semantic IDs.

An acceptance criterion describes **what must be true**.

A verification obligation describes **how that truth must be demonstrated**.

Architect proposes the catalog.

The controller validates its structure.

---

## 2. Reconcile

A fresh worker inspects the repository as it exists now and compares it with the catalog.

It can propose:

- implementation observations;
- missing implementation;
- missing evidence;
- candidate verification;
- bounded work units;
- dependencies between work.

Reconcile controls planning input.

It does **not** have verification authority.

A model believing that something is implemented is not the same as the controller possessing evidence that it is implemented.

---

## 3. Implement

A worker receives bounded work and an explicit repository scope.

Implementation occurs in an isolated detached Git worktree.

Before a candidate can proceed, Tenet checks the complete candidate diff against the authority granted to that work unit.

Unexpected out-of-scope modifications are rejected.

A worker that discovers legitimate additional work can request scope expansion for a future reconciliation instead of silently widening its own authority.

---

## 4. Verify

The candidate is committed first.

Verification then runs against an immutable checkout of that candidate revision.

Examples:

```bash
cargo test
make ci
./scripts/acceptance.sh
```

Configured project gates can produce authoritative evidence.

Agent-proposed checks remain advisory unless explicitly authorized by project configuration.

Passing and failing evidence are both preserved.

A valid contradictory failure cannot be erased by a later optimistic model statement.

---

## 5. Repair

A failed verification attempt becomes structured input to a new repair attempt:

```text
candidate repository
+
verification command
+
exit code
+
stdout
+
stderr
        │
        ▼
fresh Repair worker
```

Repairs are bounded.

Tenet prefers an explicit blocked state over infinite retry.

Today, failure routing is still relatively coarse.

A future controller should distinguish failures such as:

```text
implementation defect
regression
environment failure
dependency failure
flaky verification
requirement misunderstanding
ambiguous specification
```

because they should not all lead to the same recovery action.

---

## 6. Integrate

A verified candidate is integrated through the controller rather than being allowed to mutate canonical state directly.

Tenet maintains a durable integration journal around canonical advancement.

If interruption occurs during integration, startup reconciles the journal against the actual Git revision rather than guessing whether the operation succeeded.

After integration, the repository is reconciled again.

Yesterday's plan is not automatically trusted after today's code change.

---

## 7. Assess

Once deterministic evidence gates pass, a fresh skeptical worker searches for concrete implementation or evidence gaps.

Assess can veto completion by proposing a specific gap.

It cannot authorize completion.

The controller remains the final authority.

---

# Repository integrity

Tenet treats agent execution and canonical repository state as separate concerns.

### Read-oriented workers

Architect, Reconcile, and Assess operate in disposable detached worktrees at the exact revision they inspect.

Their worktrees are discarded afterward.

Tenet also checks canonical repository state around these workers and fails if a supposedly read-only role unexpectedly changes it.

### Implementation workers

Implement and Repair use leased detached worktrees.

Candidate changes are inspected before acceptance, including:

- additions;
- modifications;
- deletions;
- renames;
- generated files;
- executable-mode changes;
- symlink changes.

### Protected paths

Tenet always protects controller-owned project state including:

- the configured specification;
- `tenet.toml`;
- `AGENTS.md`;
- `.tenet`.

Projects can add more protected paths.

They cannot remove the mandatory ones.

---

# Security boundary

**Git worktree isolation is not a security sandbox.**

This is important.

Worktrees protect canonical repository state from uncontrolled mutations.

They do **not** inherently isolate a coding agent from the host environment.

Depending on the configured agent/runtime, a worker may still inherit access to things such as:

```text
filesystem outside the repository
network
environment variables
credentials
local services
processes
Docker
SSH configuration
cloud tooling
```

Tenet does not currently claim that arbitrary coding-agent execution is safe on a sensitive host.

Use appropriate external sandboxing and credential isolation when the threat model requires it.

---

# Agent-neutral by design

Tenet communicates with coding agents through the **[Agent Client Protocol (ACP)](https://agentclientprotocol.com/)**.

```text
                         ┌─────────────────┐
spec ───────────────────►│                 │
repo ───────────────────►│      Tenet      │
                         │                 │
                         │ controller      │
                         │ verification    │
                         │ evidence        │
                         └────────┬────────┘
                                  │
                                 ACP
                                  │
                  ┌───────────────┼───────────────┐
                  ▼               ▼               ▼
               Agent A         Agent B       custom ACP
```

The controller should not have to become a different product every time a better coding model appears.

Different Tenet roles can also use different model preferences while preserving the same surrounding control semantics.

---

# Quick start

## 1. Build Tenet

Tenet is currently distributed from source.

```bash
git clone https://github.com/flaviodelgrosso/tenet.git
cd tenet
make install
```

A current stable Rust toolchain is required.

---

## 2. Initialize a project

Inside the repository Tenet should operate on:

```bash
tenet init
```

A run requires:

- an existing Git repository;
- at least one commit;
- a clean canonical working tree.

`tenet init` creates the project configuration and Tenet state directory.

---

## 3. Write the specification

The default specification is `spec.md`.

For example:

```markdown
# Bookmark API

## Requirements

- Users can create a bookmark with a URL and title.
- Bookmark URLs must be unique.
- Users can list bookmarks ordered by creation time.
- Users can delete bookmarks.
- Invalid URLs return HTTP 400.
- The API has integration tests.
- `cargo test` passes.
```

The specification is not merely a one-shot prompt.

It remains the reference against which the repository is reconciled throughout the run.

The path can be changed in `tenet.toml`.

---

## 4. Configure an agent

Explore ACP Registry agents:

```bash
tenet agents list
tenet agents search <query>
tenet agents select <id>
```

Inspect the configured runtime:

```bash
tenet agents doctor
```

Tenet also supports custom ACP commands through `tenet.toml`.

---

## 5. Configure verification

This is one of the most important parts of a Tenet project.

Example:

```toml
[verification]

[[verification.gates]]
obligation_ids = ["REQ-001/AC-01/VO-01"]
dependency_scope = ["src/**", "tests/**"]
spec = {
  program = "cargo",
  args = ["test"],
  workingDirectory = ".",
  environment = {}
}
```

Project-configured gates bind executable verification to explicit verification obligations.

An agent cannot promote its own successful command execution into trusted evidence merely by reporting that it passed.

And no orchestration system can compensate for a verification suite that proves the wrong thing.

---

## 6. Run

Interactive mode:

```bash
tenet run
```

Headless:

```bash
tenet run --headless
```

Less output:

```bash
tenet run --headless --quiet
```

More diagnostics:

```bash
tenet run --headless --verbose
```

Continue from persisted state:

```bash
tenet resume
```

---

# Terminal UI

The default terminal interface focuses on the engineering process rather than only displaying an agent transcript.

Views expose things such as:

- active worker;
- requirement progress;
- evidence;
- verification;
- controller transitions;
- timeline.

Controls:

```text
Tab                 switch view
Home / g            top
Up / Down, j / k    scroll
End / G             follow
PageUp / PageDown   faster scroll
q / Ctrl-C          stop / exit
```

For CI, SSH, server execution, or log capture:

```bash
tenet run --headless > tenet.log
```

---

# Configuration

A new project starts with a small `tenet.toml`:

```toml
version = 1
spec_file = "spec.md"
max_cycles = 25
max_repair_attempts = 3

[agent]
```

Select an ACP Registry agent:

```bash
tenet agents select <id>
```

or configure a custom ACP source.

Additional project paths can be protected:

```toml
additional_protected_paths = [
  "secrets",
  "deployment/production"
]
```

Advanced controller, agent, verification, and execution settings are documented by:

```text
schemas/config.schema.json
```

Stable defaults are intentionally omitted from normal project configuration.

---

# Useful commands

Initialize a project:

```bash
tenet init
```

Run or continue:

```bash
tenet run
tenet resume
```

Inspect persisted state:

```bash
tenet status
tenet status --json
```

Run configured deterministic verification without invoking an LLM:

```bash
tenet verify
tenet verify --json
```

Manage ACP agents:

```bash
tenet agents list
tenet agents search <query>
tenet agents select <id>
tenet agents setup
tenet agents doctor
tenet agents login
```

---

# Architecture

Tenet is split into Rust crates with deliberate dependency boundaries.

### `tenet-domain`

Semantic state, IDs, evidence types, worker contracts, configuration, and pure invariants.

### `tenet-runtime`

Repository operations, workspaces, scheduling mechanisms, verification execution, integration, and persistence.

### `tenet-controller`

The control loop.

It owns:

- catalog trust policy;
- evidence trust policy;
- verification authorization;
- scheduling decisions;
- stopping conditions;
- completion authority.

### `tenet-acp`

Adapts ACP-compatible coding-agent runtimes to the controller's agent backend interface.

### `tenet-projection`

Read-side projections of controller state and evidence.

### `tenet-tui`

Interactive terminal presentation.

### `tenet-cli`

Application composition and command-line entry point.

The dependency direction is deliberate.

The runtime does not own completion semantics.

ACP does not own controller policy.

The model adapter is replaceable.

---

# What Tenet has not proven

This section is intentionally explicit.

There is not yet enough comparative evidence to claim that Tenet:

- solves more software tasks than a strong single-agent workflow;
- beats a simple Ralph-style loop;
- reduces false completion;
- reduces regressions;
- uses fewer tokens;
- costs less;
- finishes faster;
- has lower run-to-run variance;
- is safer for unattended production development.

Those are testable hypotheses.

They should be treated as such.

---

# Evaluation is the next major milestone

The most important next feature is not another worker role.

It is measurement.

Tenet should eventually make experiments like this reproducible:

| Strategy                  | Task success | Hidden tests | False `DONE` | Regressions | Tokens | Cost | Wall time |
| ------------------------- | -----------: | -----------: | -----------: | ----------: | -----: | ---: | --------: |
| Single agent              |            — |            — |            — |           — |      — |    — |         — |
| Simple iterative loop     |            — |            — |            — |           — |      — |    — |         — |
| Tenet                     |            — |            — |            — |           — |      — |    — |         — |
| Tenet without Assess      |            — |            — |            — |           — |      — |    — |         — |
| Tenet without Reconcile   |            — |            — |            — |           — |      — |    — |         — |
| Tenet with shared context |            — |            — |            — |           — |      — |    — |         — |

No numbers are published here because those experiments have not yet been run rigorously enough to justify them.

Of particular interest is **false `DONE`**:

> How often does a workflow declare the repository complete when independent acceptance criteria show that it is not?

That may be a more important metric for autonomous engineering than raw task completion alone.

---

# What Tenet is not

### Not a coding model

Tenet does not compete with coding agents.

It controls the process around them.

### Not a correctness oracle

Passing tests can still mean shipping the wrong thing.

### Not a specification oracle

An ambiguous or incomplete specification can lead to an incomplete requirement catalog.

### Not a security sandbox

Git isolation protects repository state.

It does not provide complete host isolation.

### Not proof that multi-agent systems are better

The use of separate roles is an architectural choice whose value should be measured through ablation.

### Not proven better than simpler loops

Not yet.

### Not production-ready

Also not yet.

---

# Project principles

### The repository outranks the conversation

If model context and repository reality disagree, inspect reality again.

### Agent claims are hypotheses

Where deterministic evidence can be obtained, obtain it.

### Completion authority should be separate from implementation

The worker that wrote the code should not be able to finish the entire process by asserting success.

### Engineering state should survive model context

Important facts should not exist only inside a conversation.

### Planning is provisional

After the repository changes, yesterday's plan is only a hypothesis.

### Verification should have provenance

A passing command matters more when the controller knows:

- what it verified;
- why it was authorized;
- which revision it ran against;
- what output it produced.

### Failure should become structured input

```text
command
exit code
stdout
stderr
repository revision
```

is better recovery input than:

```text
something failed
```

### Infinite retries are not autonomy

Sometimes the correct state is:

```text
BLOCKED
```

---

# Roadmap

The project should prioritize evidence over architectural expansion.

## Evaluation

Build reproducible comparison infrastructure for:

- single-agent execution;
- simple iterative loops;
- full Tenet;
- architectural ablations.

Measure successful and failed runs.

## Specification quality

Strengthen specification analysis and adversarial review before implementation begins.

The controller cannot verify requirements that were never captured.

## Failure attribution

Differentiate:

- code defects;
- regressions;
- environment failures;
- dependency failures;
- flaky tests;
- specification ambiguity;
- requirement misunderstanding.

Route recovery accordingly.

## Verification quality

Go beyond merely invoking existing project tests.

Potential future evidence sources include:

- generated acceptance tests;
- property testing;
- mutation testing;
- static analysis;
- security checks;
- differential testing;
- hidden evaluation suites.

## Engineering memory

Preserve stable, structured repository knowledge without recreating one permanent conversational context.

## Security

Add stronger execution boundaries where appropriate:

- filesystem capabilities;
- network policy;
- secret isolation;
- resource limits;
- container / VM execution.

## Safe concurrency

Parallel workers should be introduced only where independence can be established and final integrated verification remains controller-owned.

## GitHub / CI workflows

Turn a Tenet run into an evidence-backed merge-readiness report containing things such as:

- requirements satisfied;
- verification evidence;
- regressions;
- repair attempts;
- cost;
- human interventions;
- unresolved assumptions.

---

# Contributing

Tenet is early enough that failed experiments are particularly valuable.

Useful contributions include:

- reproducible bug reports;
- adversarial or ambiguous specifications;
- repositories that break Tenet's assumptions;
- ACP compatibility fixes;
- controller correctness work;
- verification improvements;
- worktree and Git edge cases;
- benchmark tasks;
- evaluation infrastructure;
- run observability;
- security improvements;
- documentation;
- negative benchmark results.

Especially negative results.

If a simpler architecture consistently beats Tenet, the project should discover that rather than hide it.

See [CONTRIBUTING.md](CONTRIBUTING.md).

---

# A note on trust

Autonomous development tools should be judged by what they can demonstrate, not by how confidently they describe themselves.

Tenet is built around that idea.

The project should be held to the same standard.

Today there is:

- a functioning control architecture;
- a serious evidence model;
- strong repository-integrity mechanisms;
- an experimental MVP;
- and a large amount still to prove.

That is enough for now.

---

<div align="center">

## Agents write the code

## **Tenet makes the repository earn `DONE`.**

Try it. Break it. Measure it.

And help make the loop harder to fool.

[Report a bug](https://github.com/flaviodelgrosso/tenet/issues) · [Contribute](CONTRIBUTING.md) · [MIT License](LICENSE)

</div>
