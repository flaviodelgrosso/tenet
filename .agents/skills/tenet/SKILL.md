---
name: tenet
description: Use when the user asks to use Tenet or completion is governed by a Tenet contract.
compatibility: Requires Tenet MCP tools and Git.
metadata:
  tenet-skill-version: "1"
---

# Tenet workflow

Tenet judges exact candidate revision R under independently selected authority revision A. The coding agent owns engineering; Tenet owns completion authority. `tenet init` is the only user-facing CLI workflow; after initialization, interact with Tenet only through its semantic MCP tools.

1. Inspect `tenet_status` before relying on a completion condition. Status never runs verifiers.
2. Inspect `tenet_policy_schema` before changing verification policy. Configure only supported fields; never infer hidden capabilities or reverse-engineer the binary.
3. If no contract is admitted, inspect the specification and policy. Preserve every explicit acceptance criterion and Definition of Done item.
4. Map each material obligation only to a verifier that actually observes it. Add small deterministic policy verifiers or authority-owned oracle assets when needed; do not modify product code to accommodate a verifier before authority admission.
5. Obtain the proposal shape from `tenet_contract_schema`, reference verifiers only by ID, then submit the complete typed proposal with `tenet_contract_propose`. Never supply or infer verifier authority.
6. On `pending_approval`, present the exact canonical proposal returned by Tenet, without reconstructing it from agent memory; its proposal ID and digest; every primary and oracle-assurance verifier mapping; the complete verification profile; and every Tenet warning.
7. Ask whether the human explicitly approves that exact proposal ID and digest after reviewing the canonical proposal, verifier mappings, verification profile, and warnings. Never self-approve or infer approval from silence, continuation, generic acknowledgement, or an earlier proposal.
8. Only after explicit approval, call `tenet_contract_approve` with that exact proposal ID and digest. If content, specification, policy, ID, or digest changed, discard the earlier approval and request fresh approval.
9. Freeze the admitted specification, policy, contract, and authority-owned oracle assets in a commit. Present its full SHA and ask the operator to select it explicitly as authority A. Never choose or advance A yourself.
10. Implement and debug normally without changing A's authority surface. Produce immutable candidate commit R descended from A.
11. Call `tenet_gate` with both exact full revisions: `authorityRevision = A` and `revision = R`. Never infer either revision from HEAD, state, or prior calls.
12. Treat `not_done`, contradiction, inconclusive evidence, and infrastructure failure according to their typed blockers. Inspect `tenet_evidence` for exact R and continue engineering; never weaken authority to obtain `done`.
13. Claim completion only when `tenet_gate` returns `done` for the exact reported (A, R) pair.

Proposal admission and operator selection of A are explicit human trust decisions, not security sandboxes.
