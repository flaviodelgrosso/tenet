# Tenet

Tenet is an agent-neutral, evidence-backed completion authority for immutable content.

It decides one narrow claim:

> Exact Candidate Snapshot R satisfies the admitted completion contract under exact independently sealed Authority Capsule A.

Tenet knows content, not source-control history. It requires no Git repository, VCS executable, commit graph, branch, or ancestry relation.

## Trust model

- **Authority Capsule A** contains only the sealed specification, verification policy, admitted contract, and authority-snapshot oracle bundles. Its `authorityId` is a Tenet SHA-256 content identity.
- **Candidate Snapshot R** contains the captured candidate project content. Its `candidateId` is a distinct typed SHA-256 content identity.
- A coding agent never selects A. After `tenet_authority_seal`, a human reviews the returned exact `authorityId` and explicitly selects it.
- The gate reads authority material only from A and candidate content only from R. Mutable workspace content is not an input once snapshots exist.
- Local audit history is observation only; it never establishes completion.

`done` means every admitted obligation reached `contract_satisfied` from admissible current evidence. It does not claim that an oracle is adequate, protected, isolated, or universally correct.

## Workflow

```text
tenet init
  ↓
inspect policy and specification
  ↓
tenet_contract_propose
  ↓
human explicitly approves exact proposal
  ↓
tenet_contract_approve
  ↓
tenet_authority_seal → authorityId A
  ↓
human explicitly selects A
  ↓
engineering
  ↓
tenet_candidate_capture → candidateId R
  ↓
tenet_gate({ authorityId: A, candidateId: R })
```

A successful proposal is not an authority capsule. Modifying authority source later requires a new proposal/approval where stale, a new sealed A, and fresh human selection. Candidate implementation present in the workspace is not included in A because sealing captures only the authority surface.

## Initialization

`tenet init` works from an ordinary directory. It initializes that directory as the project root and writes `.tenet/tenet.toml`, a starter specification when necessary, an MCP entry, and a local Tenet Skill. Later operations find the nearest enclosing `.tenet/tenet.toml`; nested initialized roots therefore resolve deterministically to the nearest root.

The admitted policy's `candidate.root` selects the project-relative Candidate Snapshot capture root. Its `candidate.exclude` rules are deterministic exact paths or directory rules ending in `/**`; Tenet administration and common source-control administration remain excluded by default. Candidate capture uses this policy from the explicitly selected authority capsule, never the mutable current policy.

## Snapshot semantics

The local content store captures deterministic directory trees. A snapshot identity binds sorted normalized relative paths, entry kind, regular-file SHA-256 content digests, and executable state. Timestamps, ownership, inode identity, and storage location do not affect it.

Symlinks and special filesystem entries are rejected. Capture never follows an entry outside its root. Objects are integrity-checked against their canonical manifest whenever loaded; missing or corrupt objects are infrastructure failures, never substituted content.

## Verifier policy

`tenet_policy_schema` is the agent-facing authority for the policy format.
- `project` verifier: command definition comes from sealed A; execution root is Candidate Snapshot R. Tenet passes `argv[0]` directly to the operating system process launcher, so relative paths resolve from verifier `cwd` and ordinary PATH/absolute-path semantics apply. Candidate content can therefore influence a relative executable.
- `authority_snapshot` verifier: `oracle_path` names an A-owned directory to seal; `argv[0]` directly names a regular executable inside that bundle; `cwd` is relative to the bundle. The candidate is only exposed as `TENET_CANDIDATE_ROOT`.

For example, this is invalid unless the sealed bundle contains an executable file named `sh`:

```toml
authority = "authority_snapshot"
oracle_path = ".tenet/oracles"
argv = ["sh", "verify.sh"]
```

Tenet never treats that configuration as an implicit request to use a host shell.

Before proposal and approval, authority-snapshot bundles are checked in the mutable workspace. Sealing repeats validation over the exact captured authority and rejects machine-actionable structural failures such as `oracle_bundle_missing`, `oracle_bundle_not_directory`, `oracle_executable_missing`, `oracle_executable_not_file`, `oracle_executable_not_executable`, `oracle_cwd_missing`, and `oracle_cwd_not_directory`.

## Evidence

Every verifier observation binds the exact `authorityId`, `candidateId`, authority policy/specification/contract digests, obligation, verifier, effect, validity, provenance, and primary oracle identity.

Project identities bind verifier ID, exact candidate ID, and definition digest. Authority-snapshot identities bind verifier ID, exact authority ID, bundle content ID, executable content ID, and definition digest. Evidence from another `(A, R)` pair is stale. Contradiction overrides support; missing, invalid, stale, untrusted, or inconclusive evidence cannot authorize `done`.
