# Tenet

Tenet is an agent-neutral, CLI-first completion authority for exact content identities. It decides one claim:

> The exact Candidate identified by `CandidateId C` satisfies the completion contract carried by the exact Authority identified by `AuthorityId A`, under the exact active `AdmissionId`.

Tenet persists immutable objects and derives workflow and completion state. Agent statements, mutable refs, verifier exit codes, and generated integrations do not decide completion.

## CLI

The CLI is the canonical process surface. It drives the complete lifecycle with the same application use cases and kernel semantics as MCP; MCP is an optional adapter.

```text
tenet init [--spec PATH] [--json]
tenet status [--json]
tenet authority prepare --contract FILE [--issues FILE] [--json]
tenet authority reconcile --proposal PROPOSAL_ID [--findings FILE] [--json]
tenet authority clarify --proposal PROPOSAL_ID --text TEXT [--json]
tenet authority grant --proposal PROPOSAL_ID --authority AUTHORITY_ID [--json]
tenet authority admit --proposal PROPOSAL_ID --reconciliation RECONCILIATION_ID --authority AUTHORITY_ID --grant FILE [--json]
tenet authority inspect [--json]
tenet requirement check --id REQUIREMENT_ID [--json]
tenet verify [--json]
tenet blockers [--json]
tenet evidence [--requirement REQUIREMENT_ID] [--json]
tenet receipt verify --id EVALUATION_ID [--json]
tenet doctor [--receipt EVALUATION_ID] [--json]
tenet mcp
tenet version
```

`tenet init` creates repository-contained state, a starter `SPEC.md` when needed, MCP configuration, and the Tenet Skill. `tenet doctor` validates repository root discovery, `SPEC.md`, repository format, object/blob/ref integrity, the active Admission chain, supported semantic versions, repository-write scope, and integration consistency.

Exit codes are part of the machine contract: `0` success or `DONE`, `1` invalid input or error, `2` `NOT_DONE`, `3` `INCONCLUSIVE`, `4` infrastructure failure. `tenet doctor --receipt <EvaluationId> --json` (and `tenet receipt verify`) verifies a canonical Final Evaluation receipt and its referenced Admission, Authority, Candidate, contract, policy, evidence set, and execution-environment identities; an incomplete receipt exits `2`.

`tenet authority grant` is the trusted-side operation. It requires the trusted admission secret in the process environment (`TENET_ADMISSION_SECRET`, hexadecimal, at least 32 bytes) and is intentionally absent from the MCP surface. The candidate producer normally never holds this secret.

## Four-operation protocol

The MCP completion-domain surface is exactly:

```text
tenet_context
tenet_authority_submit
tenet_requirement_check
tenet_verify
```

### `tenet_context`

Call this first. It derives, rather than persists:

- phase;
- active `AdmissionId`, `AuthorityId`, and `CompletionPolicyId`;
- current `CandidateId` when available;
- Requirement-check status;
- the next action.

Supported phases are `SPEC_REQUIRED`, `AUTHORITY_REQUIRED`, `AUTHORITY_RECONCILIATION`, `AUTHORITY_CLARIFICATION`, `AUTHORITY_ADMISSION`, `AUTHORITY_STALE`, `INCOMPATIBLE`, `IMPLEMENTATION`, and `COMPLETED`.

`COMPLETED` requires a successful Final Evaluation bound to the active Admission and Authority, kernel state `satisfied`, and a freshly captured current Candidate equal to the Evaluation Candidate.

### `tenet_authority_submit`

Submit one exact lifecycle stage:

```text
PROPOSAL → RECONCILIATION → CLARIFICATION (when needed) → ADMISSION
```

A Proposal captures the specification, `CompletionContractV1`, policy, and authority-owned verifier material into an immutable Authority. Reconciliation binds one exact Proposal. Clarification records information without admitting anything. Admission binds the exact Proposal, Reconciliation, and Authority **and a trusted admission grant**. A ref has no authority independent of the referenced immutable object and validated chain.

`ADMISSION` requires an `AdmissionGrant` bound to the exact proposal and authority identities. The grant's mac is verified by the kernel under the trusted admission secret, which the candidate producer cannot possess; a process without the secret fails closed with `admission_secret_unavailable`. The grant is embedded in the immutable Admission object, so it is part of the Admission's content identity and cannot be swapped afterwards.

Example request shape:

```json
{
  "submission": {
    "stage": "ADMISSION",
    "proposalId": "sha256:…",
    "reconciliationId": "sha256:…",
    "authorityId": "sha256:…",
    "grant": {
      "schemaVersion": 1,
      "semantics": "tenet:admission-grant:v1",
      "proposal": "sha256:…",
      "authority": "sha256:…",
      "mac": "<64 lowercase hex>"
    }
  }
}
```

