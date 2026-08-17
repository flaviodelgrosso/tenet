<div align="center">

<img src="https://github.com/user-attachments/assets/4c078e90-69f5-44d9-a634-91eefbc4470f" alt="Tenet" width="720" />

# 🪢 Tenet

### Coding agents write code. **Tenet decides whether the repository has earned `DONE`.**

_An evidence-driven control plane for autonomous, spec-driven software development._

[![CI](https://github.com/flaviodelgrosso/tenet/actions/workflows/ci.yml/badge.svg)](https://github.com/flaviodelgrosso/tenet/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Status: Experimental MVP](https://img.shields.io/badge/status-experimental_MVP-orange)](#-current-status)

[Quick Start](#-quick-start) · [How It Works](#-the-control-loop) · [Architecture](#-architecture) · [Roadmap](#-roadmap) · [Contributing](#-contributing)

</div>

---

## 📋 Table of contents

- [Current status](#-current-status)
- [The problem](#-the-problem)
- [The control loop](#-the-control-loop)
- [`DONE` is a state, not a sentence](#-done-is-a-state-not-a-sentence)
- [Evidence, not agent confidence](#-evidence-not-agent-confidence)
- [Why not just use a simple loop?](#-why-not-just-use-a-simple-loop)
- [Replaceable context, durable state](#-replaceable-context-durable-engineering-state)
- [What happens during a run](#-what-happens-during-a-run)
- [Repository integrity](#-repository-integrity)
- [Security boundary](#-security-boundary)
- [Agent-neutral by design](#-agent-neutral-by-design)
- [Quick start](#-quick-start)
- [Terminal UI](#-terminal-ui)
- [Configuration](#-configuration)
- [Useful commands](#-useful-commands)
- [Architecture](#-architecture)
- [What Tenet has not proven](#-what-tenet-has-not-proven)
- [Evaluation](#-evaluation-is-the-next-major-milestone)
- [What Tenet is not](#-what-tenet-is-not)
- [Project principles](#-project-principles)
- [Roadmap](#-roadmap)
- [Contributing](#-contributing)

---

## 🚦 Current status

> **Tenet is an experimental MVP.**
> The core control architecture is implemented. Its advantage over simpler coding-agent loops has **not yet been demonstrated experimentally**.

Tenet is built around a research hypothesis:

> **Separating probabilistic coding work from deterministic process authority can reduce false completion, regressions, and long-horizon drift in autonomous software development.**

That hypothesis still needs benchmarks. Tenet does **not** currently claim to be faster, cheaper, safer, or more reliable than a strong single coding agent or a simple iterative agent loop. The project exists to find out.

### What exists today

| Capability                                                                    |     Status      |
| ----------------------------------------------------------------------------- | :-------------: |
| Specification → requirements / acceptance criteria / verification obligations |       ✅        |
| Architect, Reconcile, Implement, Repair, and Assess roles                     |       ✅        |
| Fresh role-specific agent sessions                                            |       ✅        |
| ACP-based agent execution                                                     |       ✅        |
| Controller-owned completion authority                                         |       ✅        |
| Controller-executed deterministic verification                                |       ✅        |
| Revision-scoped requirement evidence graph                                    |       ✅        |
| Evidence invalidation after relevant changes                                  |       ✅        |
| Isolated Git worktrees                                                        |       ✅        |
| Candidate scope enforcement                                                   |       ✅        |
| Protected controller-owned paths                                              |       ✅        |
| Dependency-aware scheduling                                                   |       ✅        |
| Transactional integration and recovery journal                                |       ✅        |
| Persistent controller state                                                   |       ✅        |
| TUI and headless execution                                                    |       ✅        |
| Comparative evaluation harness                                                |   🚧 Planned    |
| Published single-agent / simple-loop / Tenet benchmarks                       |   ❌ Not yet    |
| Failure-specific recovery routing                                             |   🚧 Planned    |
| Controller-owned engineering memory                                           |   🚧 Planned    |
| Safe parallel work units                                                      |   🚧 Planned    |
| Host/container security sandbox                                               | ❌ Not provided |
| GitHub PR / merge-readiness workflow                                          |   🚧 Planned    |

---

## ❗ The problem

Coding agents are becoming very good at **changing code**. That is not the same thing as being good at **finishing software autonomously**.

Long-running coding workflows have a different set of failure modes:

- context accumulates
- assumptions become stale
- plans drift away from the repository
- failures get summarized instead of preserved as evidence
- later changes regress earlier work
- the agent that wrote the implementation often evaluates its own work
- weak tests create false confidence
- and eventually something says _"done."_

Tenet moves process authority out of the conversation:

```
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

Agents still reason. Agents still write code. Agents still repair failures. Agents still inspect the result.

But **agents do not decide what counts as authoritative verification**, and they cannot declare the entire run complete by saying that the work is finished.

---

## 🔁 The control loop

A Tenet run repeatedly asks one question:

> **Given the specification, the current repository, and the evidence we actually have, what still needs to become true?**

```
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
     │                                             │
     ▼                                             │
bounded work unit                                  │
     │                                             │
     ▼                                             │
fresh implementation session                       │
     │                                             │
     ▼                                             │
isolated Git worktree                              │
     │                                             │
     ▼                                             │
immutable candidate commit                         │
     │                                             │
     ▼                                             │
controller-run verification                        │
     │                                             │
     ├── fail ──► bounded Repair ──► verify ────────┤
     │                                             │
     └── pass ──► controlled integration ───────────┘
                                       │
                                       ▼
                               skeptical Assess
                                       │
                                       ▼
                                     DONE?
```

The repository evolves. The plan is allowed to evolve with it. The specification and controller-owned evidence remain the reference points.

---

## ✅ `DONE` is a state, not a sentence

A coding agent can always produce:

```
Everything has been implemented successfully.
```

Tenet does not treat that statement as completion. Conceptually, `DONE(R)` is derived for one exact canonical repository revision `R`. It requires, among other invariants:

```
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

`DONE` does **not** mean mathematically proven correct. It means:

> **the repository satisfied the specification through the evidence and assessment mechanisms available to this run.**

That distinction matters. A bad specification can still produce the wrong software. Weak tests can still produce weak evidence. An agent can still miss something.

Tenet does not remove uncertainty from software engineering — it tries to make that uncertainty **explicit, revision-bound, inspectable, and harder to hand-wave away**.

---

## 🔬 Evidence, not agent confidence

Suppose the specification contains:

```
REQ-003
Password reset tokens expire after 15 minutes.
```

An acceptance criterion might require:

```
AC-003-01
A token is accepted before expiry and rejected after expiry.
```

And a verification obligation might bind that requirement to:

```
VO-003-01
cargo test password_reset_expiry
```

Tenet's authoritative evidence is **not**:

```
Agent: "I ran the tests and they pass."
```

It is closer to:

```
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

**The model proposes. The controller observes.**

---

## 🆚 Why not just use a simple loop?

A simple coding-agent loop can be extremely effective:

```
inspect → change → test → repeat
```

Tenet deliberately adds more machinery:

```
specification → explicit requirements → repository reconciliation → bounded work
→ isolated candidate → controller-run verification → revision-scoped evidence
→ controlled integration → independent reassessment → controller-owned completion
```

The additional complexity is only justified if it produces measurable benefits — that is an empirical question. Tenet needs to demonstrate whether this architecture reduces things such as:

- false `DONE`
- regressions
- requirement drift
- repeated failed work
- unrecoverable repository mutations
- run-to-run variance

...without imposing unacceptable token cost, wall-clock cost, model-call overhead, or orchestration complexity.

Until comparative benchmarks exist, **simpler loops remain a completely reasonable alternative**.

---

## 🧠 Replaceable context, durable engineering state

Tenet deliberately avoids relying on one permanent master conversation. Different roles use fresh sessions.

| Element              | Nature      |
| -------------------- | ----------- |
| Model reasoning      | Replaceable |
| Repository changes   | Durable     |
| Specification        | Durable     |
| Requirements         | Durable     |
| Verification results | Durable     |
| Controller state     | Durable     |

A long-lived agent context can accumulate obsolete architecture assumptions, old plans, compressed versions of previous failures, unverified conclusions, and confidence inherited from its own earlier reasoning. A fresh worker can instead inspect current repository state.

But this design has a cost: fresh workers may repeatedly rediscover stable facts — so this is a **hypothesis, not dogma**. One important future direction is controller-owned engineering memory: structured, revision-aware facts that can survive sessions without recreating one giant permanent conversation.

---

## ⚙️ What happens during a run

### 1 · Architect

Turns the specification into a typed catalog containing requirements, acceptance criteria, verification obligations, and stable semantic IDs. An acceptance criterion describes **what must be true**; a verification obligation describes **how that truth must be demonstrated**. Architect proposes the catalog — the controller validates its structure.

### 2 · Reconcile

A fresh worker inspects the repository as it exists now and compares it with the catalog. It can propose implementation observations, missing implementation, missing evidence, candidate verification, bounded work units, and dependencies between work. Reconcile controls planning input — it does **not** have verification authority. A model believing something is implemented is not the same as the controller possessing evidence that it is.

### 3 · Implement

A worker receives bounded work and an explicit repository scope, in an isolated detached Git worktree. Before a candidate can proceed, Tenet checks the complete candidate diff against the authority granted to that work unit — unexpected out-of-scope modifications are rejected. A worker that discovers legitimate additional work can request scope expansion for a future reconciliation instead of silently widening its own authority.

### 4 · Verify

The candidate is committed first; verification then runs against an immutable checkout of that candidate revision (e.g. `cargo test`, `make ci`, `./scripts/acceptance.sh`). Configured project gates produce authoritative evidence — agent-proposed checks remain advisory unless explicitly authorized by project configuration. Passing and failing evidence are both preserved; a valid contradictory failure cannot be erased by a later optimistic model statement.

### 5 · Repair

A failed verification attempt becomes structured input to a new repair attempt: candidate repository + verification command + exit code + stdout + stderr → fresh Repair worker. Repairs are bounded — Tenet prefers an explicit blocked state over infinite retry. Today, failure routing is still relatively coarse; a future controller should distinguish defects, regressions, environment failures, dependency failures, flaky verification, and specification ambiguity, since they shouldn't all lead to the same recovery action.

### 6 · Integrate

A verified candidate is integrated through the controller rather than being allowed to mutate canonical state directly. Tenet maintains a durable integration journal around canonical advancement — if interruption occurs, startup reconciles the journal against the actual Git revision rather than guessing whether the operation succeeded. After integration, the repository is reconciled again: yesterday's plan is not automatically trusted after today's code change.

### 7 · Assess

Once deterministic evidence gates pass, a fresh skeptical worker searches for concrete implementation or evidence gaps. Assess can **veto** completion by proposing a specific gap — it cannot authorize completion. The controller remains the final authority.

---

## 🛡️ Repository integrity

Tenet treats agent execution and canonical repository state as separate concerns.

**Read-oriented workers** — Architect, Reconcile, and Assess operate in disposable detached worktrees at the exact revision they inspect; their worktrees are discarded afterward. Tenet also checks canonical repository state around these workers and fails if a supposedly read-only role unexpectedly changes it.

**Implementation workers** — Implement and Repair use leased detached worktrees. Candidate changes are inspected before acceptance, including additions, modifications, deletions, renames, generated files, executable-mode changes, and symlink changes.

**Protected paths** — Tenet always protects controller-owned project state, including the configured specification, `tenet.toml`, `AGENTS.md`, and `.tenet`. Projects can add more protected paths but cannot remove the mandatory ones.

---

## 🔐 Security boundary

> **Git worktree isolation is not a security sandbox.**

Worktrees protect canonical repository state from uncontrolled mutations. They do **not** inherently isolate a coding agent from the host environment. Depending on the configured agent/runtime, a worker may still inherit access to the filesystem outside the repository, network, environment variables, credentials, local services, processes, Docker, SSH configuration, and cloud tooling.

Tenet does not currently claim that arbitrary coding-agent execution is safe on a sensitive host. Use appropriate external sandboxing and credential isolation when the threat model requires it.

---

## 🔌 Agent-neutral by design

Tenet communicates with coding agents through the **[Agent Client Protocol (ACP)](https://agentclientprotocol.com/)**.

```
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

The controller should not have to become a different product every time a better coding model appears. Different Tenet roles can also use different model preferences while preserving the same surrounding control semantics.

---

## 🚀 Quick start

### 1. Build Tenet

Tenet is currently distributed from source (a current stable Rust toolchain is required):

```bash
git clone https://github.com/flaviodelgrosso/tenet.git
cd tenet
make install
```

### 2. Initialize a project

Inside the repository Tenet should operate on:

```bash
tenet init
```

A run requires an existing Git repository, at least one commit, and a clean canonical working tree. `tenet init` creates the project configuration and Tenet state directory.

### 3. Write the specification

The default specification is `spec.md`. For example:

```md
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

The specification is not merely a one-shot prompt — it remains the reference against which the repository is reconciled throughout the run. The path can be changed in `tenet.toml`.

### 4. Configure an agent

```bash
tenet agents list
tenet agents search <query>
tenet agents select <id>
tenet agents doctor      # inspect the configured runtime
```

Tenet also supports custom ACP commands through `tenet.toml`.

### 5. Configure verification

This is one of the most important parts of a Tenet project:

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

Project-configured gates bind executable verification to explicit verification obligations. An agent cannot promote its own successful command execution into trusted evidence merely by reporting that it passed — and no orchestration system can compensate for a verification suite that proves the wrong thing.

### 6. Run

```bash
tenet run                              # interactive
tenet run --headless                   # headless
tenet run --headless --quiet           # less output
tenet run --headless --verbose         # more diagnostics
tenet resume                           # continue from persisted state
```

---

## 🖥️ Terminal UI

The default terminal interface focuses on the engineering process rather than only displaying an agent transcript. Views expose the active worker, requirement progress, evidence, verification, controller transitions, and timeline.

| Key                      | Action        |
| ------------------------ | ------------- |
| `Tab`                    | switch view   |
| `Home` / `g`             | top           |
| `Up` / `Down`, `j` / `k` | scroll        |
| `End` / `G`              | follow        |
| `PageUp` / `PageDown`    | faster scroll |
| `q` / `Ctrl-C`           | stop / exit   |

For CI, SSH, server execution, or log capture:

```bash
tenet run --headless > tenet.log
```

---

## 🛠️ Configuration

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

...or configure a custom ACP source. Additional project paths can be protected:

```toml
additional_protected_paths = [
  "secrets",
  "deployment/production"
]
```

Advanced controller, agent, verification, and execution settings are documented in [`schemas/config.schema.json`](schemas/config.schema.json). Stable defaults are intentionally omitted from normal project configuration.

---

## 📎 Useful commands

```bash
# Initialize a project
tenet init

# Run or continue
tenet run
tenet resume

# Inspect persisted state
tenet status
tenet status --json

# Run configured deterministic verification without invoking an LLM
tenet verify
tenet verify --json

# Manage ACP agents
tenet agents list
tenet agents search <query>
tenet agents select <id>
tenet agents setup
tenet agents doctor
tenet agents login
```

---

## 🏗️ Architecture

Tenet is split into Rust crates with deliberate dependency boundaries.

| Crate                  | Responsibility                                                                                                                                                       |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **`tenet-domain`**     | Semantic state, IDs, evidence types, worker contracts, configuration, and pure invariants                                                                            |
| **`tenet-runtime`**    | Repository operations, workspaces, scheduling mechanisms, verification execution, integration, and persistence                                                       |
| **`tenet-controller`** | The control loop — owns catalog trust policy, evidence trust policy, verification authorization, scheduling decisions, stopping conditions, and completion authority |
| **`tenet-acp`**        | Adapts ACP-compatible coding-agent runtimes to the controller's agent backend interface                                                                              |
| **`tenet-projection`** | Read-side projections of controller state and evidence                                                                                                               |
| **`tenet-tui`**        | Interactive terminal presentation                                                                                                                                    |
| **`tenet-cli`**        | Application composition and command-line entry point                                                                                                                 |

The dependency direction is deliberate: the runtime does not own completion semantics, ACP does not own controller policy, and the model adapter is replaceable.

---

## 🧪 What Tenet has not proven

This section is intentionally explicit. There is not yet enough comparative evidence to claim that Tenet:

- solves more software tasks than a strong single-agent workflow
- beats a simple Ralph-style loop
- reduces false completion
- reduces regressions
- uses fewer tokens
- costs less
- finishes faster
- has lower run-to-run variance
- is safer for unattended production development

Those are testable hypotheses. They should be treated as such.

---

## 📊 Evaluation is the next major milestone

The most important next feature is not another worker role — it is **measurement**. Tenet should eventually make experiments like this reproducible:

| Strategy                  | Task success | Hidden tests | False `DONE` | Regressions | Tokens | Cost | Wall time |
| ------------------------- | :----------: | :----------: | :----------: | :---------: | :----: | :--: | :-------: |
| Single agent              |      —       |      —       |      —       |      —      |   —    |  —   |     —     |
| Simple iterative loop     |      —       |      —       |      —       |      —      |   —    |  —   |     —     |
| Tenet                     |      —       |      —       |      —       |      —      |   —    |  —   |     —     |
| Tenet without Assess      |      —       |      —       |      —       |      —      |   —    |  —   |     —     |
| Tenet without Reconcile   |      —       |      —       |      —       |      —      |   —    |  —   |     —     |
| Tenet with shared context |      —       |      —       |      —       |      —      |   —    |  —   |     —     |

No numbers are published here because those experiments have not yet been run rigorously enough to justify them.

Of particular interest is **false `DONE`**: how often does a workflow declare the repository complete when independent acceptance criteria show that it is not? That may be a more important metric for autonomous engineering than raw task completion alone.

---

## 🚫 What Tenet is not

|                                              |                                                                                                       |
| -------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| **Not a coding model**                       | Tenet does not compete with coding agents — it controls the process around them.                      |
| **Not a correctness oracle**                 | Passing tests can still mean shipping the wrong thing.                                                |
| **Not a specification oracle**               | An ambiguous or incomplete specification can lead to an incomplete requirement catalog.               |
| **Not a security sandbox**                   | Git isolation protects repository state — it does not provide complete host isolation.                |
| **Not proof multi-agent systems are better** | The use of separate roles is an architectural choice whose value should be measured through ablation. |
| **Not proven better than simpler loops**     | Not yet.                                                                                              |
| **Not production-ready**                     | Also not yet.                                                                                         |

---

## 🧭 Project principles

- **The repository outranks the conversation** — if model context and repository reality disagree, inspect reality again.
- **Agent claims are hypotheses** — where deterministic evidence can be obtained, obtain it.
- **Completion authority should be separate from implementation** — the worker that wrote the code should not be able to finish the entire process by asserting success.
- **Engineering state should survive model context** — important facts should not exist only inside a conversation.
- **Planning is provisional** — after the repository changes, yesterday's plan is only a hypothesis.
- **Verification should have provenance** — a passing command matters more when the controller knows what it verified, why it was authorized, which revision it ran against, and what output it produced.
- **Failure should become structured input** — `command + exit code + stdout + stderr + repository revision` is better recovery input than `something failed`.
- **Infinite retries are not autonomy** — sometimes the correct state is `BLOCKED`.

---

## 🗺️ Roadmap

The project should prioritize evidence over architectural expansion.

- **Evaluation** — reproducible comparison infrastructure for single-agent execution, simple iterative loops, full Tenet, and architectural ablations; measure successful and failed runs.
- **Specification quality** — strengthen specification analysis and adversarial review before implementation begins. The controller cannot verify requirements that were never captured.
- **Failure attribution** — differentiate code defects, regressions, environment failures, dependency failures, flaky tests, and specification ambiguity, and route recovery accordingly.
- **Verification quality** — go beyond invoking existing project tests: generated acceptance tests, property testing, mutation testing, static analysis, security checks, differential testing, hidden evaluation suites.
- **Engineering memory** — preserve stable, structured repository knowledge without recreating one permanent conversational context.
- **Security** — stronger execution boundaries: filesystem capabilities, network policy, secret isolation, resource limits, container / VM execution.
- **Safe concurrency** — parallel workers introduced only where independence can be established and final integrated verification remains controller-owned.
- **GitHub / CI workflows** — turn a Tenet run into an evidence-backed merge-readiness report (requirements satisfied, verification evidence, regressions, repair attempts, cost, human interventions, unresolved assumptions).

---

## 🤝 Contributing

Tenet is early enough that **failed experiments are particularly valuable**. Useful contributions include:

- reproducible bug reports
- adversarial or ambiguous specifications
- repositories that break Tenet's assumptions
- ACP compatibility fixes
- controller correctness work
- verification improvements
- worktree and Git edge cases
- benchmark tasks and evaluation infrastructure
- run observability
- security improvements
- documentation
- **negative benchmark results** — especially

If a simpler architecture consistently beats Tenet, the project should discover that rather than hide it.

See [CONTRIBUTING.md](CONTRIBUTING.md).

---

## 🎯 A note on trust

Autonomous development tools should be judged by what they can demonstrate, not by how confidently they describe themselves. Tenet is built around that idea, and the project should be held to the same standard.

Today there is a functioning control architecture, a serious evidence model, strong repository-integrity mechanisms, an experimental MVP, and a large amount still to prove.

That is enough for now.

---

<div align="center">

### Agents write the code. **Tenet makes the repository earn `DONE`.**

_Try it. Break it. Measure it. And help make the loop harder to fool._

[Report a bug](https://github.com/flaviodelgrosso/tenet/issues) · [Contribute](CONTRIBUTING.md) · [MIT License](LICENSE)

</div>
