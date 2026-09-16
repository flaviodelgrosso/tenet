# AGENTS.md

## Product boundary

Tenet is an agent-neutral, CLI-first completion authority for exact content identities.

The coding agent owns investigation, planning, editing, tests, and blocker responses. Tenet owns immutable authority state, admitted contract semantics, verifier observations and provenance, Candidate capture, and deterministic completion derivation.

No agent statement, model output, generated Skill, mutable ref, or verifier exit code is itself a completion decision. Only `tenet_verify` may return protocol-level `DONE`, derived by the kernel from one Final Evaluation.

## Final protocol

Expose exactly four completion-domain MCP operations:

```text
tenet_context
tenet_authority_submit
tenet_requirement_check
tenet_verify
```

The CLI is the canonical process surface and drives the complete lifecycle with identical application and kernel semantics: `init`, `status`, `authority prepare|reconcile|clarify|grant|admit|inspect`, `requirement check`, `verify`, `blockers`, `evidence`, `receipt verify`, `doctor`, `mcp`, `version`. Exit codes distinguish success/`DONE` (0), invalid input (1), `NOT_DONE` (2), `INCONCLUSIVE` (3), and infrastructure failure (4). Do not reintroduce public propose/approve/seal/select/capture/gate workflows, and do not add completion semantics to the CLI beyond the shared application use cases.

`tenet_context` derives phase from persisted facts. Never persist workflow phase. `COMPLETED` requires a successful Final Evaluation for the active Admission and Authority whose Candidate equals a fresh current capture.

`tenet_authority_submit` implements `PROPOSAL`, `RECONCILIATION`, `CLARIFICATION`, and `ADMISSION`. Every transition binds exact identities. Clarification never admits. Admission validates the complete exact chain and requires an `AdmissionGrant` bound to the exact proposal and authority. The grant mac is verified by the kernel under the trusted admission secret (`TENET_ADMISSION_SECRET`, hex, at least 32 bytes) held only by the trusted operator process; grant minting is CLI-only and never exposed through MCP. A process without the secret fails closed with `admission_secret_unavailable`. Every load path that can influence verification or completion revalidates the persisted grant mac under the trusted secret, so a hand-edited or forged Admission in repository state fails closed with `admission_grant_invalid`; informational loads (context, inspect, doctor, evidence) enforce the grant's structural binding (semantics version and exact proposal/authority identity) and surface an invalid chain as incompatibility rather than admission.

`tenet_requirement_check` captures one Candidate, reruns all verifiers for one Requirement using a fresh Candidate view per verifier, persists a Requirement-scoped Evaluation, and updates its ref. It cannot establish terminal completion.

`tenet_verify` captures the final Candidate once, reruns every required verifier with a fresh Candidate view per verifier, persists one Final Evaluation, and delegates all completion semantics to the kernel. Requirement-check runs are never promoted. A successful Final `EvaluationId` is the `LOCAL_V1` receipt identity; do not add another receipt type.

After a successful Final Evaluation, recapture current content. If the Candidate changed, preserve the historical Evaluation but return `INCONCLUSIVE` with `CANDIDATE_CHANGED_DURING_VERIFICATION`, the verified Candidate, and the current Candidate. Never apply historical evidence to the new state.

## Authority and Candidate identities

Authority and Candidate are distinct typed, content-addressed identities. The active Authority is derived only by loading `.tenet/refs/active-admission` and validating the referenced immutable Admission, Proposal, Reconciliation, Authority, and SpecSnapshot chain.

A Proposal is not Admission. A mutable ref has no authority independent of its referenced immutable object. Reconciliation for one Proposal cannot authorize another. Admission for one Authority cannot authorize another.

Candidate capture uses policy from the admitted immutable Authority, never live mutable policy. `tenet:candidate-semantics:v1` and all Tenet-owned format versions remain version `1` during the unreleased MVP. Unknown versions and semantics fail closed; do not add migrations or compatibility branches.

## Evidence and completion

Every `VerifierRun` binds exact Authority and Candidate subjects, captured observation, execution context, Runner semantics, assurance, and provenance. Final Evaluation contains exactly one run for every required verifier and no duplicates or extras.

Completion must fail closed for missing, stale, cross-subject, duplicate, inadmissible, contradictory, inconclusive, infrastructure-failed, or unknown-semantic evidence. Assurance requirements participate in completion. `LOCAL_V1` cannot satisfy `Protected`.

Candidate-controlled evidence is admissible only when explicitly permitted by the Authority's CompletionContract. Verifier definitions come from immutable Authority, while Candidate inputs come from the exact captured Candidate.

## Trust distinctions

Never collapse these boundaries:

- `LOCAL_V1` is not same-user tamper resistance.
- `PROTECTED_V1` is not independent authorship.
- a valid admission grant is not a cryptographic human identity; it authenticates possession of the trusted secret.
- `AuthorityBound` is not independent authorship.
- fresh materialization is not sandboxing; only `protected` verifier protection adds the OS enforcement boundary.
- content addressing is not writer authentication.
- MCP user input is not cryptographic human identity.
- verifier `Pass` is not task completion.

