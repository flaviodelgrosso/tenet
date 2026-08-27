---
name: tenet
description: Use when the user explicitly asks to use Tenet, invokes the Tenet workflow, or requests implementation or completion under this repository's Tenet contract.
compatibility: Requires the tenet CLI and Git.
metadata:
  tenet-skill-version: "4"
---

# Tenet workflow

Tenet determines whether candidate revision R satisfies the admitted completion contract from authority revision A. It does not perform the engineering.

1. Run `tenet status --json` and use only the current repository state.
2. Before creating or changing `.tenet/tenet.toml`, run `tenet policy schema --json` and configure only fields and values it emits. Never inspect the Tenet binary with `strings`, `nm`, `objdump`, or other binary reverse-engineering, and never infer hidden configuration. A capability absent from this CLI schema is unsupported.
3. If the contract is missing, inspect the configured specification and verification policy before proposing anything. Treat explicit acceptance criteria and Definition of Done items as completion requirements that must not be silently omitted.
4. Build a verification plan from the specification. For each material obligation, identify what observable evidence would actually support it and which verifier could produce that evidence. Verifier availability alone is not sufficient: do not map unrelated claims to a generic verifier merely because it is already configured, and do not claim that a verifier proves properties it does not observe.
5. If the current verification policy is empty or lacks suitable verifiers, prepare the missing verification policy and authority-owned oracle assets as part of the Tenet authority bootstrap. You may create or update `.tenet/tenet.toml` and `.tenet/oracles/**`, and may use existing external tools or small purpose-built verifier scripts where appropriate.
   This bootstrap phase is verification engineering, not product implementation. Do not modify the application implementation merely to satisfy or accommodate the proposed verifiers before the authority is admitted.
   Prefer simple deterministic or empirical checks over custom verification machinery. Reuse standard tools where possible. Do not invent a verifier for inherently subjective properties merely to make them appear mechanically verified; identify those properties explicitly as non-mechanically verified or requiring operator judgement.
6. Once the verification policy can support the material obligations, derive the completion contract from the specification and verification plan. Get the proposal shape with `tenet contract schema --json`, submit it with `tenet contract propose --file <path> --json`, and proceed to explicit operator approval.
   Do not stop merely because the initial policy has no verifiers. Stop only when a material requirement cannot be given an honest verification strategy with the available environment/tools, or when operator input is genuinely required to decide what evidence should be authoritative.
7. On `pending_approval`, show the user the exact proposal: its ID and digest; every requirement and obligation ID and statement; every primary verifier mapping; and every oracle-assurance ID, criterion, verifier, and authority mapping. Call out any specification requirement that remains non-mechanically verified or intentionally grouped under another obligation.
   After presenting the proposal, request explicit approval of that exact proposal. If a structured `ask` tool is available, use it with a single Yes/No confirmation that explicitly includes the proposal ID and digest, for example: `Approve proposal <proposal-id> with digest <proposal-digest> exactly as shown?`
   Treat only an explicit `Yes` selection as approval. `No`, dismissal, cancellation, no response, tool failure, or any ambiguous answer is not approval. If the `ask` tool is unavailable, ask the same exact Yes/No question in normal chat; the user does not need to manually repeat the proposal ID or digest.
8. Only after the user explicitly approves that exact ID and digest, run `tenet contract approve --proposal <id> --digest <digest> --json`. Never self-approve, infer approval from silence, continuation, a generic acknowledgement unrelated to the exact confirmation, or a previous approval.
9. Approval is bound to the exact proposal ID and digest that were shown to the user. If the proposal content, specification, policy, ID, or digest changes after the approval question was presented—or the CLI rejects the approval as stale or mismatched—discard that approval, show the current proposal, and request a new explicit Yes/No approval. Never reuse an earlier approval for a changed proposal.
10. After contract admission and before product implementation begins, freeze the complete authority surface in an immutable Git commit containing the admitted specification, verification policy, completion contract, and any authority-owned oracle assets.
    Never choose or advance A yourself. The resulting commit is only a proposed authority revision; Tenet or the coding agent must not silently select it as A.
    Present the exact full commit SHA to the operator and request explicit selection of that commit as authority A. If a structured `ask` tool is available, use a Yes/No confirmation such as:
    `Use commit <full-sha> as the immutable Tenet authority revision A for this implementation run?`
    Treat only an explicit `Yes` as selection of A. The user does not need to manually repeat or paste the SHA.
    If the user selects `Yes`, retain that exact SHA as A for the remainder of the run and proceed with engineering. Do not ask for A again unless the authority surface changes, the selected commit becomes invalid for the candidate lineage, or the user explicitly selects a different authority.
    If the user selects `No`, cancels, dismisses the prompt, or the confirmation cannot be obtained, stop before product implementation.
    Never infer A merely because a commit contains the admitted authority surface, never silently use candidate revision R as A, and never advance A after engineering has begun.
11. Perform the engineering normally without modifying the authority-owned surface, and produce immutable candidate commit R descended from A.
12. Run `tenet gate --authority-revision <authority-sha> --revision <candidate-sha> --json`.
13. On a non-`done` verdict, inspect typed blockers and `tenet evidence --revision <candidate-sha> --json`, then continue engineering or report the blocker. Do not weaken the admitted contract or authority to make the candidate pass.
14. Declare completion only when Tenet returns `done` for the exact (A, R) pair you report.

Proposal/admission separation and operator selection of A are same-user workflow trust boundaries, not security sandboxes. The CLI is authoritative; this optional Skill is not.
