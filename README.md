# Tenet

Tenet is an agent-neutral evidence-backed completion authority that judges one exact candidate revision under one independently selected authority revision.

Your coding agent performs the engineering. An operator or CI supplies authority revision A, which defines the specification, verifier policy, and admitted contract. Tenet evaluates candidate revision R against that immutable authority snapshot.

A `DONE` verdict means only this:

> The admitted completion contract from authority revision A is satisfied for candidate revision R.

It is not a claim of universal correctness or mathematical proof unless the admitted evidence mechanism genuinely establishes that stronger property.

## Quick start

Install Tenet, initialize the repository once, then register its local stdio MCP server in the coding agent:

```bash
cargo install tenet-cli
cd /path/to/repository
tenet init
```

`tenet init` is the only user-facing CLI workflow. It defaults to `SPEC.md`; use `tenet init --spec path/to/spec.md` when the repository uses another specification. Initialization creates repository-local policy, state exclusions, and a portable Agent Skill without replacing existing specification content.

Generic MCP configuration:

```json
{
  "mcpServers": {
    "tenet": {
      "command": "tenet",
      "args": ["mcp"]
    }
  }
}
```

`tenet mcp` is the local stdio server entrypoint for agent/runtime configuration, not an interactive user command. After initialization, agents call semantic `tenet_*` tools; Tenet does not spawn its CLI or parse CLI output.

```text
Coding agent -> MCP adapter -> shared Tenet application/domain core

User -> CLI adapter -> repository initialization only
```

After an operator admits a contract, commit `.tenet/contract.json` with its policy, specification, generated Skill, and authority-owned oracle assets. The operator may then explicitly select that immutable commit as authority revision A. Local `.tenet/state.json` records observations and gate history but never defines completion.

### MCP tool surface

- `tenet_status`: inspect initialization, contract, digest, last-gate, and unresolved-obligation state without running verifiers.
- `tenet_contract_schema`: obtain the proposal schema generated from the canonical Rust type.
- `tenet_policy_schema`: obtain the verification-policy schema generated from canonical Rust types.
- `tenet_contract_propose`: validate and persist a deterministic proposal pending explicit human approval.
- `tenet_contract_approve`: admit only the exact approved proposal after revalidating its ID, digest, specification, policy, and semantics.
- `tenet_gate`: evaluate explicit `authorityRevision` and `revision` values and return a typed verdict, obligations, and blockers.
- `tenet_evidence`: return structured evidence and gate history for an exact candidate revision.

Malformed tool inputs and invalid operations are MCP tool errors. `not_done`, `inconclusive`, contradictions, and `infrastructure_error` are structured Tenet outcomes, not protocol failures. Only `done` authorizes a completion claim.

## MCP-first workflow

### 1. Initialize the repository

The target must already be a Git repository, and the specification path must resolve inside it. If the selected specification is missing, `tenet init` creates missing parent directories inside the repository and writes a starter specification. Existing specification content is never replaced. Initialization is repository-local and idempotent for the same specification. It does not alter `AGENTS.md`, `CLAUDE.md`, or global tooling configuration.

### 2. Configure verifiers

Edit `.tenet/tenet.toml`. A project verifier executes the authority-defined command in the candidate tree:

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

`environment_mode` defaults to `"ambient"`, which preserves the inherited process environment and overlays `env` plus Tenet-defined revision inputs. Set `environment_mode = "declared"` to clear the inherited environment before supplying only the verifier's configured `env` values and Tenet-defined inputs:

```toml
environment_mode = "declared"
env = { PATH = "/usr/bin:/bin", CHECK_PROFILE = "release" }
```

Declared mode gives stronger, explicit environment provenance. It is not hermetic: Tenet does not isolate the filesystem, network, toolchain, kernel, host, or other processes.

An authority-snapshot verifier keeps its executable and assets in a repository directory owned by A:

```toml
[[verifiers]]
id = "acceptance"
argv = ["verify"]
cwd = "."
timeout_seconds = 300
max_output_bytes = 65536
authority = "authority_snapshot"
oracle_path = ".tenet/oracles/acceptance"
```

