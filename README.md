# Tenet

Tenet is an agent-neutral evidence-backed completion authority that determines whether an exact repository revision satisfies explicit verification obligations.

Your coding agent performs the engineering. Tenet evaluates whether an exact resulting revision satisfies the repository's admitted completion contract.

A `DONE` verdict means only this:

> The admitted completion contract for this specification and verification policy is satisfied for this exact revision.

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

After an operator admits a contract, `.tenet/contract.json` is the version-controlled canonical completion contract. Local `.tenet/state.json` records observations and gate history, but never defines what `DONE` means.

The CLI is the universal protocol. Skill discovery paths and invocation syntax differ between coding-agent runtimes; Tenet correctness never depends on Skill support or automatic discovery.

## Workflow

### 1. Initialize the repository

The target must already be a Git repository, and the specification must be inside it.

```bash
tenet init --spec SPEC.md --json
```

Initialization is repository-local and idempotent for the same specification. It does not alter `AGENTS.md`, `CLAUDE.md`, or global tooling configuration.

### 2. Configure authoritative verifiers

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

Verifier commands are structured argument arrays, not shell strings. Tenet executes them in a temporary detached materialization of the selected commit, captures bounded output, and enforces the configured timeout.

Verifier exit semantics:

- `0`: supports every obligation bound to that verifier;
- `125`: produced no admissible evidence, so the obligation remains `missing_evidence`;
- `126`: produced an admissible but inconclusive observation;
- any other ordinary exit code: contradicts the bound obligation;
- timeout, launch failure, or termination without an exit code: infrastructure error.

`authority = "project"` identifies a repository-owned project check. `authority = "protected"` records that policy treats the verifier as protected from candidate control. The label is admitted policy, not proof that isolation exists. If resistance to candidate manipulation matters, the verifier logic or inputs must actually be outside the candidate-controlled revision or protected by a separately trusted mechanism.

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

Any content change produces a different digest and requires new admission. Admission writes `.tenet/contract.json`; commit it with `.tenet/tenet.toml`, the specification, and the generated Skill.

Proposal/admission separation is a domain boundary, not a same-user security sandbox.

### 5. Gate an immutable revision

Produce a candidate commit, then invoke:

```bash
tenet gate --revision <sha> --json
```

Tenet resolves the argument to an exact commit, materializes that revision, loads its admitted contract and policy, independently runs required configured verifiers, admits revision-bound observations, and deterministically derives one verdict:

- `done`
- `not_done`
- `inconclusive`
- `infrastructure_error`

Only `done` authorizes completion. The result includes the exact revision plus specification, contract, and policy digests.

A failed gate returns typed blockers. Tenet does not decide what code to write or attempt repairs; the caller owns that engineering loop.

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
- exact Git revision;
- verifier identity;
- verifier authority classification;
- specification, contract, and policy digests;
- supporting, contradicting, or inconclusive effect;
- repository-wide dependency surface;
- validity and captured observation.

Repository-wide freshness is conservative: evidence from another revision or another control-plane digest cannot satisfy an obligation. Gate execution currently reruns required verifiers rather than relying on cache reuse; local history is audit data only.

Authoritative contradiction wins over unrelated supporting evidence for the same obligation. Missing, stale, unverified, or contradictory evidence cannot produce `DONE`.

## Claim-to-oracle honesty

Verifier mapping is admitted policy. A passing generic test suite establishes exactly the evidence contract that admits it; it does not automatically become a complete semantic oracle for every associated claim. Operator review of claim-to-evidence mappings remains part of the trust model where no stronger derivation exists.

Project checks are also candidate-controlled when candidate code can modify them. Isolation alone does not prevent oracle manipulation if the candidate controls the oracle.

## Threat model

Tenet state and repository control-plane files are trusted as owned by the invoking OS user. Coding-agent statements are epistemically untrusted. Tenet does not provide a security boundary against an arbitrary process intentionally bypassing the protocol while running under the same OS principal.

Tenet uses deterministic hashes for identity and staleness binding, not authentication. The default workflow requires no persistent secret or model credential.

## Fresh clones and CI

Canonical completion semantics live in Git. A fresh clone containing `.tenet/tenet.toml`, `.tenet/contract.json`, and the bound specification can run:

```bash
tenet gate --revision <sha> --json
```

No previous local audit state or coding-agent session is required. CI can invoke the same command directly.

## Architecture

The workspace has two crates:

- `tenet-domain`: pure contract validation, evidence types, and deterministic completion derivation;
- `tenet-cli`: repository initialization, Git materialization, verifier execution, local audit persistence, and typed CLI rendering.

The product boundary is strict: coding agents own reasoning, planning, editing, delegation, branches, worktrees, tests, and repair. Tenet owns admitted contracts, independent observations, evidence validity, and revision-bound completion derivation.

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