Admitted `VerifierSpec.protection` is either `local` (`LOCAL_V1`) or `protected` (`PROTECTED_V1`). Protected execution must run only against a privately staged view whose immutability the OS enforces against other same-user processes — macOS: a DiskArbitration read-only volume whose backing image is unlinked after attach, with the mount identity and admitted content reverified before and after the run; Linux: a private Bubblewrap-namespace tmpfs copy built from trusted expectations delivered over path-unreachable file descriptors and byte-verified inside the namespace before exec — with separate writable scratch and a controlled output directory, `TMPDIR` redirected into scratch, and an explicit infrastructure result when the platform cannot enforce the boundary; never downgrade assurance. Repository-internal fresh materialization is never the protected boundary: a same-user producer can transiently mutate and restore it, defeating detection-after-the-fact. Keep the admission trust anchor the smallest mechanism that works: one HMAC capability grant verified deterministically by the kernel on submission and on every verification-influencing load. Do not add PKI, signatures, keychains, privileged services, or mandatory containers to imply guarantees the same-user local boundary does not provide.

## Persistence

Runtime state is repository-contained:

```text
.tenet/
├── format
├── .gitignore
├── objects/
├── blobs/
├── refs/
│   ├── proposal
│   ├── reconciliation
│   ├── active-admission
│   ├── final
│   └── requirements/
├── tmp/
└── lock
```

Objects and blobs are immutable and content-addressed. Refs are mutable navigation only. All writes must remain beneath the repository root and reject symlink/path escape. Phase is derived, never stored.

`tenet doctor` validates repository root, `SPEC.md`, format, object/blob/ref integrity, active Admission chain, supported semantic versions, repository-write scope, and integration consistency.

## Domain and validation

Rust domain types are the source of truth. Derive serialization and JSON Schema where practical. Keep layers distinct:

```text
syntax → schema/Serde → domain invariants → repository/runtime invariants
```

Use semantic ID newtypes where identity confusion matters. Use `thiserror` for distinguishable domain errors and `anyhow` at CLI/I/O boundaries. Avoid `unwrap` and `expect` in production code.

Breaking changes are allowed during MVP. Make clean cutovers: update every caller, test, fixture, generated integration, and active document; remove obsolete variants and competing paths. All Tenet-owned schema and format versions remain `1` until public release.

## Six-crate architecture

The workspace contains exactly:

- `tenet-domain`: semantic vocabulary only;
- `tenet-kernel`: pure deterministic identity, admission, evidence, completion, and phase semantics; depends only on domain;
- `tenet-application`: use cases and repository/runner ports; no filesystem, process, or persistence work;
- `tenet-workspace`: repository, object/blob/ref persistence, capture, and materialization;
- `tenet-runner`: structured process execution, timeout, bounded output, and provenance;
- `tenet-cli`: CLI and MCP composition root.

Dependency direction is `domain ← kernel ← application`; workspace and runner implement application ports and never depend on each other; CLI composes all layers. Delivery code contains no completion logic.

Prefer existing files and direct primitives. Do not add provider integrations, model runtimes, plugin systems, databases, generic rule engines, or speculative traits. Structured verifier commands use explicit argv, cwd, environment, timeout, and bounded output without an implicit shell.

## Testing

Architectural changes require deterministic offline adversarial tests. Preserve coverage for:

- producer assertions cannot create `DONE`;
- a producer without the trusted admission secret cannot mint a grant or admit;
- a grant cannot admit across proposal or authority identities, a tampered mac cannot admit, and unknown grant semantics fail closed on load;
- candidate-controlled verifier trust requires explicit Authority policy;
- evidence cannot transfer across Candidate or Authority identities;
- reconciliation and admission cannot transfer across identities;
- missing or duplicate runs cannot hide missing evidence;
- `LOCAL_V1` cannot satisfy `Protected`;
- a protected verifier cannot write the Candidate or Authority view, scratch and output stay writable, and a protected run without a capable backend returns infrastructure failure rather than `PROTECTED_V1`;
- every verifier gets a fresh Candidate view;
- mutation during final verification cannot produce `DONE` for the new state;
- unknown semantic versions fail closed;
- CLI and MCP cannot redefine kernel completion semantics; the CLI completes the full lifecycle without MCP and derives identical verdicts.

Before completion, run:

```bash
make ci
```

This checks formatting, compilation, Clippy with warnings denied, and all tests.

## Working style

1. Inspect domain types, kernel semantics, application flow, adapters, and callers before editing.
2. Preserve the Admission/Authority/Candidate/Evaluation identity split at every interface.
3. Reuse established patterns; keep changes materially small.
4. Update all affected callers and tests in one clean cutover.
5. Verify focused adversarial behavior, then run full CI.
6. Report any unverifiable invariant; never weaken completion semantics to obtain green output.
