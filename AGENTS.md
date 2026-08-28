# AGENTS.md

## Product boundary

Tenet is an agent-neutral, CLI-first completion authority for exact Git revisions.

The coding agent owns engineering: investigation, planning, editing, testing, commits, and responses to blockers. Tenet owns:

- the trusted authority snapshot;
- admitted contract semantics;
- evidence semantics and provenance;
- exact-revision verifier execution;
- deterministic completion derivation.

Tenet does not need to know which coding agent produced a candidate. No agent statement, model output, generated Skill, or verifier exit code is itself a completion decision. `DONE` is derived only by Tenet's deterministic domain rules from admitted evidence.

## Authority and candidate revisions

Final gating always has two explicit Git identities:

```text
authority revision A   defines what must be true
candidate revision R   contains the software being judged
```

Both identities must resolve to full immutable commit IDs. The caller supplies A as trusted context; candidate-controlled files and local audit state must never select it. A must be an ancestor of R.

Never make the candidate revision the source of the authority definition used to judge itself.

The final gate loads all completion control-plane inputs from A:

- `.tenet/tenet.toml`;
- `.tenet/contract.json`;
- the specification at the authority policy's `spec_path`.

Verifier definitions come from A but execute in a detached materialization of R. If R changes any authority-owned path, gating fails closed with `authority_surface_changed`. An intentional specification, contract, or policy change requires operator admission in a new commit, and that commit becomes the next authority revision.

Use Git's own commit and ancestry operations. Do not introduce another repository identity or commit-graph model.

## Admission boundary

A contract proposal is not an admitted contract. A coding agent may construct and submit a deterministic proposal, but operator admission remains a separate workflow step.

Under the same-user threat model, the exact authority revision selected by the operator or CI is trusted repository control state. `.tenet/contract.json` at A is canonical for that snapshot. Proposal and contract digests provide identity, reproducibility, and stale-state detection; they are not authentication.

This is a domain and workflow boundary, not a security sandbox against a deliberately malicious process running as the same OS user. Do not add passwords, HMACs, keychains, signatures, controller secrets, privileged services, or mandatory containers to imply otherwise.

## Evidence and completion

Evidence produced by a final gate must bind:

- authority revision A;
- candidate revision R;
- specification, contract, and policy digests from A;
- obligation ID;
- verifier ID;
- captured observation, effect, validity, authority, and provenance.

Evidence from another authority or candidate pair is stale for the current gate. Local `.tenet/state.json` is disposable audit history and never defines completion.

Keep implementation state distinct from verification state. Apparent implementation is not verification. Previous evidence is an observation, not mutable truth; preserve contradictory observations and let contradiction block completion.

Fail closed. Missing, stale, invalid, inadmissible, contradictory, or unverifiable evidence must not become satisfied. Infrastructure failure remains distinct from a verifier contradiction or inconclusive result. Every required obligation must reach `contract_satisfied` before `DONE`.

## Claim-to-oracle honesty

The only currently executable verifier authority is a Tenet-observed project verifier. Do not add a `protected`, external, or human authority label without a real producer and an independently established boundary matching that claim.

A project verifier proves only that the admitted project check produced the observed result against R. Candidate-controlled project code may influence that check. Do not describe an ordinary local process execution as protected, isolated, or resistant to candidate-controlled oracle manipulation.

Agent-reported commands and assertions may aid engineering, but they are not admitted final-gate evidence. Executing an agent-proposed command does not upgrade the semantic claim behind it.

## Domain and validation

Rust domain types are the source of truth. Derive serialization and JSON Schema from those types where practical. Keep validation layers distinct:

```text
syntax -> schema/Serde -> domain invariants -> repository/runtime invariants
```

Use semantic ID newtypes when identity confusion matters. Use `thiserror` for domain errors callers distinguish and `anyhow` at CLI and I/O boundaries. Avoid `unwrap` and `expect` in production code.

Breaking changes are allowed during the MVP when they strengthen or simplify the architecture. Update every in-repository caller, test, example, fixture, and active document in the same change. Remove obsolete variants and compatibility paths rather than maintaining competing designs.

## MVP versioning policy

Tenet is unreleased MVP software. Until the first public release establishes a compatibility boundary, all Tenet-owned schema and format versions remain `1`.

Do not increment a Tenet-owned version because of a breaking change made during MVP development.

This applies to current Tenet-owned persisted and serialized formats, including contract, proposal, policy, evidence, state, protocol-facing schema identifiers, fixtures, examples, and equivalent version markers where the value represents a Tenet format.

During the unreleased MVP:

- breaking changes are explicitly allowed without a version increment;
- every current Tenet-owned schema or format version must remain `1`;
- the current repository state defines the only supported form of each format;
- earlier unreleased development variants do not require backward compatibility;
- do not add migrations or compatibility branches for obsolete unreleased variants;
- remove obsolete structures and update all callers, tests, fixtures, generated schemas, and documentation in the same change;
- never introduce version `2` or higher in anticipation of future compatibility needs.

A version increment becomes appropriate only after a public release has established a format that Tenet intentionally continues to recognize or distinguish from a later incompatible format.

Do not apply this rule to versions owned by external protocols, dependencies, standards, or libraries. Their versions must follow the requirements of those external systems.

## Minimal architecture

The project intentionally uses one Cargo workspace with four crates:

- `tenet-domain` contains contract validation, evidence types, and deterministic completion derivation;
- `tenet-application` contains repository initialization, Git object reads, candidate materialization, verifier execution, and audit persistence;
- `tenet-mcp` exposes the typed application interface over stdio MCP;
- `tenet-cli` contains the `tenet` binary and CLI rendering.

Prefer existing files and direct Git commands. Do not introduce provider integrations, model runtimes, general plugin systems, databases, generic rule engines, or speculative traits and frameworks. Add a dependency only for a concrete capability the existing stack cannot express cleanly.

Structured verifier commands use explicit argv, cwd, environment, timeout, and bounded output. Avoid shell interpretation where direct process execution is possible.

## Testing expectations

Architectural changes require adversarial tests for their invariants. Important cases include:

- candidate policy, contract, or specification mutation cannot produce `DONE`;
- a non-ancestor authority fails closed;
- verifier definitions come from A and execute against R;
- evidence from `(A1, R)` cannot satisfy `(A2, R)`;
- untrusted agent evidence never upgrades an obligation;
- contradiction overrides support;
- missing evidence remains blocking;
- a fresh clone gates exact A and R without historical state or credentials;
- unsupported authority labels fail deserialization.

Tests must be deterministic and offline. They must not require a coding agent, model provider, API key, keychain, network service, or mandatory sandbox platform.

Before completion, run:

```bash
make ci
```

This checks formatting, the package, Clippy with warnings denied, and all tests.

## Working style

1. Inspect the relevant domain types, gate flow, and callers before editing.
2. Preserve the authority/candidate split at every interface and persistence boundary.
3. Reuse established patterns and Git primitives; keep the change materially small.
4. Update all affected callers and tests in one clean cutover.
5. Verify the exact changed behavior with focused adversarial tests, then run full CI.
6. Report any remaining unverifiable invariant explicitly; never weaken completion semantics to obtain a green result.

> Coding agents perform engineering. Trusted authority revision A defines the contract. Tenet observes candidate revision R and derives `DONE(A, R)`.
