# AGENTS.md

## Project context

Tenet is an MVP for deterministic, evidence-backed orchestration of autonomous coding agents.

The core idea is that probabilistic agents may reason, inspect, propose, implement, and repair, while the Tenet controller owns the authoritative engineering state transitions: requirements, work admission, verification, evidence validity, integration, and completion.

The architectural direction is:

```text
Specification
    ↓
Requirement catalog
    ↓
Acceptance criteria
    ↓
Verification obligations
    ↓
Bounded agent work
    ↓
Controller-executed verification
    ↓
Revision-bound evidence
    ↓
Deterministic completion policy
    ↓
DONE
```

`DONE` is a controller state. It must never be treated as an agent assertion.

---

## MVP policy: breaking changes are allowed

Tenet is currently an MVP and is still establishing its architecture.

**Backward compatibility is not a default requirement.**

For new implementations and refactors:

- breaking API changes are allowed;
- persisted local state formats may change;
- internal module boundaries may change;
- config schemas may change;
- agent structured-output contracts may change;
- CLI/TUI behavior may change where required by the architecture;
- old migrations or compatibility shims are not required unless explicitly requested by the task;
- do not preserve a weak or misleading abstraction only to remain compatible with an earlier MVP implementation.

Prefer a clean, coherent architecture over compatibility with accidental early design decisions.

When a breaking change is introduced:

- update all in-repository callers in the same change;
- update tests;
- update examples, README, schemas, prompts, and fixtures affected by the change;
- remove obsolete compatibility code rather than layering new behavior on top of it.

Do not intentionally break behavior unrelated to the task.

---

## Architectural invariants

Preserve these unless the task explicitly requires changing them.

### Controller authority

Agents propose. The controller decides.

Agents must not be authoritative for:

- requirement verification;
- evidence validity;
- integration acceptance;
- completion;
- canonical repository state;
- trusted execution policy.

Model output is input to deterministic controller logic, not controller truth.

### Evidence-backed completion

Completion should be derived from explicit, revision-bound evidence.

The intended relationship is:

```text
Spec fragment
    ↓
Requirement
    ↓
Acceptance Criterion
    ↓
Verification Obligation
    ↓
Verification Execution
    ↓
Evidence
    ↓
Repository Revision
```

Model observations may be useful, but they must not carry the same authority as controller-observed verification.

### Fail closed

When Tenet cannot establish a safety or correctness property, prefer:

```text
UNVERIFIED
STALE
CONTRADICTED
BLOCKED
NEEDS_REVERIFICATION
```

over silently assuming success.

Unknown evidence validity must not mean valid.

Unknown dependency coverage must not mean unchanged.

Missing deterministic evidence must not mean verified.

### Canonical repository mutation

Canonical repository changes must occur through controlled integration.

Workers should operate in isolated/disposable workspaces where appropriate.

A worker completing successfully does not imply its changes are accepted.

### Git revisions are authoritative repository identities

Reuse Git revision identities rather than inventing a second repository-version mechanism.

Evidence tied to code must identify the revision against which it was established.

### Persistent identity must not depend on graph container internals

Stable domain IDs are authoritative.

Do not persist `petgraph::NodeIndex` or similar process-local graph indexes.

`petgraph` is appropriate for transient graph algorithms and projections, not durable identity.

---

## Rust design principles

### Domain types are the source of truth

Prefer strongly typed Rust domain models.

Agent-facing schemas, serialization, and validation should derive from the Rust types wherever practical.

The structured-output path should remain conceptually:

```text
Rust type
    ↓
schemars
    ↓
JSON Schema
    ↓
agent output
    ↓
jsonschema validation
    ↓
Serde deserialization
    ↓
domain validation
```

Do not reintroduce manually duplicated JSON Schema definitions when the contract already exists as a Rust type.

### Keep validation layers distinct

Use the right layer for each invariant:

```text
JSON syntax
    ↓
JSON Schema
    ↓
Serde typed deserialization
    ↓
Domain invariants
    ↓
Runtime/controller invariants
```

Examples:

- required field: schema;
- valid enum representation: schema/Serde;
- referenced requirement exists: domain validation;
- work graph is acyclic: graph/domain validation;
- repository HEAD is unchanged: runtime invariant;
- `DONE` is legal: completion policy.

Do not force semantic controller rules into JSON Schema.

### Prefer semantic IDs over raw strings

Where identity confusion matters, use newtypes such as:

```rust
RequirementId
CriterionId
ObligationId
EvidenceId
VerificationRunId
WorkUnitId
LeaseId
```

