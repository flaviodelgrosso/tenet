# Tenet

Tenet is an agent-neutral evidence-backed completion authority that judges one exact candidate revision under one independently selected authority revision.

Your coding agent performs the engineering. An operator or CI supplies authority revision A, which defines the specification, verifier policy, and admitted contract. Tenet evaluates candidate revision R against that immutable authority snapshot.

A `DONE` verdict means only this:

> The admitted completion contract from authority revision A is satisfied for candidate revision R.

It is not a claim of universal correctness or mathematical proof unless the admitted evidence mechanism genuinely establishes that stronger property.

## Quick start

```bash
cargo install tenet-cli

cd my-project
tenet init --spec SPEC.md

# Configure authoritative verifiers in .tenet/tenet.toml.
# Start your coding agent normally.
# Explicitly invoke the repository's Tenet Skill using that agent's native Skill UX.
```

`tenet init` creates:

```text
.tenet/
  tenet.toml       version-controlled specification and verifier policy
  .gitignore       excludes local audit and proposal state
.agents/skills/tenet/
  SKILL.md         optional, explicitly invoked Agent Skills workflow
```

After an operator admits a contract, commit `.tenet/contract.json` with its policy and specification. That commit can then be selected as an authority revision. Local `.tenet/state.json` records observations and gate history, but never defines what `DONE` means.

The CLI is the universal protocol. Skill discovery paths and invocation syntax differ between coding-agent runtimes; Tenet correctness never depends on Skill support or automatic discovery.

## Workflow

### 1. Initialize the repository

The target must already be a Git repository, and the specification must be inside it.

```bash
tenet init --spec SPEC.md --json
```

Initialization is repository-local and idempotent for the same specification. It does not alter `AGENTS.md`, `CLAUDE.md`, or global tooling configuration.

### 2. Configure project verifiers

Edit `.tenet/tenet.toml`:

```toml
version = 1
spec_path = "SPEC.md"

[[verifiers]]
id = "quality"
argv = ["make", "ci"]
cwd = "."
timeout_seconds = 300
max_output_bytes = 65536
authority = "project"
```

Verifier commands are structured argument arrays, not shell strings. Final gating reads the command definition from authority revision A, executes it in a temporary detached materialization of candidate revision R, captures bounded output, and enforces the configured timeout.

Verifier exit semantics:

- `0`: supports every obligation bound to that verifier;
- `125`: produced no admissible evidence, so the obligation remains `missing_evidence`;
- `126`: produced an admissible but inconclusive observation;
- any other ordinary exit code: contradicts the bound obligation;
- timeout, launch failure, or termination without an exit code: infrastructure error.

`authority = "project"` is the only currently executable authority. It identifies a repository-owned check observed by Tenet's ordinary local process runner; it does not claim protected execution or resistance to candidate-controlled oracle manipulation.

### 3. Propose a completion contract

Get the proposal schema generated from Tenet's Rust request type:

```bash
tenet contract schema --json
```

A proposal binds claims and obligations to verifier IDs already present in repository policy. It cannot introduce executable commands.

```bash
tenet contract propose --file proposal.json --json
```

Tenet returns a proposal ID and deterministic digest. A coding agent may prepare and submit this proposal, but must stop for operator admission.

### 4. Admit the exact proposal

The operator admits the exact ID and digest:

```bash
tenet contract approve \
  --proposal proposal-0123456789abcdef \
  --digest sha256:... \
  --json
```

Any content change produces a different digest and requires new admission. Admission writes `.tenet/contract.json`; commit it with `.tenet/tenet.toml`, the specification, and the generated Skill. Select that exact commit as the new authority revision before gating descendants that intentionally change the control plane.

Proposal/admission separation is a domain workflow boundary under the default same-user trust model, not a security sandbox.

### 5. Gate a candidate under an authority revision

The operator or CI selects trusted authority commit A. Produce candidate commit R, then invoke:

```bash
tenet gate \
  --authority-revision <authority-sha> \
  --revision <candidate-sha> \
  --json
```

Tenet resolves both arguments to exact commits, requires A to be an ancestor of R, and loads the specification, admitted contract, and verifier policy only from A. It rejects R if `.tenet/tenet.toml`, `.tenet/contract.json`, or A's configured specification path differs between A and R. Verifiers defined by A execute against R's detached candidate tree.

The deterministic verdict is one of:

- `done`
- `not_done`
- `inconclusive`
- `infrastructure_error`

Only `done` authorizes completion. The result includes exact `authorityRevision` and `revision` identities plus specification, contract, and policy digests from A.

An authority-surface change returns `authority_surface_changed`; intentional control-plane changes require admission and selection of a new authority revision. Other failed gates return typed blockers. Tenet does not decide what code to write or attempt fixes; the caller owns that engineering loop.

### 6. Inspect state and evidence

```bash
tenet status --json
tenet evidence --revision <sha> --json
```

`status` is cheap and never launches verifiers. `evidence` explains persisted observations and gate decisions without requiring direct inspection of local state files.

## Contract and evidence model

The semantic chain is deliberately small:

```text
Specification
  -> Requirement or claim
    -> Verification obligation
      -> Evidence contract (configured verifier ID)
```

Evidence mechanically binds:

- obligation ID;
- exact authority Git revision;
- exact candidate Git revision;
- verifier identity and implemented authority classification;
- specification, contract, and policy digests from the authority revision;
- supporting, contradicting, or inconclusive effect;
- repository-wide dependency surface;
- validity and captured observation.

Repository-wide freshness is conservative: evidence from another authority revision, candidate revision, or control-plane digest cannot satisfy an obligation. Gate execution currently reruns required verifiers rather than relying on cache reuse; local history is audit data only.

Authoritative contradiction wins over unrelated supporting evidence for the same obligation. Missing, stale, unverified, or contradictory evidence cannot produce `DONE`.

## Claim-to-oracle honesty

Verifier mapping is admitted policy. A project verifier proves only that the admitted project check produced the observed result against R. A passing generic test suite does not automatically become a complete semantic oracle for every associated claim, and candidate code may still influence a project-owned oracle. Operator review of claim-to-evidence mappings remains part of the trust model.

## Threat model

The authority revision supplied by the operator or CI is trusted repository control state. Coding-agent statements and candidate-selected control-plane files are epistemically untrusted. Tenet does not provide a security boundary against an arbitrary process intentionally bypassing the protocol while running under the same OS principal.

Tenet uses deterministic hashes for identity and staleness binding, not authentication. The default workflow requires no persistent secret or model credential.

## Fresh clones and CI

Canonical completion semantics live in Git. A fresh clone containing authority commit A and descendant candidate commit R can run:

```bash
tenet gate \
  --authority-revision <trusted-base-sha> \
  --revision <candidate-sha> \
  --json
```

No previous local audit state, proposal state, developer credential, or coding-agent session is required. CI supplies the trusted authority SHA and invokes the same universal CLI directly.

## Architecture

The workspace has two crates:

- `tenet-domain`: pure contract validation, evidence types, and deterministic completion derivation;
- `tenet-cli`: repository initialization, authority Git-object reads, candidate materialization, verifier execution, local audit persistence, and typed CLI rendering.

The product boundary is strict: coding agents own reasoning, planning, editing, delegation, branches, worktrees, tests, and fixes. Tenet owns authority snapshots, admitted contracts, independent observations, evidence validity, and deterministic completion derivation for exact `(A, R)` pairs.

## Development

```bash
make fmt-check
make check
make clippy
make test
# or
make ci
```

No test requires a coding agent or paid API.
