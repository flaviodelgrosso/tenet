<div align="center">

# Tenet

**The completion authority around your coding agent.**

[![CI](https://github.com/flaviodelgrosso/tenet/actions/workflows/ci.yml/badge.svg)](https://github.com/flaviodelgrosso/tenet/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
![Version](https://img.shields.io/badge/version-0.1.0-blue)

<br />

> A coding agent can reason, implement, test, and propose that its work is complete.
> It should not be the sole authority that declares itself **DONE**.

</div>

Tenet is an agent-neutral, deterministic completion layer. It evaluates an exact **Candidate** against an admitted **Completion Authority** using explicit **Evidence** and deterministic **Policy**, and derives one of four verdicts:

```text
Completion(Authority, Candidate, Evidence, Policy)
           → DONE | NOT_DONE | INCONCLUSIVE | INFRASTRUCTURE_ERROR
```

```text
Agent proposes
      ↓
Evidence is produced
      ↓
Policy evaluates
      ↓
Tenet declares completion
```

Tenet is **not** a coding-agent framework, an orchestrator, an LLM runtime, or a replacement for CI and test frameworks. It is the authority layer that sits around them — the component that gives “done” a meaning no agent can assert for itself.

---

## Why Tenet exists

An autonomous coding agent can write the code, generate its own tests, run those tests, review its own work, and declare the task complete. Each step is reasonable; the chain is circular. The producer is grading its own exam.

And passing tests is not the same as being done:

```text
tests pass  ≠  task complete
```

Tests can be weak, skipped, self-fulfilling, or aimed at the wrong claim. A verdict about completion needs an authority that does not move when the producer does.

Tenet breaks the circle by separating four roles:

| Role | Question it answers | Who owns it |
|---|---|---|
| **Producer** | *What did I build?* | the coding agent (or a human) |
| **Authority** | *What does “done” mean?* | the admitted Completion Authority |
| **Evidence** | *What was actually observed?* | the admitted verifiers |
| **Policy** | *Are the obligations satisfied?* | the deterministic kernel |

The producer can propose what completion means — it cannot admit that definition, evaluate the work against it, and hand back `DONE` by itself.

## The five concepts

| Concept | One-line definition |
|---|---|
| **Completion Authority** | The admitted meaning of completion: requirements, criteria, verifier definitions, evidence rules, and completion policy — captured as one immutable, content-addressed object. |
| **Candidate** | The exact immutable version of the work being evaluated — a content-addressed snapshot of your files. |
| **Evidence** | Verifier results bound to that exact Candidate and Authority. Evidence cannot be reused across identities. |
| **Policy** | The deterministic rule that decides whether the admitted obligations are satisfied. |
| **Receipt** | The auditable record of which Authority, Candidate, Evidence, Policy, and environment produced the verdict. |

## How it works

1. **Define done.** Describe the work in `SPEC.md`, then declare a completion contract: requirements, falsifiable criteria, and the verifiers that must observe each criterion.
2. **Admit the Authority.** The contract moves through a staged lifecycle — `PROPOSAL → RECONCILIATION → CLARIFICATION → ADMISSION` — into an immutable Completion Authority. Admission requires a grant minted under a trusted admission secret the producer does not hold.
3. **Build.** Implement the work. `tenet requirement check` reruns one requirement's verifiers against the current Candidate for fast, honest feedback.
4. **Verify.** `tenet verify` captures the Candidate once, runs every required verifier against a fresh copy of it, and the kernel derives the verdict from the complete evidence set.
5. **Keep the receipt.** The Final Evaluation is content-addressed; `tenet receipt verify` re-checks it later: same bytes, same verdict.

The verdicts are part of the machine contract:

| Verdict | Meaning | Exit code |
|---|---|---|
| `DONE` | Every admitted obligation is satisfied by admissible evidence for this exact Candidate | `0` |
| `NOT_DONE` | An obligation is contradicted or missing | `2` |
| `INCONCLUSIVE` | Evidence is inconclusive, or the Candidate changed during verification | `3` |
| `INFRASTRUCTURE_ERROR` | A verifier could not be executed or observed reliably | `4` |

Invalid input exits `1`. Every failure path fails closed: nothing incomplete can present itself as `DONE`.

## Quick start

Install from source (Rust stable):

```bash
git clone https://github.com/flaviodelgrosso/tenet.git
cd tenet && make install        # cargo install --path tenet-cli --locked
```

Then, in your project:

```bash
tenet init                       # .tenet/ state, SPEC.md, .mcp.json, and the Tenet Skill
# • refine SPEC.md
# • configure Candidate capture and verifiers in .tenet/tenet.toml
# • write the completion contract (requirements → criteria → verifiers)

tenet status                                            # derived phase + next action
tenet authority prepare   --contract contract.json      # PROPOSAL: pin the exact Authority
tenet authority reconcile --proposal <PROPOSAL_ID>      # RECONCILIATION

# The admission trust boundary. User approval is not an AdmissionGrant:
# the agent presents the `tenet status` admission preview through the host
# agent's native approval UX, then this trusted handoff runs in a context the
# candidate producer does not control. It derives the exact Proposal,
# Reconciliation, and Authority identities from repository state, mints the
# grant under TENET_ADMISSION_SECRET, and admits through the same kernel path.
# The agent never runs `tenet authority grant`, not even as a probe.
TENET_ADMISSION_SECRET=<hex> tenet authority admit-prepared

# Manual equivalent, when the operator drives each step explicitly:
TENET_ADMISSION_SECRET=<hex> tenet authority grant \
  --proposal <PROPOSAL_ID> --authority <AUTHORITY_ID> --json > grant.json

tenet authority admit --proposal <PROPOSAL_ID> \
  --reconciliation <RECONCILIATION_ID> --authority <AUTHORITY_ID> \
  --grant grant.json                                    # ADMISSION

# Implement before or after Admission — requirement checks and final
# verification are authoritative only under an admitted Authority:
tenet requirement check --id <REQUIREMENT_ID>           # one requirement's verifiers
tenet verify                                            # final verdict; exit code = verdict
```

`tenet doctor` validates repository integrity, the active Authority chain, and integration consistency at any point; `tenet blockers` lists what currently holds completion back.

<details>
<summary>What the completion contract looks like</summary>

A contract is a JSON document mapping requirements to criteria, and criteria to verifiers:

```json
{
  "schemaVersion": 1,
  "policy": "tenet:completion-policy:v1",
  "requirements": [
    {
      "id": "greeting-input",
      "statement": "The CLI accepts a name and generates a personalized greeting.",
      "criteria": [
        {
          "id": "greets-provided-name",
          "proposition": "Running the binary with a non-empty name exits 0 and prints a greeting containing that name.",
          "verifiers": [{ "id": "v-greeting-input-greets", "material": "candidate" }],
          "evidence": { "assurance": "local_or_stronger", "control": "candidate_controlled_permitted" }
        }
      ]
    }
  ]
}
```

Each `criteria[].verifiers[]` entry is a *reference*, not a definition: its `id` must exactly match a verifier *defined* in `.tenet/tenet.toml` — the structured command with explicit argv, environment, timeout, and exit-code dispositions — and its `material` must match that definition's authority (`candidate` or `authority_bundle`). Reference IDs are unique across the whole Contract: two Criteria cannot share one verifier, so if one observation proves both claims they are a single Criterion, and genuinely independent Criteria each get their own distinct verifier. Never rename the same command into a second config entry just to satisfy uniqueness. No implicit shell; every run is bounded and recorded.

</details>

## The Completion Authority

The Authority is where “done” stops being an opinion. An admitted Authority defines:

- **Requirements** — what must be true;
- **Criteria** — independently falsifiable proof obligations for each requirement;
- **Verifiers** — the structured commands that observe each criterion;
- **Evidence rules** — the assurance a verifier run must carry (`local` or OS-enforced `protected`) and whether candidate-controlled verification is admissible at all;
- **Completion policy** — how the kernel combines evidence into a verdict.

Once admitted, the Authority is immutable and content-addressed. Mutable refs are navigation only — pointing at an object grants it no authority. A proposal is not an admission; a reconciliation for one proposal cannot authorize another; evidence cannot cross an Authority boundary.

## Agent integration

`tenet init` writes `.mcp.json` and a Tenet Skill, so any MCP-capable coding agent can follow the protocol. The completion surface is exactly four operations:

| Operation | Purpose |
|---|---|
| `tenet_context` | Where am I? Phase, active identities, and next action — derived, never stored |
| `tenet_authority_submit` | Submit one lifecycle stage: `PROPOSAL`, `RECONCILIATION`, `CLARIFICATION`, `ADMISSION` |
| `tenet_requirement_check` | Rerun one requirement's verifiers against the current Candidate |
| `tenet_verify` | Final evaluation — the only operation that can return `DONE` |

The generated Skill and the derived `tenet_context` next action spell out the rules agents most often miss: Criteria only reference configured verifier IDs (unique across the Contract — never invent near-duplicate definitions), implementation may precede Admission while requirement checks and final verification stay authoritative only under an admitted Authority, and at `AUTHORITY_ADMISSION` the producer presents the context's structured admission preview through the host agent's native approval UX and then runs the trusted handoff `tenet authority admit-prepared` — user approval is not an `AdmissionGrant`, the producer never runs `tenet authority grant`, and after the handoff the agent resumes from the phase Tenet re-derives.

**MCP is optional.** The CLI is the canonical process surface and drives the entire lifecycle with identical application and kernel semantics. Completion never depends on a particular agent or adapter:

> Replace the coding agent tomorrow and Tenet's meaning of `DONE` stays the same.

## Trust model

What Tenet holds fixed:

- The producer of a Candidate **can propose** an Authority but **cannot admit it**. Admission requires a grant minted under a trusted admission secret the producer does not possess.
- The producer **cannot produce `DONE`**. Only the kernel's derivation over one complete Final Evaluation can.
- Missing, inconclusive, or infrastructure-failed evidence **never becomes a pass**.
- Evidence is bound to one exact Candidate. Change the tree after a successful verification and `DONE` does not carry over: `tenet verify` recaptures, reruns, and reports `CANDIDATE_CHANGED_DURING_VERIFICATION`.
- `DONE` is **deterministic**. The same Authority, Candidate, and evidence always derive the same verdict.

## What Tenet does not prove

Tenet does not claim to determine objective software correctness. Its defensible claim is:

> An exact Candidate satisfies an admitted Completion Authority, according to admissible evidence and explicit policy.

The boundaries are stated on purpose:

- A passing verifier is **evidence, not completion**.
- **Local execution is not tamper resistance.** A same-user process can interfere with `local` verifiers; `LOCAL_V1` evidence claims only detection, never confinement.
- **Protected execution raises assurance, not authorship.** An OS-enforced immutable view (`PROTECTED_V1`) confines what the verifier reads; it does not prove who wrote the code.
- **A valid grant is not a human review.** It authenticates possession of the admission secret, not a person's judgment.
- **Content addressing is not writer authentication.** A digest identifies bytes, not their producer.

## Architecture at a glance

Six Rust crates, one dependency direction:

| Crate | Responsibility |
|---|---|
| `tenet-domain` | Semantic vocabulary only |
| `tenet-kernel` | Pure, deterministic identity, admission, evidence, completion, and phase semantics |
| `tenet-application` | Use cases and infrastructure ports |
| `tenet-workspace` | Repository-contained persistence, Candidate capture, materialization |
| `tenet-runner` | Structured process execution, timeout, bounded output, provenance |
| `tenet-cli` | CLI and MCP composition root |

`domain ← kernel ← application`; `workspace` and `runner` implement application ports; `cli` composes them. Delivery code contains no completion logic.

<details>
<summary>Where state lives</summary>

All runtime state is repository-contained under `.tenet/`:

```text
.tenet/
├── objects/     immutable, SHA-256 content-addressed protocol objects and tree manifests
├── blobs/       immutable, content-addressed file bytes of captured snapshots
├── refs/        mutable navigation pointers only
├── tenet.toml   project configuration (capture + verifiers)
└── tmp/         disposable staging
```

A Candidate is a normalized manifest of the repository's regular files (`candidate.include` in `.tenet/tenet.toml` defines the surface; `.tenet/**` is excluded). Workflow phase is derived from persisted facts, never stored. Missing, corrupt, or unknown-version state fails closed.

</details>

The full protocol, trust model, and residual limitations are documented in [`AGENTS.md`](AGENTS.md).

## Development

```bash
make ci        # formatting, compilation, Clippy (warnings denied), all tests
```

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md).

## License

MIT — see [LICENSE](LICENSE).