Do not add a new ID crate unless there is a concrete reason.

Use UUIDs for instance identities where appropriate and human-readable deterministic IDs for semantic catalog identities where useful.

### Error handling

Use typed errors for domain semantics.

Prefer `thiserror` for errors that callers may meaningfully distinguish.

Use `anyhow` at application/runtime boundaries for I/O, orchestration, context propagation, and command execution failures.

Do not mechanically convert every error into either style.

### Time

Prefer typed `chrono::DateTime<Utc>` for new domain timestamps.

Avoid adding new RFC3339 timestamp fields as arbitrary `String` values unless compatibility or an external wire format specifically requires it.

### Avoid speculative abstraction

Do not introduce a trait, service, framework, or generic parameter solely because a future feature might need it.

Extract abstractions when they already correspond to a coherent responsibility with independent invariants or tests.

---

## Controller organization

The controller should orchestrate the run state machine, not own all subsystem implementation details.

Keep high-level sequencing in `Controller`, including:

- run lifecycle;
- phase transitions;
- cancellation;
- cycle orchestration;
- deciding when to continue, block, or attempt completion;
- coordination between subsystems.

Prefer extracting cohesive capabilities such as:

```text
CatalogManager
    spec parsing / catalog lifecycle / catalog coverage

EvidenceManager
    evidence establishment / invalidation / persistence / transitions

VerificationEngine
    trusted verification requests / execution / obligation-bound results

CompletionPolicy
    deterministic completion decision and typed blockers
```

Existing cohesive components such as scheduler, integrator, workspace manager, and work graph should remain separate.

Do **not** split `controller.rs` merely to reduce line count. Avoid thin one-method wrappers that only forward calls.

The desired controller style is closer to:

```rust
let catalog = catalog_manager.ensure(...).await?;
let reconciliation = reconcile(...).await?;
let candidates = scheduler.execute(...).await?;
let integration = integrator.integrate(...).await?;
evidence_manager.apply(...).await?;
let decision = completion_policy.evaluate(...);
```

than embedding every subsystem's implementation inside the orchestration flow.

---

## Evidence Graph rules

### Separate implementation state from verification state

Do not collapse these concepts.

An implementation may appear present while remaining unverified.

Verification may be stale even when implementation still exists.

### Preserve evidence history

Evidence is an observation, not a mutable truth record.

Do not overwrite previous pass/fail evidence merely because a newer execution exists.

Contradictory evidence should remain representable.

### Provenance matters

Evidence must carry provenance sufficient to distinguish controller-observed execution from model assertions or other advisory observations.

Executing an agent-proposed command does not automatically make the semantic claim behind that command trustworthy.

### Verification must bind explicitly to obligations

Prefer:

```text
ObligationId
    ↓
VerificationExecutionRequest
    ↓
VerificationExecutionResult
    ↓
Evidence
```

Do not rely primarily on reverse matching command strings to infer which obligation an execution was meant to satisfy.

### Evidence invalidation must be conservative

If a relevant repository change may invalidate evidence, mark it stale and reverify.

Agent-proposed dependency scopes are not automatically trusted.

When a trustworthy narrow dependency relationship cannot be established, prefer a broad scope and additional re-verification over preserving potentially stale evidence.

---

## Specification and catalog rules

The authoritative specification defines the product intent.

The requirement catalog is an interpretation of that spec, not a replacement for it.

For normative specification content:

- requirements should be required by default;
- acceptance criteria should be mandatory by default;
- required verification obligations should not be silently downgraded by model choice;
- optionality should be justified by the specification or explicit controller/user policy.

Preserve provenance from normative spec fragments to requirements where supported.

A structurally valid catalog is not necessarily a complete catalog.

Completion must eventually fail closed when normative specification coverage is incomplete.

---

## Verification and command execution

Avoid treating arbitrary model-generated shell strings as trusted verification primitives.

Prefer structured execution specifications with explicit:

- program;
- arguments;
- working directory;
- environment;
- timeout;
- authority/provenance;
- obligation binding.

Avoid `sh -lc` or equivalent shell interpretation for trusted verification where direct process execution is possible.

Distinguish between:

- project-configured verification;
- controller-derived or controller-approved verification;
- agent-proposed checks.

These may have different evidence authority.

A command exiting `0` is not by itself proof that an acceptance criterion is satisfied.

---

## Isolation and security boundaries

Disposable Git worktrees provide repository mutation isolation, not a complete security sandbox.