For `authority_snapshot`, `oracle_path` must name a Git tree and `argv[0]` and `cwd` are relative to that bundle. Tenet materializes the bundle from A, runs the A-owned executable with the bundle as its execution root, and exposes the independent fresh candidate tree through `TENET_CANDIDATE_ROOT`. The configured executable path must directly name an executable Git file; symlink traversal is rejected so two verifier definitions cannot alias the same executable that way. Oracle scripts should read or execute candidate content only through `TENET_CANDIDATE_ROOT`; relative helpers and assets resolve from the A-owned bundle.

Verifier commands are structured argument arrays, not shell strings. Final gating reads each command definition from authority revision A, executes every distinct verifier against its own temporary materialization of candidate revision R, captures bounded output, and enforces the configured timeout.

Verifier exit semantics:

- `0`: supports every obligation bound to that verifier;
- `125`: produced no admissible evidence, so the obligation remains `missing_evidence`;
- `126`: produced an admissible but inconclusive observation;
- any other ordinary exit code: contradicts the bound obligation;
- timeout, launch failure, or termination without an exit code: infrastructure error.

`authority = "project"` identifies a repository-owned check executed from R, so candidate content may influence both implementation and oracle. `authority = "authority_snapshot"` identifies an oracle bundle loaded from A and executed with R only as explicit candidate input. It establishes provenance and candidate independence of the admitted oracle bundle; it does not establish oracle adequacy, protected execution, semantic correctness, or complete coverage of the associated claim.

An obligation may require explicit assurance of its primary oracle. Each assurance entry names one criterion and an independent `authority_snapshot` verifier:

```json
{
  "evidenceContract": {
    "claim": {
      "verifierId": "quality",
      "authority": "project"
    },
    "assurances": [
      {
        "id": "ASSURE-001",
        "criterion": "the primary oracle rejects the admitted seeded defect set",
        "verifierId": "mutation-assurance",
        "authority": "authority_snapshot"
      }
    ]
  }
}
```

The primary verifier produces claim evidence. Every listed assurance verifier must separately support its stated criterion for the exact primary `OracleIdentity`. Assurance is one level only: project verifiers cannot provide it, an oracle cannot assure itself, and an assurance entry cannot require further assurance. This is a fixed all-required contract, not a weighted or `any_of` rule system.

### 3. Propose a completion contract

The coding agent obtains the proposal schema from `tenet_contract_schema`, constructs a typed proposal, and submits it with `tenet_contract_propose`. A proposal binds claims and obligations to verifier IDs already present in repository policy; it cannot introduce executable commands.

Tenet returns an exact proposal ID and deterministic digest. `pending_approval` does not admit the contract.

### 4. Obtain explicit human approval and admit the contract

Before calling `tenet_contract_approve`, the coding agent must present:

- the exact proposal ID and digest;
- every requirement and obligation ID and statement;
- every obligation's primary verifier ID and authority mapping; and
- every oracle-assurance ID, criterion, verifier ID, and authority mapping.

The human must explicitly approve that exact proposal. The agent must not self-approve or infer approval from silence, a generic acknowledgement, continuation, or an earlier approval. `tenet_contract_approve` receives the exact `proposalId` and `proposalDigest`, then revalidates stored content plus the current specification and policy before writing `.tenet/contract.json`.

A proposal-content change creates a new digest and ID. A specification or policy change makes the proposal stale. Either change invalidates earlier approval and requires the agent to present the current proposal and request fresh explicit approval.

Commit the admitted specification, policy, contract, generated Skill, and authority-owned oracle assets. The operator—not Tenet or the coding agent—explicitly selects that immutable commit as authority revision A.

### 5. Gate an exact candidate under an exact authority

The agent calls `tenet_gate` with explicit full commit identities: `authorityRevision = A` and `revision = R`. Tenet resolves both identities to commits, requires A to be an ancestor of R, and loads the specification, admitted contract, verifier policy, and authority-snapshot oracle bundles only from A. It rejects R if `.tenet/tenet.toml`, `.tenet/contract.json`, A's configured specification path, or any configured authority-snapshot `oracle_path` differs between A and R.

Project verifiers execute inside independent detached candidate trees. Authority-snapshot verifiers execute from independent A-owned bundles against independent detached candidate trees.

The deterministic verdict is one of:

- `done`
- `not_done`
- `inconclusive`
- `infrastructure_error`

The structured result includes exact authority and candidate identities, authority control-plane digests, typed obligation states, and typed blockers. Only `done` authorizes completion. On any other verdict, the agent inspects `tenet_evidence` for exact R and continues engineering or reports the blocker; it must never weaken authority to obtain `done`.

