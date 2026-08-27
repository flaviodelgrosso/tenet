---
name: tenet
description: Use when the user explicitly asks to use Tenet, invokes the Tenet workflow, or requests implementation or completion under this repository's Tenet contract.
compatibility: Requires the tenet CLI and Git.
metadata:
  tenet-skill-version: "1"
---

# Tenet workflow

Tenet determines whether candidate revision R satisfies the admitted completion contract from authority revision A. It does not perform the engineering.

1. Run `tenet status --json` and use only its current repository state.
2. If the contract is missing, inspect the configured specification, get the proposal shape with `tenet contract schema --json`, and submit it with `tenet contract propose --file <path> --json`.
3. When approval is required, report the proposal ID and digest, stop, and ask the user to perform operator admission. Never invoke `tenet contract approve` yourself.
4. Treat A as operator/CI-provided trust context. Never choose or advance A yourself; if none is provided, stop and ask the user.
5. Perform the engineering normally and produce immutable candidate commit R.
6. Run `tenet gate --authority-revision <authority-sha> --revision <candidate-sha> --json`.
7. On a non-`done` verdict, inspect typed blockers and `tenet evidence --revision <candidate-sha> --json`, then continue or report the blocker.
8. Declare completion only when Tenet returns `done` for the exact (A, R) pair you report.

Proposal/admission separation is a same-user workflow boundary, not a security sandbox. The CLI is authoritative; this optional Skill is not.
