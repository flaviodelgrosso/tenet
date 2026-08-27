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

That distinction matters. A bad specification or weak evidence contract can still describe or prove the wrong thing. Advisory model judgments cannot bridge missing evidence, select a trusted executable, mint human authority, or narrow dependency scope.

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

A verification obligation is a claim paired with an explicit evidence contract. Tenet supports explicit authoritative leaves that may be composed with deterministic `All` / `Any` contracts:

```text
Artifact(NamedProjectCheck("password-reset-expiry"))
Artifact(TrustedVerifierCheck("private-expiry-boundary"))
Artifact(FalsifierCheck("expiry-boundary-search"))
HumanAttestation("Manual UX review confirms the expiry interaction")
```

`NamedProjectCheck` binds a public project check from `verification.checks`. It runs in a disposable Git worktree. This protects canonical repository mutation, but it is not a security sandbox.

`TrustedVerifierCheck` binds a check from `verification.trusted_checks`. Its specification is controller configuration: a digest-pinned image, structured program and argument vector, repository-relative working directory, explicit environment, timeout, fixed no-network isolation policy, guest resource bounds, host archive/tree byte limits, and an input-entry limit. Agent output cannot create or modify this specification, select the image, add mounts, pass environment, or grant authority.

`FalsifierCheck` binds a controller-configured bounded search using the same Microsandbox boundary. The assessor may propose schema-validated input data, but never the image, executable, arguments, environment, network, resource limits, isolation policy, or obligation bindings. Exit `0` means only that this configured bounded search found no counterexample; exit `1` records authoritative counterevidence. Infrastructure failure records neither.

`HumanAttestation` remains pending until an explicit `tenet evidence attest` invocation signs the exact statement, obligation, catalog, revision, timestamp, attestor identity, public key, and dependency snapshot with the configured Ed25519 identity. Autonomous workers and microVM guests never receive the private key.

Controller-owned ceilings reject configuration above 16 GiB guest memory, 16 vCPUs, 4,096 processes, 16 GiB writable root storage, 1 GiB archive input, 1 GiB expanded file content, 250,000 input entries, a one-hour verifier lifetime, or 16 MiB of retained output. These are admission ceilings, not defaults; lower per-verifier limits remain fingerprinted authority policy.

The Microsandbox backend materializes the exact requested Git revision directly from raw tree and blob objects, without applying `.gitattributes` export transformations, rejects gitlinks and controller-owned `.tenet` content, hashes the deterministic raw-object archive, and copies that export into `/workspace` in a disposable local microVM. Raw object materialization keeps path-scoped dependency object IDs identical to the bytes and file presence supplied to the verifier. The guest workspace is private and writable, while the canonical repository is never mounted or exposed to the guest. Tenet uses the typed Microsandbox SDK rather than invoking or parsing the `msb` CLI or constructing shell commands. Through that SDK it requests a digest-pinned OCI image, hardware-virtualized microVM boundary, disabled networking, the restricted guest profile, a numeric non-root user, explicit CPU and memory limits, a process limit, a bounded sandbox lifetime, no secrets, no vsock, and no volumes. It validates the resulting capability report before any semantic result can become authoritative.

Before verifier execution, Tenet normalizes the runtime-resolved OCI manifest digest and requires it to equal the SHA-256 digest pinned in the controller-owned image reference. The capability report repeats this comparison at authority admission, including after persistence reload; malformed, unsupported, or mismatched digests are infrastructure failures and cannot issue semantic evidence.

After admitted execution, the controller authenticates every authoritative issuer record and artifact with an HMAC key derived from an independently supplied Tenet controller-authority key and a stable operator-assigned repository authority namespace. This includes public project checks, trusted verifiers, falsifiers, human-attestation artifacts, and cross-revision compatibility transitions. The sandbox backend and human signer never own or receive the controller identity. Artifact authentication binds the active catalog hash and serialized definitions of every bound obligation; issuer authentication binds the complete controller-observed execution payload. Reload accepts authority only when the tags, persisted issuer record, catalog context, current revision or admitted compatible revision, and current issuer configuration all agree.

