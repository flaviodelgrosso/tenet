---
name: tenet
description: Use when the user explicitly asks to use Tenet, invokes the Tenet workflow, or requests implementation or completion under this repository's Tenet contract.
compatibility: Requires the tenet CLI and Git.
metadata:
  tenet-skill-version: "2"
---

# Tenet workflow

Tenet determines whether candidate revision R satisfies the admitted completion contract from authority revision A. It does not perform the engineering.

1. Run `tenet status --json` and use only its current repository state.
2. If the contract is missing, inspect the configured specification, get the proposal shape with `tenet contract schema --json`, and submit it with `tenet contract propose --file <path> --json`.
3. On `pending_approval`, show the user the exact proposal: its ID and digest; every requirement and obligation ID and statement; and every obligation's verifier ID and authority mapping. Request an explicit approval naming that exact ID and digest.
4. Never self-approve, infer approval from silence, a generic acknowledgement, or a prior approval. Only after the user explicitly approves that exact ID and digest, run `tenet contract approve --proposal <id> --digest <digest> --json`.
5. If the proposal content, specification, policy, ID, or digest changes before admission—or the CLI rejects the approval as stale or mismatched—treat the approval as invalid. Show the current proposal and request fresh explicit approval; never retry admission using the prior approval.
6. Treat A as operator/CI-provided trust context. Never choose or advance A yourself; if none is provided, stop and ask the user.
7. Perform the engineering normally and produce immutable candidate commit R.
8. Run `tenet gate --authority-revision <authority-sha> --revision <candidate-sha> --json`.
9. On a non-`done` verdict, inspect typed blockers and `tenet evidence --revision <candidate-sha> --json`, then continue or report the blocker.
10. Declare completion only when Tenet returns `done` for the exact (A, R) pair you report.

Proposal/admission separation is a same-user workflow boundary, not a security sandbox. The CLI is authoritative; this optional Skill is not.