### `tenet_requirement_check`

A Requirement check:

1. loads the active Admission and derives its Authority;
2. captures current Candidate `C`;
3. runs every verifier for one Requirement;
4. gives every verifier a fresh materialization of `C` and fresh scratch directory;
5. persists one Requirement-scoped Evaluation;
6. derives the Requirement result in the kernel;
7. updates the Requirement ref.

This evidence is Candidate-specific development feedback. It never becomes Final evidence and this operation cannot return protocol-level `DONE`.

### `tenet_verify`

Final verification:

1. loads the exact active Admission and Authority;
2. requires supported `CompletionPolicyV1` and Runner/Candidate semantics;
3. captures `Cfinal` once;
4. gives every verifier a fresh materialization of `Cfinal` and a fresh scratch directory;
5. terminates the verifier process group and rejects a run if its Candidate or Authority materialization changed;
6. persists one Final Evaluation containing the exact Admission, Authority, Candidate, contract, completion-policy, verifier, oracle, execution, and result bindings;
7. derives every evidence disposition, Criterion, Requirement, and Authority outcome in the kernel.

Only `tenet_verify` can return `DONE`. Its response includes the complete persisted Evaluation and deterministic per-verifier dispositions (`observed`, `missing`, or `rejected_assurance`). A successful Final `EvaluationId` is the canonical `LOCAL_V1` receipt identity; there is no competing receipt object. Verify it later with `tenet doctor --receipt <EvaluationId> --json`. The receipt response includes the Authority, Candidate, contract digest, completion-policy identity, evidence-set digest, and execution-environment identities.

Receipt verification re-derives every run result from the admitted verifier definition's exit-code policy, recomputes each verifier definition digest, and compares Authority-bundle oracle references against the immutable sealed surface manifest. A newly serialized Evaluation containing a contradictory `result` or fabricated oracle reference is rejected rather than accepted because it is content-addressed.

After a successful Evaluation for `C1`, Tenet captures the working tree again. If it is now `C2`, Tenet preserves the successful historical Evaluation for `C1` but returns `INCONCLUSIVE`, reason `CANDIDATE_CHANGED_DURING_VERIFICATION`, and both Candidate identities. Evidence for `C1` is never implied to cover `C2`.

## Persistence

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

Objects and blobs are addressed by SHA-256 content identity. Refs are mutable navigation pointers only. `.tenet/tmp` and `.tenet/lock` are disposable; workflow phase is not stored.

`tenet:candidate-semantics:v1` identifies a sorted manifest of normalized repository-relative regular-file paths, content IDs, and executable bits. `.tenet/**` and repository metadata are excluded from Candidate capture. Missing, corrupt, noncanonical, unknown-version, or unknown-semantics content fails closed.

## Completion and evidence policy

`CompletionPolicyV1` requires the exact verifier set for the Evaluation scope. Every run binds the exact Admission, Authority, Candidate, contract digest, completion-policy identity, verifier, and typed oracle identity. The kernel rejects duplicate, missing, extra, cross-Admission, cross-Authority, cross-Candidate, cross-contract, cross-policy, oracle-mismatched, and internally inconsistent provenance. Assurance and evidence-control requirements participate in every Criterion result. Candidate-controlled verification is admissible only when the Authority contract explicitly permits it.

The local runner uses structured argv, typed Candidate/Authority/scratch/output paths, explicit environment inheritance, timeouts, bounded output, and `RunnerSemanticsV1`. It invokes no implicit shell. Its environment identity covers the resolved executable digest, admitted command, effective inherited environment digests, typed oracle identity, Tenet subjects, runner version, OS, architecture, and protection backend. It terminates the verifier process group before accepting output.

Admitted verifier definitions carry a `protection` level:

- `local` executes without an enforcement boundary and yields `LOCAL_V1` (detection only).
- `protected` executes only against a privately staged view whose immutability the operating system enforces against other processes, with a separate writable scratch directory and controlled output directory, and yields `PROTECTED_V1`. On macOS the workspace stages the exact Candidate and Authority surfaces into a disk image mounted read-only through DiskArbitration and then unlinks the image, so the kernel's mount is the only reference to the bytes and a same-user producer cannot write, swap, or re-attach what the verifier reads; the view's identity and content are re-verified before the verifier runs and after it exits. On Linux the runner builds a private copy inside a fresh Bubblewrap namespace's tmpfs strictly from trusted expectations delivered over path-unreachable file descriptors, verifies every byte and mode inside that namespace, and only then execs the verifier. When the platform cannot enforce the boundary, the runner returns an explicit infrastructure result and never downgrades to `LOCAL_V1`. `TMPDIR` is redirected into the run's scratch directory for protected runs.