The exit-code protocol is controller-defined: exit `0` supports the bound claim and exit `1` is the sole semantic contradiction status. Other statuses—including crash and signal conventions—are infrastructure failures and issue no semantic artifact. Timeouts, isolation failures, and cleanup failures likewise are not contradictions. Deterministic proof evaluation requires a current, valid, authoritative artifact from exactly the named verifier. No positive assessor verdict is required.

The authority classes are:

- **Authoritative:** exact controller-configured `NamedProjectCheck`, `TrustedVerifierCheck`, `FalsifierCheck`, and authenticated `HumanAttestation` contracts.
- **Supporting:** controller-owned immutable source inspection; it cannot satisfy an authoritative obligation contract.
- **Advisory only:** assessor judgments, counterexample proposals, and legacy reproduction requests. Proposals are admitted against controller configuration before any authoritative execution.
- **Rejected:** generic executable/project-verification obligation contracts, unknown issuers, unsigned attestations, arbitrary obligation bindings, and model-declared dependency scopes.

On repository transitions, repository-wide authority becomes stale. A trusted verifier, falsifier, or human attestor may instead declare controller-owned path globs. Tenet materializes the exact matching Git object IDs at issuance; reuse is allowed only when the complete expanded path/object set is unchanged and the catalog, contract, issuer identity, image, protocol, isolation policy, and specification fingerprints remain valid. Changed, added, removed, unknown, or malformed dependencies fail closed and trigger reacquisition when a mechanical issuer exists.

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

The controller then derives proof gaps and acquires only the exact missing configured trusted-verifier or falsifier leaves. Each admitted execution uses the local Microsandbox boundary, persists its issuer record, immediately re-derives proof, and stops as soon as deterministic proof or contradiction decides the contract. Human leaves remain explicit external actions. Assessor-proposed executables never enter this path.

### 5 · Repair

A failed advisory verification attempt becomes structured input to a new Repair worker. Trusted-verifier infrastructure failures never route to product repair. Repairs are bounded—Tenet prefers an explicit blocked state over infinite retry.

### 6 · Integrate

A verified candidate is integrated through the controller rather than being allowed to mutate canonical state directly. Tenet maintains a durable integration journal around canonical advancement. After integration, the repository is reconciled again.

### 7 · Residual advisory adjudication

Tenet calls Assess only for residual uncertainty after controller-owned acquisition. Assess may propose observations, structured falsifier inputs, and source inspections, but its judgments cannot directly mutate `ProofState`. Mechanically complete contracts finish without positive LLM authorization; authoritative contradiction blocks without assessor override.

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

Trusted verification is a separate boundary. It uses local Microsandbox microVMs with their own guest Linux kernel and host hardware virtualization. Tenet does not use Microsandbox Cloud and never falls back to host, Docker, Podman, an ordinary worktree, or another execution path. Each verifier receives only an exact controller-exported Git revision in its disposable private guest filesystem. Networking is explicitly disabled; no canonical repository path, `.tenet` state, user home, SSH/cloud credential path, host environment, secret, vsock, or volume is exposed. The verifier runs as the configured numeric non-root guest user under Microsandbox's restricted security profile with explicit CPU, memory, process, root-disk, execution-time, and sandbox-lifetime limits.

Microsandbox is a beta dependency isolated behind `tenet-runtime::TrustedVerifierRunner`. Tenet records the pinned Rust SDK version and SHA-256 identities of the resolved local `msb` and `libkrunfw` runtime files, rejecting an execution if those files change before cleanup completes. The local `msb` binary version remains unreported because the SDK does not expose it through the typed runtime API; file digests bind the observed host runtime but do not authenticate its publisher. The capability report records controller-requested and controller-observed runtime properties; it is not cryptographic remote attestation. Tenet does not claim confidential computing, hardware attestation, verifier-image signature/provenance verification, or protection against a microVM/hypervisor escape. Digest pinning establishes immutable OCI layer identity, not publisher provenance. Git submodules and network-dependent authoritative verifiers are unsupported.

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

