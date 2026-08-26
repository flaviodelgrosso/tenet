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
- [Console output](#console-output)
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
| Append-only console execution                                                 |       ✅        |
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
     │                                            │
     ▼                                            │
bounded work unit                                 │
     │                                            │
     ▼                                            │
fresh implementation session                      │
     │                                            │
     ▼                                            │
isolated Git worktree                             │
     │                                            │
     ▼                                            │
immutable candidate commit                        │
     │                                            │
     ▼                                            │
controller-run project verification               │
     │                                            │
     ├── fail ──► bounded Repair ──► verify ──────┤
     │                                            │
     └── pass ──► controlled integration ─────────┘
                                       │
                                       ▼
                         final project checks at R
                                       │
                                       ▼
                  controller-issued EvidenceArtifacts
                                       │
                                       ▼
                    deterministic EvidenceContract
                                       │
                     ┌─────────────────┼─────────────────┐
                  PROVEN          INSUFFICIENT      CONTRADICTED
                     │                 │                 │
                     ▼                 ▼                 ▼
                   DONE?       acquire/adjudicate      BLOCKED
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
required verification obligations with explicit EvidenceContracts
        +
controller-issued, revision-compatible authoritative artifacts satisfy every contract
        +
deterministically derived ProofState::Proven for every required obligation
        +
advisory model support, suspicion, or prose has no proof authority
        +
no remaining work from current reconciliation
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

> **every required obligation's explicit evidence contract is satisfied by controller-owned authoritative artifacts at the canonical revision, and every independent controller safety gate passes.**

That distinction matters. A bad specification or weak evidence contract can still describe or prove the wrong thing. Human attestation remains unsupported; advisory model judgments cannot bridge missing evidence or select a trusted executable.

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

A verification obligation is a claim paired with an explicit evidence contract. Tenet has two mechanical issuers:

```text
Artifact(NamedProjectCheck("password-reset-expiry"))
Artifact(TrustedVerifierCheck("private-expiry-boundary"))
```

`NamedProjectCheck` binds a public project check from `verification.checks`. It runs in a disposable Git worktree. This protects canonical repository mutation, but it is not a security sandbox.

`TrustedVerifierCheck` binds a check from `verification.trusted_checks`. Its specification is controller configuration: a digest-pinned image, structured program and argument vector, repository-relative working directory, explicit environment, timeout, fixed no-network isolation policy, and resource bounds. Agent output cannot create or modify this specification, select the image, add mounts, pass environment, or grant authority.

The Docker backend first derives a private Git commit from the exact candidate revision with a controller-generated Dockerfile and Dockerfile-specific ignore policy that excludes only the generated build files, validates that the derivation changed only those generated files, and streams a Git archive over an authenticated Docker Engine API connection to build a content-addressed snapshot image. Git submodules are rejected because their contents require candidate-selected remotes. The execution container receives the candidate only through that immutable image: no candidate or canonical host path is mounted. Tenet creates—not shells into—the container and inspects its configuration before and after execution. Authority admission requires every configured control: no network or effective network attachment, no image pull, a read-only root filesystem, no host mounts, disposable size-bounded `/tmp`, private PID namespace, all Linux capabilities dropped with none re-added, enabled `no-new-privileges`, an unprivileged fixed user, the exact image/program/arguments/working directory/environment, and configured memory, CPU, process, timeout, and temporary-storage limits. The effective container environment must exactly equal `HOME`, `TMPDIR`, and explicitly configured entries; digest-pinned verifier images with additional baked-in `ENV` entries must declare and override those entries in controller configuration or execution fails closed.

After admitted execution, the controller authenticates the complete trusted-execution payload and artifact with an HMAC key derived from the externally supplied mTLS private-key identity and a stable operator-assigned repository authority namespace before persistence, then issues an obligation-bound `Authoritative` artifact with `ControllerTrustedVerifier` provenance. The authenticated context binds the active catalog hash and serialized definitions of every bound obligation, in addition to the exact Git revision, verifier name, specification hash, isolation-policy hash, derived image/backend attestation, mutually authenticated control-plane fingerprint, execution observation, and record hash. Reload accepts authority only when both authentication tags, the persisted controller record, current verifier configuration, current catalog/contract context, and the same externally supplied controller identity and repository namespace match. The namespace is included in the control-plane fingerprint and must be unique per repository authority boundary; copying database rows across repositories or replaying authority after a claim changes cannot preserve authority. Missing or changed identity/namespace rejects prior trusted authority as insufficient. A missing exclusive endpoint, namespace, credential descriptor, TLS failure, missing image, malformed specification, timeout, startup failure, OOM, failed cleanup, weaker inspected capability, or unauthenticated persisted payload issues no authoritative artifact.

The exit-code protocol is controller-defined: exit `0` supports the bound claim; a completed non-zero verifier assertion contradicts it. Infrastructure and isolation failures are not semantic contradictions. Deterministic proof evaluation requires a current, valid, authoritative artifact from exactly the named verifier. No positive assessor verdict is required.

The authority classes are:

- **Authoritative:** exact controller-configured `NamedProjectCheck` and `TrustedVerifierCheck` contracts.
- **Supporting:** controller-owned immutable source inspection; it cannot satisfy an authoritative obligation contract.
- **Advisory only:** assessor judgments and reproduction requests. Reproduction remains deferred and is never promoted into trusted execution.
- **Unsupported:** human attestation and generic executable/project-verification obligation contracts.

On repository transitions, invalidation is conservative. Repository-wide trusted artifacts become stale at another revision. Although the domain can represent path/blob compatibility, the controller does not currently claim dependency-aware reuse.

**The model proposes and interprets. The controller configures, executes, admits, persists, and proves.**


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

Turns the specification into a typed catalog containing requirements, acceptance criteria, verification obligations, and stable semantic IDs. An acceptance criterion describes **what must be true**; a verification obligation describes **the semantic claim that must be established** for that criterion. Architect proposes the catalog — the controller validates its structure.

The controller first derives authoritative normative fragments, then sends Architect deterministic batches of at most 64 fragments. Each fragment receives a short batch-bound token such as `B0003-F07`; Architect returns only those tokens in each requirement's `sourceRefs`. The controller rejects unknown or stale tokens and expands accepted tokens into authoritative fragment IDs, sections, and text hashes. Batch-local requirement, criterion, and obligation IDs are deterministically renumbered in batch order before one global coverage check. This bounds each structured response and keeps hashed fragment identities entirely controller-owned. Tenet deliberately does not similarity-merge requirements across batches: probabilistic deduplication could erase distinct normative intent and make IDs unstable, so cross-batch overlap remains visible in the catalog.

### 2 · Reconcile

A fresh worker inspects the repository as it exists now and compares it with the catalog. It can propose implementation observations, missing implementation, missing evidence, candidate verification, bounded work units, and dependencies between work. Reconcile controls planning input — it does **not** have verification authority. A model believing something is implemented is not the same as the controller possessing evidence that it is.

### 3 · Implement

A worker receives bounded work and an explicit repository scope, in an isolated detached Git worktree. Before a candidate can proceed, Tenet checks the complete candidate diff against the authority granted to that work unit. An out-of-scope immutable candidate is pinned under a controller-owned Git ref and carried across reconciliation; it can proceed only when a later validated work unit explicitly authorizes every changed path under the same requirement and verification authority. Stale, superseded, or integrated candidate refs are removed.

### 4 · Verify

The candidate is committed first. Public project verification runs against clean disposable checkouts of that exact revision. Configured named checks can issue obligation-bound public verification artifacts when an evidence contract names them exactly.

Required trusted-verifier contracts then run through the Docker isolation backend. Tenet materializes the exact revision in a disposable worktree but treats that worktree only as immutable input to the container boundary. The controller verifies the created container's policy before starting it and persists the execution record before issuing authority. A trusted assertion failure creates authoritative contradiction; sandbox or runtime failure creates no semantic evidence. Assessor-proposed reproduction commands remain deferred.

### 5 · Repair

A failed advisory verification attempt becomes structured input to a new Repair worker. Trusted-verifier infrastructure failures never route to product repair. Repairs are bounded—Tenet prefers an explicit blocked state over infinite retry.

### 6 · Integrate

A verified candidate is integrated through the controller rather than being allowed to mutate canonical state directly. Tenet maintains a durable integration journal around canonical advancement. After integration, the repository is reconciled again.

### 7 · Advisory adjudication

Current production-valid obligation contracts are mechanically decided by exact configured project checks or trusted verifier checks, so completion does not call Assess merely to confirm them. Assess remains a falsifier and evidence-gap finder. Its judgments, supporting source spans, and deferred reproduction requests cannot issue authoritative proof or contradiction.

---

## 🛡️ Repository integrity

Tenet treats agent execution and canonical repository state as separate concerns.

**Read-oriented workers** — Architect, Reconcile, and Assess operate in disposable detached worktrees at the exact revision they inspect; their worktrees are discarded afterward. Tenet also checks canonical repository state around these workers and fails if a supposedly read-only role unexpectedly changes it.

**Implementation workers** — Implement and Repair use leased detached worktrees. Candidate changes are inspected before acceptance, including additions, modifications, deletions, renames, generated files, executable-mode changes, and symlink changes.

**Protected paths** — Tenet always protects controller-owned project state, including the configured specification, `tenet.toml`, `AGENTS.md`, and `.tenet`. Projects can add more protected paths but cannot remove the mandatory ones.

---

## 🔐 Security boundary

> **Git worktree isolation is not a security sandbox.**

Coding agents still run under the configured ACP runtime and may inherit host filesystem, network, process, environment, credential, Docker, SSH, or cloud access. Tenet does not claim arbitrary worker execution is safe on a sensitive host.

Trusted verification is a separate boundary. It requires a dedicated remote Docker-compatible Linux daemon whose API accepts only the controller's mTLS client identity, plus a verifier image already present there by immutable digest. Tenet never connects to a local/default Docker socket and never falls back to host execution. The backend streams the named Git revision to that daemon, materializes it as a derived content-addressed image, uses no host bind mounts for candidate input, verifies the created container configuration before and after execution, and fails closed if the daemon cannot attest every requested control. No Docker socket, canonical repository path, `.tenet` state, user home, SSH/cloud credential path, or unrelated host path is mounted.

This is container isolation backed by an exclusive authenticated control plane, not a VM or protection against every hostile-kernel/container-runtime exploit. The daemon and its client-certificate admission policy are trusted infrastructure; sharing the daemon with worker-accessible credentials invalidates the boundary. Tenet does not provide confidential hidden tests, a secrets broker, Git-submodule verification, or security for ordinary coding-agent work. Trusted verifier assets are not supported in this release rather than exposing controller-side assets to unsandboxed workers.

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

A run requires an existing Git repository, at least one commit, and a clean canonical working tree. `tenet init` creates the project configuration, Tenet state directory, and a local `.tenet/config.schema.json` referenced by `tenet.toml`.

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

Public project checks and isolated trusted verifiers are distinct controller-owned mechanisms. Both use structured program/argument data; no model-provided shell string enters either authoritative path.

```toml
[verification]
timeout_secs = 300
max_output_bytes = 65536

[[verification.checks]]
name = "tests"
command = ["./test"]

[[verification.trusted_checks]]
name = "expiry-boundary"
backend = "docker"
image = "registry.example/expiry-verifier@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
program = "verify-expiry"
args = ["--assert-boundary"]
working_directory = "."
timeout_secs = 120
environment = { CI = "true", CARGO_TARGET_DIR = "/tmp/target" }
resources = { memory_bytes = 1073741824, cpu_millis = 1000, process_limit = 256, writable_tmp_bytes = 536870912 }
```

Every `verification.checks` entry is a mandatory public project gate and runs in declaration order with fail-fast behavior. A `verification.trusted_checks` entry runs during final verification only when an approved obligation contract names it exactly. Trusted names are unique, images must be digest-pinned and preloaded on the dedicated daemon, the backend is currently `docker`, and network is always disabled. Missing exclusive Docker connectivity or mTLS credentials, missing images, unsupported isolation, and infrastructure failures stop verification without issuing evidence. Complex trusted pipelines belong inside the pinned verifier image.

The trusted Docker endpoint and mTLS identity are controller-launch inputs, not repository configuration. Supply the HTTPS origin and three already-open descriptors; Tenet reads and closes the descriptors and removes their environment names while constructing the controller, before any agent operation:

```bash
TENET_TRUSTED_DOCKER_HOST=https://verifier.internal:2376 \
TENET_TRUSTED_AUTHORITY_NAMESPACE=production-password-service \
TENET_TRUSTED_DOCKER_CA_FD=3 \
TENET_TRUSTED_DOCKER_CERT_FD=4 \
TENET_TRUSTED_DOCKER_KEY_FD=5 \
tenet run 3<docker-ca.pem 4<tenet-client-cert.pem 5<tenet-client-key.pem
```

The private key is parsed directly from its descriptor into the in-process TLS client. It and `TENET_TRUSTED_AUTHORITY_NAMESPACE` are consumed before agent work begins and are never serialized into `tenet.toml`, `.tenet`, a worker workspace, a subprocess command line, or a worker environment. The namespace is not a secret, but it must be a stable, operator-controlled identifier unique to this repository authority boundary; changing it intentionally invalidates previously persisted trusted authority. The endpoint must expose only TLS, require client certificates, and authorize this controller identity as its sole mutation client for the verifier lifecycle.

### 6. Run

```bash
tenet run                              # engineering progress stream
tenet run --quiet                      # outcome-changing events only
tenet run --verbose                    # harness and worker diagnostics
tenet resume                           # continue from persisted state
```

---

## Console output

The run interface is an append-only engineering progress stream organized around cycles, work-unit lifecycles, verification, semantic gaps, progress, and the controller-owned completion gate. It works unchanged in terminals, CI, SSH sessions, and redirected logs.

```bash
tenet run > tenet.log
```

Symbols preserve meaning without color; ANSI styling is disabled when stdout is redirected and when `NO_COLOR` is set. Use `status --json` and `verify --json` for machine-readable output.

---

## 🛠️ Configuration

A new project starts with a small `tenet.toml`:

```toml
version = 1
spec_file = "spec.md"
max_cycles = 25
max_repair_attempts = 3

[verification]
checks = []
trusted_checks = []
timeout_secs = 300
max_output_bytes = 65536

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

Advanced controller, agent, verification, and execution settings are documented in [`schemas/config.schema.json`](schemas/config.schema.json). Replace the empty public `checks` list with at least one mandatory project check before running Tenet. Add `trusted_checks` only for obligations that require the isolated verifier boundary.

`agent.completion_retries = N` permits at most `N + 1` model completions per Architect batch, Reconcile operation, or Assess operation—not across the entire multi-batch Architect phase. Structured-output correction and controller semantic-validation retries consume that same per-operation budget. `max_repair_attempts = N` permits at most `N` total Repair worker invocations for a work unit across both empty-implementation recovery and candidate-verification recovery.

---

## 📎 Useful commands

```bash
# Initialize a project
tenet init

# Run or continue
tenet run
tenet resume

# Review and approve a generated requirement catalog before execution
tenet requirements dump
tenet requirements approve

# Inspect persisted state
tenet status
tenet status --json

# Run configured deterministic verification without invoking an LLM
tenet verify
tenet verify --json

# Check SQLite integrity and foreign keys
tenet db check

# Export deterministic JSON projections (exports are never authoritative)
tenet state dump --json
tenet requirements dump --json
tenet evidence dump --json
tenet evidence dump --json --requirement REQ-001
tenet roadmap dump --json

# Manage ACP agents
tenet agents list
tenet agents search <query>
tenet agents select <id>
tenet agents setup
tenet agents doctor
tenet agents login
```

### Controller-state storage and backup

Controller-generated durable state lives in `.tenet/tenet.db`. SQLite WAL mode may also create `.tenet/tenet.db-wal` and `.tenet/tenet.db-shm` while Tenet is running; all three belong to controller state. The specification and `tenet.toml` remain ordinary authoritative files.

Do not copy a live `tenet.db` file while the controller is writing. Stop Tenet first, run `tenet db check`, then copy the database with no live `-wal` file, or capture deterministic JSON projections with the dump commands above. Dump output is for inspection and recovery tooling; normal execution never reads it back.

---

## 🏗️ Architecture

Tenet is split into Rust crates with deliberate dependency boundaries.

| Crate                  | Responsibility                                                                                                                                                       |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **`tenet-domain`**     | Semantic state, IDs, evidence types, worker contracts, configuration, and pure invariants                                                                            |
| **`tenet-storage`**    | SQLite connection policy, embedded migrations, relational persistence, targeted projections, and integrity diagnostics                                                 |
| **`tenet-runtime`**    | Repository operations, workspaces, scheduling mechanisms, verification execution, and integration                                                                          |
| **`tenet-controller`** | The control loop — owns catalog trust policy, evidence trust policy, verification authorization, scheduling decisions, stopping conditions, and completion authority |
| **`tenet-acp`**        | Adapts ACP-compatible coding-agent runtimes to the controller's agent backend interface                                                                              |
| **`tenet-cli`**        | Console presentation, application composition, and command-line entry point                                                                                           |

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