An authority-surface change returns `authority_surface_changed`. Intentional control-plane or oracle changes require a newly admitted contract and explicit selection of a new authority revision.

### 6. Inspect status and evidence

`tenet_status` is cheap and never launches verifiers. `tenet_evidence` explains persisted observations and gate decisions for an explicit exact candidate revision without requiring direct inspection of local state files.

## Contract and evidence model

The semantic chain is deliberately small:

```text
Specification
  -> Requirement or claim
    -> Verification obligation
      -> Claim evidence contract (one primary oracle)
        -> Required oracle-assurance contracts (zero or more independent criteria)
```

Claim evidence mechanically binds:

- obligation ID;
- exact authority Git revision;
- exact candidate Git revision;
- primary verifier identity, required authority, and observed authority classification;
- primary oracle identity: project identities include the verifier, its definition digest, and exact candidate revision; authority-snapshot identities include the verifier, definition digest, A, bundle path and tree object ID, and executable blob object ID;
- specification, contract, and policy digests from the authority revision;
- supporting, contradicting, or inconclusive effect;
- repository-wide dependency surface;
- validity, captured observation, and execution provenance.

Oracle-assurance evidence is a separate artifact kind. It additionally binds the assurance ID and criterion to the exact primary `OracleIdentity` it qualifies. It supports only that admitted criterion about that oracle; it is never direct evidence for the software claim and never establishes universal oracle adequacy.

Every verifier-produced artifact records the local-process runner identity, Tenet version, OS, architecture, ambient or declared environment mode, and a deterministic execution-environment identity. That identity hashes canonicalized inputs Tenet knows or controls, including the mode, configured verifier environment, command definition, revisions, authority, and oracle identity. It deliberately does not identify or attest to the complete host environment.

Repository-wide freshness is conservative: evidence from another authority revision, candidate revision, control-plane digest, verifier authority, or oracle identity cannot satisfy an obligation. Assurance for an earlier primary oracle identity is stale. Gate execution reruns required verifiers rather than relying on cache reuse; local history is audit data only.

Primary contradiction falsifies the obligation. Primary support satisfies it only when every required assurance supports its admitted criterion. Missing or failed assurance makes the obligation unverifiable; inconclusive assurance propagates an inconclusive result; assurance runner failure propagates infrastructure error. Assurance contradiction does not become contradiction of the software claim.

## Claim-to-oracle honesty

Verifier mapping and required authority are admitted contract semantics. A project verifier proves only that the admitted project check produced the observed result against R. An authority-snapshot verifier additionally proves that the executed oracle bundle came from A and that R could not replace that committed bundle while retaining A. Oracle-assurance evidence proves only that its independent admitted verifier supported its explicitly named criterion for the exact qualified oracle identity. Neither execution mode, verifier authority, nor assurance proves that a generic test suite is a sufficient semantic oracle, that coverage is complete, that execution is hermetic, or that the associated software claim is universally correct. Operator review of claim-to-evidence and assurance mappings remains part of the trust model.

## Threat model

The authority revision supplied by the operator or CI is trusted repository control state. Coding-agent statements and candidate-selected control-plane files are epistemically untrusted. Tenet does not provide a security boundary against an arbitrary process intentionally bypassing the protocol or modifying another process's files while running under the same OS principal. `authority_snapshot` is a provenance and workflow boundary within that model, not a sandbox.

Tenet uses Git object IDs and deterministic hashes for identity and staleness binding, not authentication. The default workflow requires no persistent secret or model credential.

## Fresh clones and CI

Canonical completion semantics live in Git. A fresh clone containing authority commit A and descendant candidate commit R can launch `tenet mcp` and call `tenet_gate` with those exact full revisions. No previous local audit state, proposal state, developer credential, or coding-agent session is required. CI must act as an MCP client and supply the trusted authority SHA; the removed operational CLI commands are not compatibility paths.

## Architecture

The project is a four-crate Cargo workspace. `tenet-domain` contains pure contract validation, evidence types, and deterministic completion derivation. `tenet-application` contains repository initialization, authority Git-object reads, candidate materialization, verifier execution, and local audit persistence. `tenet-mcp` exposes typed application operations over stdio MCP. `tenet-cli` owns the `tenet` binary, including initialization output and the hidden MCP entrypoint.

Both adapters call their respective crate interfaces directly; neither duplicates domain semantics.

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