Public project checks, isolated trusted verifiers, bounded falsifiers, and human identities are controller-owned configuration. No model-provided shell string or dependency scope enters an authoritative path.

```toml
[verification]
timeout_secs = 300
max_output_bytes = 65536

[[verification.checks]]
name = "tests"
command = ["./test"]

[[verification.trusted_checks]]
name = "expiry-boundary"
backend = "microsandbox"
image = "registry.example/expiry-verifier@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
program = "verify-expiry"
args = ["--assert-boundary"]
working_directory = "."
timeout_secs = 120
environment = { CI = "true", CARGO_TARGET_DIR = "/tmp/target" }
resources = { memory_mib = 1024, vcpus = 1, process_limit = 256, writable_root_mib = 4096, max_input_archive_bytes = 536870912, max_input_tree_bytes = 268435456, max_input_entries = 100000 }
dependencies = { policy = "paths", patterns = ["src/**", "tests/**", "Cargo.toml", "Cargo.lock"] }

[[verification.human_attestors]]
id = "alice"
publicKey = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
dependencies = { policy = "repository_wide" }
```

Every authoritative project-check, trusted-verifier, falsifier, and human artifact—including controller-admitted cross-revision compatibility—is authenticated with the controller-authority identity. The human signature remains a separate proof of explicit human action; the controller key cannot mint it. `tenet run` always requires the stable controller namespace and key descriptor; `tenet evidence dump` and `tenet evidence attest` require the same identity whenever authoritative state exists or is created.

Every `verification.checks` entry is a mandatory public project gate and runs in declaration order with fail-fast behavior. Trusted verifier and falsifier names are unique, images must be immutable OCI digest references, the production backend is `microsandbox`, and network access is disabled. Missing runtime support, timeouts, cleanup failures, malformed falsifier results, and other infrastructure failures stop acquisition without issuing semantic evidence. `dependencies` defaults to `repository_wide`; path policies are trusted only from this configuration.

The controller-authority identity is a Tenet launch input independent of Microsandbox. Supply a stable namespace and an already-open descriptor containing secret key material:

```bash
TENET_CONTROLLER_AUTHORITY_NAMESPACE=production-password-service \
TENET_CONTROLLER_AUTHORITY_KEY_FD=3 \
tenet run 3<tenet-controller-authority.key
```

Tenet reads and closes the descriptor, removes both environment names before agent work, derives the persistence authentication key in memory, and never serializes the supplied key or namespace into `tenet.toml`, SQLite, a worker workspace, a subprocess argument, the microVM, or a verifier environment. The namespace is not secret, but it must remain stable and unique to this repository authority boundary. A missing or changed identity fails closed whenever authoritative state is created or validated. `tenet evidence dump` requires the same identity when authoritative evidence exists and returns an explicit error without it.

Run the real backend acceptance test on a supported hardware-virtualization host before release. It starts a local microVM from an architecture-matched pinned Alpine manifest, verifies that the runtime-resolved manifest digest equals the controller-authorized digest, and checks the exact exported revision, non-root UID, absent planted host secret, unavailable outbound guest network, private writable workspace, authoritative-capable execution record, unchanged canonical repository, and confirmed sandbox cleanup:

```bash
TENET_MICROSANDBOX_PLANTED_SECRET=must-not-enter-guest \
cargo test -p tenet-runtime trusted_verifier::tests::local_microsandbox_acceptance_establishes_the_real_boundary -- --ignored --exact
```

An ignored or skipped result is not evidence that the Microsandbox boundary works.

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

# Explicitly sign one pending human contract; FD 3 is controller authority and FD 7 is the human Ed25519 key
TENET_CONTROLLER_AUTHORITY_NAMESPACE=production-password-service TENET_CONTROLLER_AUTHORITY_KEY_FD=3 tenet evidence attest --obligation REQ-001/AC-01/VO-01 --statement "Manual UX review confirms the expiry interaction" --attestor alice --signing-key-fd 7 3<controller.key 7<attestor.key
tenet run
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