## Trust boundaries

These distinctions are mandatory:

- **`LOCAL_V1` ≠ same-user tamper resistance.** A same-user process can affect local execution; `LOCAL_V1` makes no stronger claim.
- **`PROTECTED_V1` ≠ independent authorship.** It enforces the runtime observation boundary for verifier processes; it does not prove who wrote the Candidate.
- **a valid grant ≠ a human decision.** The admission grant authenticates possession of the trusted secret, not a specific person; the trust anchor is the secret's confidentiality in the operator process.
- **`AuthorityBound` ≠ independent authorship.** Binding verifier material to Authority identifies content; it does not prove who wrote it.
- **fresh materialization ≠ sandboxing.** Each verifier gets a pristine view; only `protected` adds the OS enforcement boundary, and a repository-internal directory is never that boundary because a same-user producer can transiently mutate and restore it.
- **content addressing ≠ writer authentication.** A digest identifies bytes; it does not authenticate their producer.
- **MCP user input ≠ cryptographic human identity.** Admission is an explicit workflow boundary, not a signature scheme.
- **verifier `Pass` ≠ task completion.** Only deterministic kernel evaluation of the full admitted Final Evaluation can yield `DONE`.

## Residual limitations

- **The admission trust anchor is a shared secret.** `ADMISSION` now requires a grant the producer cannot mint without the trusted admission secret, so protocol-level self-admission is closed. Possession of `TENET_ADMISSION_SECRET` is the only authorization: a process that obtains the secret can mint grants. The secret is a single shared capability with no per-identity revocation; rotation requires re-admission.
- **Stored-state integrity is not writer-authenticated.** Content addressing authenticates bytes, not writers. A same-user process with direct `.tenet/` write access can hand-craft a persisted chain whose grant mac is structurally well-formed but was never issued under the trusted secret. Every load that can influence verification or `DONE` now revalidates the grant mac under `TENET_ADMISSION_SECRET` and fails closed with `admission_grant_invalid`, so forged state cannot complete; a process without the secret cannot derive completion at all (`admission_secret_unavailable`). Informational reads (context, inspect, doctor, evidence) verify the grant's structural binding and surface the failure as incompatibility rather than admission.
- **`LOCAL_V1` is detection, not confinement.** Fresh materializations, process-group cleanup, per-view recapture, and final working-tree recapture detect ordinary mutation and prevent sequential contamination. A hostile same-user process can still race or tamper with local files; a mutate-and-restore race is provably defeatable only by the `protected` OS boundary. Such evidence cannot satisfy `Protected` criteria.
- **Protected enforcement is platform-dependent.** Where neither Seatbelt nor Bubblewrap exists, protected verification fails closed with an infrastructure result; it never silently degrades. Unprivileged-container Bubblewrap restrictions are the operator's deployment concern. A same-user attacker can always `hdiutil detach` (macOS) or kill the sandboxed process (Linux) as pure denial of service; Tenet detects the substitution or loss and returns infrastructure rather than evidence, but cannot prevent the interruption. The macOS mountpoint identity (volume device, root inode, source device) is re-checked after the run so a detach-and-remount substitution at the same path is rejected; residual kernel fsid/device recycling within one verification window is considered negligible and unprovable at same-user assurance.

- **Descendants can escape process-group cleanup.** Tenet places verifiers in a dedicated group and sends `SIGKILL` to that group, but a descendant that starts a new session can retain an inherited output pipe. Output collection now fails closed after a five-second drain deadline; the surviving process itself is outside the same-user local assurance boundary. Protected runs additionally confine such descendants by the OS sandbox.

## Architecture

The workspace has exactly six crates:

- `tenet-domain`: semantic types and errors;
- `tenet-kernel`: pure identity, admission, policy, phase, and completion derivation;
- `tenet-application`: protocol use cases and infrastructure ports;
- `tenet-workspace`: repository-contained persistence and materialization;
- `tenet-runner`: process execution and provenance;
- `tenet-cli`: CLI and MCP composition root.

Dependency direction is enforced by tests: `domain ← kernel ← application`, with `workspace` and `runner` implementing application ports and `cli` composing them.

## Development

```bash
make ci
```

This checks formatting, compilation, Clippy with warnings denied, and all deterministic offline tests.