Do not describe them as a security sandbox.

Architect, Reconcile, and Assess should remain repository-read-only from the perspective of the canonical repository.

Controller-owned files such as `.tenet/`, the authoritative specification, configuration, and other explicitly protected artifacts must not be mutated by implementation workers.

Do not add a broad sandbox framework unless the task specifically requires it. If safe verification execution needs an execution-environment seam, keep it narrow.

---

## Persistence

For the MVP, prefer the existing versioned local persistence architecture.

Do not introduce Neo4j, another graph database, an ORM, or a second authoritative persistence system without an explicit task requiring it.

Persist stable domain entities and relationships.

Because backward compatibility is not currently required by default, a new implementation may replace an obsolete state shape directly rather than carrying multiple legacy migrations, provided all in-repository uses are updated consistently.

Keep persistence deterministic, inspectable, and easy to rebuild where possible.

---

## Dependencies

Prefer existing workspace crates before adding alternatives.

Important existing choices include:

- `serde`
- `serde_json`
- `schemars`
- `jsonschema`
- `thiserror`
- `anyhow`
- `chrono`
- `uuid`
- `sha2`
- `globset`
- `petgraph`
- `tokio`
- `proptest`

Do not add:

- another serialization framework;
- another graph library for functionality already handled by `petgraph`;
- `git2` merely to replace working Git CLI helpers;
- a database merely because the domain contains graphs;
- a generic rule engine for completion or evidence policy.

Add a crate only when it removes meaningful custom infrastructure or provides a capability that cannot be cleanly expressed with the existing stack.

---

## Testing expectations

Architectural changes must include tests for the invariants they introduce.

Prefer adversarial tests over only happy-path coverage.

Important examples include:

- advisory model evidence cannot establish `Verified`;
- missing mandatory evidence blocks verification;
- stale evidence cannot remain authoritative;
- contradictory trusted evidence blocks verification;
- wrong `ObligationId` bindings are rejected;
- an unrelated passing command cannot establish a requirement;
- unknown dependency validity fails conservative;
- a normative spec fragment cannot disappear silently from completion;
- completion is impossible with a dirty or changed canonical repository;
- active leases or pending integrations prevent `DONE`;
- serialization round-trips preserve domain semantics.

Use `proptest` where the property itself matters more than a fixed fixture.

Examples:

```text
adding untrusted evidence never upgrades UNVERIFIED → VERIFIED

adding a blocking contradiction never preserves VERIFIED

removing required coverage never makes completion easier
```

---

## Quality gate

Before considering repository-wide Rust changes complete, run the equivalent of:

```bash
make fmt-check
make check
make clippy
make test
```

or simply:

```bash
make ci
```

The repository Makefile defines the full CI gate as formatting, workspace checks, Clippy with warnings denied, and all workspace tests.

Do not claim completion while relevant tests or compiler/linter checks are failing.

---

## Scope discipline

Tenet should not become another general-purpose coding-agent runtime.

Do not add unrelated infrastructure such as:

- model-provider frameworks;
- generic MCP hosting;
- skill marketplaces;
- plugin ecosystems;
- generic agent-role frameworks;
- vector/RAG memory by default;
- large web UI frameworks;
- graph databases solely because relationships are graph-shaped.

Tenet's differentiation is the supervisory control layer around autonomous software engineering:

```text
what must be true
what remains
which work is admissible
what was actually verified
which evidence is still valid
whether a candidate may integrate
whether DONE is justified
```

Optimize new code for those responsibilities.

---

## Working style for coding agents

When implementing a task:

1. Inspect the relevant domain types and controller invariants before editing.
2. Prefer removing obsolete code over preserving two competing designs.
3. Make breaking changes freely when they materially simplify or strengthen the MVP architecture.
4. Update all affected callers and tests in the same change.
5. Keep probabilistic/model-derived information distinguishable from controller-established facts.
6. Fail closed on ambiguous correctness or evidence states.
7. Avoid speculative infrastructure.
8. Run the relevant focused tests during implementation and the complete quality gate before declaring the task complete.
9. Report architectural compromises or incomplete invariants explicitly rather than hiding them behind a passing test.
10. Do not weaken tests, verification, evidence policy, or completion rules merely to obtain a green result.

---

## Guiding principle

> **Agents propose actions and interpretations. Tenet admits state transitions. Trusted evidence authorizes completion.**

For the MVP, choose architectural clarity and trustworthy invariants over backward compatibility with earlier experimental implementations.
