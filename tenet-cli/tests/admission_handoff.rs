//! Regression coverage for trusted Authority Admission inside coding-agent
//! workflows.
//!
//! At `AUTHORITY_ADMISSION` the derived context must carry a structured
//! approval-UX preview so a host agent can render a native confirmation
//! without copying content IDs or invoking Tenet CLI knowledge. A user
//! approval value is never an `AdmissionGrant`: the candidate producer without
//! the trusted admission secret cannot admit, a fabricated grant fails closed
//! even in a trusted process, and mismatched identities fail closed. The
//! trusted handoff `tenet authority admit-prepared` admits the exact prepared
//! chain derived from repository state — no manual identity re-entry — and
//! the CLI-only workflow continues to completion afterwards.

use std::{fs, path::Path, process::Command};

use tenet_domain::{
  algebra::{
    AssuranceRequirementV1, COMPLETION_POLICY_V1, CompletionContractV1, CompletionPolicyId,
    Criterion, CriterionId, EvidenceControlRequirementV1, EvidenceRequirementV1, Requirement,
    Verifier, VerifierId, VerifierMaterial,
  },
  policy::{CandidateCapturePolicy, ProjectConfig},
};

const SECRET: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

fn tenet_with_secret(root: &Path, args: &[&str], secret: Option<&str>) -> std::process::Output {
  let mut command = Command::new(env!("CARGO_BIN_EXE_tenet"));
  command.arg("--cwd").arg(root).args(args);
  command.env_remove("TENET_ADMISSION_SECRET");
  if let Some(secret) = secret {
    command.env("TENET_ADMISSION_SECRET", secret);
  }
  command.output().expect("spawn tenet")
}

/// A trusted-context call (holds the admission secret).
fn ok_json(root: &Path, args: &[&str]) -> serde_json::Value {
  let output = tenet_with_secret(root, args, Some(SECRET));
  assert!(
    output.status.success(),
    "args {args:?} failed: {}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  serde_json::from_slice(&output.stdout).expect("JSON output")
}

/// A candidate-producer call (no admission secret in the process).
fn producer_ok_json(root: &Path, args: &[&str]) -> serde_json::Value {
  let output = tenet_with_secret(root, args, None);
  assert!(
    output.status.success(),
    "args {args:?} failed: {}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  serde_json::from_slice(&output.stdout).expect("JSON output")
}

fn error_json(output: &std::process::Output) -> serde_json::Value {
  assert_eq!(output.status.code(), Some(1), "expected invalid-input exit");
  serde_json::from_slice(&output.stdout).expect("JSON error")
}

fn criterion(id: &str, proposition: &str, verifier: &str) -> Criterion {
  Criterion {
    id: CriterionId(id.into()),
    proposition: proposition.into(),
    verifiers: vec![Verifier {
      id: VerifierId(verifier.into()),
      material: VerifierMaterial::Candidate,
    }],
    evidence: EvidenceRequirementV1 {
      control: EvidenceControlRequirementV1::CandidateControlledPermitted,
      assurance: AssuranceRequirementV1::LocalOrStronger,
    },
  }
}

/// Two requirements, three criteria, three distinct verifiers.
fn contract() -> CompletionContractV1 {
  CompletionContractV1 {
    schema_version: 1,
    policy: CompletionPolicyId(COMPLETION_POLICY_V1.into()),
    requirements: vec![
      Requirement {
        id: tenet_domain::contract::RequirementId("R1".into()),
        statement: "requirement one statement".into(),
        criteria: vec![
          criterion("C1", "criterion one holds", "V1"),
          criterion("C2", "criterion two holds", "V2"),
        ],
      },
      Requirement {
        id: tenet_domain::contract::RequirementId("R2".into()),
        statement: "requirement two statement".into(),
        criteria: vec![criterion("C3", "criterion three holds", "V3")],
      },
    ],
  }
}

struct Repo {
  directory: tempfile::TempDir,
}

impl Repo {
  fn new() -> Self {
    let directory = tempfile::tempdir().expect("repository");
    let root = directory.path();
    fs::write(root.join("SPEC.md"), "# Specification\n").expect("spec");
    let init = tenet_with_secret(root, &["init", "--json"], Some(SECRET));
    assert!(init.status.success(), "init failed");
    fs::write(root.join("candidate.txt"), "original candidate").expect("candidate");
    let mut include = vec!["candidate.txt".to_string()];
    let mut verifiers = Vec::new();
    for id in ["V1", "V2", "V3"] {
      let script = format!("verify-{id}.sh");
      fs::write(root.join(&script), "#!/bin/sh\nexit 0\n").expect("verify script");
      fs::set_permissions(
        root.join(&script),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
      )
      .expect("executable");
      include.push(script.clone());
      verifiers.push(tenet_domain::policy::VerifierSpec {
        id: id.into(),
        command: tenet_domain::policy::CommandSpec {
          argv: vec![tenet_domain::policy::CommandArgument::CandidatePath(script)],
          cwd: tenet_domain::policy::CommandCwd::Candidate(".".into()),
          env: tenet_domain::policy::EnvironmentSpec::default(),
          timeout_ms: 5_000,
          result: tenet_domain::policy::ExitCodePolicy {
            pass: [0].into(),
            fail: [1].into(),
            inconclusive: [].into(),
          },
        },
        max_output_bytes: 4_096,
        authority: tenet_domain::policy::VerifierAuthority::Project,
        oracle_path: None,
        protection: tenet_domain::policy::VerifierProtection::Local,
      });
    }
    let policy = ProjectConfig {
      version: 1,
      spec_path: "SPEC.md".into(),
      candidate: CandidateCapturePolicy {
        root: ".".into(),
        include,
        exclude: vec![],
      },
      verifiers,
    };
    fs::write(
      root.join(".tenet/tenet.toml"),
      toml::to_string_pretty(&policy).expect("policy"),
    )
    .expect("write policy");
    Self { directory }
  }

  fn root(&self) -> &Path {
    self.directory.path()
  }

  fn write_contract(&self, contract: &CompletionContractV1) -> String {
    let path = self.root().join("contract.json");
    fs::write(
      &path,
      serde_json::to_vec_pretty(contract).expect("contract"),
    )
    .expect("write");
    path.to_string_lossy().into_owned()
  }

  /// Producer-side lifecycle (no admission secret): PROPOSAL + RECONCILIATION.
  /// Returns `(proposal_id, reconciliation_id, authority_id)`.
  fn prepare(&self) -> (String, String, String) {
    let contract_path = self.write_contract(&contract());
    let proposal = producer_ok_json(
      self.root(),
      &[
        "authority",
        "prepare",
        "--contract",
        &contract_path,
        "--json",
      ],
    );
    let proposal_id = proposal["proposalId"]
      .as_str()
      .expect("proposal id")
      .to_owned();
    let authority_id = proposal["authorityId"]
      .as_str()
      .expect("authority id")
      .to_owned();
    let reconciliation = producer_ok_json(
      self.root(),
      &[
        "authority",
        "reconcile",
        "--proposal",
        &proposal_id,
        "--json",
      ],
    );
    let reconciliation_id = reconciliation["reconciliationId"]
      .as_str()
      .expect("reconciliation id")
      .to_owned();
    (proposal_id, reconciliation_id, authority_id)
  }
}

#[test]
fn context_at_admission_carries_the_approval_ux_payload() {
  let repo = Repo::new();
  let root = repo.root();

  // No preview before the chain is prepared.
  let required = producer_ok_json(root, &["status", "--json"]);
  assert_eq!(required["phase"], "AUTHORITY_REQUIRED");
  assert!(
    required["admission"].is_null(),
    "preview must not appear before AUTHORITY_ADMISSION"
  );

  let (proposal_id, reconciliation_id, authority_id) = repo.prepare();
  let status = producer_ok_json(root, &["status", "--json"]);
  assert_eq!(status["phase"], "AUTHORITY_ADMISSION");
  let preview = &status["admission"];
  assert!(
    preview.is_object(),
    "AUTHORITY_ADMISSION must carry the structured preview"
  );
  assert_eq!(preview["proposalId"], proposal_id);
  assert_eq!(preview["reconciliationId"], reconciliation_id);
  assert_eq!(preview["authorityId"], authority_id);

  // Human-readable summary: enough for an approval prompt without noise.
  assert_eq!(preview["summary"]["requirements"], 2);
  assert_eq!(preview["summary"]["criteria"], 3);
  assert_eq!(preview["summary"]["verifiers"], 3);
  assert_eq!(preview["summary"]["assurance"], "LOCAL_V1");
  assert_eq!(preview["summary"]["specPath"], "SPEC.md");
  let surface = preview["summary"]["candidateSurface"]
    .as_array()
    .expect("candidate surface");
  for pattern in [
    "candidate.txt",
    "verify-V1.sh",
    "verify-V2.sh",
    "verify-V3.sh",
  ] {
    assert!(
      surface.iter().any(|entry| entry == pattern),
      "candidate surface omits {pattern:?}"
    );
  }

  // Optional detail view: statements, propositions, evidence policy, IDs.
  let requirements = preview["detail"]["requirements"]
    .as_array()
    .expect("requirements");
  assert_eq!(requirements.len(), 2);
  assert_eq!(requirements[0]["id"], "R1");
  assert_eq!(requirements[0]["statement"], "requirement one statement");
  assert_eq!(
    requirements[0]["criteria"][0]["proposition"],
    "criterion one holds"
  );
  assert_eq!(
    requirements[0]["criteria"][0]["verifierIds"],
    serde_json::json!(["V1"])
  );
  assert_eq!(
    requirements[0]["criteria"][0]["evidence"]["control"],
    "candidate_controlled_permitted"
  );
  assert_eq!(
    requirements[0]["criteria"][0]["evidence"]["assurance"],
    "local_or_stronger"
  );
  assert_eq!(
    requirements[1]["criteria"][0]["proposition"],
    "criterion three holds"
  );
  let verifiers = preview["detail"]["verifiers"]
    .as_array()
    .expect("verifiers");
  assert_eq!(verifiers.len(), 3);
  for verifier in verifiers {
    assert_eq!(verifier["authority"], "project");
    assert_eq!(verifier["protection"], "local");
  }
  for key in ["specId", "contractId", "surfaceId"] {
    assert!(
      preview["detail"]["contentIds"][key]
        .as_str()
        .is_some_and(|id| id.starts_with("sha256:")),
      "content id {key} missing"
    );
  }

  // The trusted handoff is one agent-independent argv.
  assert_eq!(preview["handoff"]["command"][0], "tenet");
  assert_eq!(preview["handoff"]["command"][1], "authority");
  assert_eq!(preview["handoff"]["command"][2], "admit-prepared");
  assert_eq!(preview["handoff"]["command"][3], "--json");
  assert_eq!(preview["handoff"]["requiresTrustedSecret"], true);
}

#[test]
fn user_approval_alone_cannot_admit() {
  let repo = Repo::new();
  let root = repo.root();
  let (proposal_id, reconciliation_id, authority_id) = repo.prepare();

  // The candidate producer's process has no trusted secret: neither the
  // trusted handoff nor the manual admission can be driven from it, so no
  // agent-generated `approved=true` value can become Admission here.
  let handoff = tenet_with_secret(root, &["authority", "admit-prepared", "--json"], None);
  assert_eq!(handoff.status.code(), Some(1));
  let error: serde_json::Value = serde_json::from_slice(&handoff.stdout).expect("JSON error");
  assert_eq!(error["code"], "admission_secret_unavailable");

  // Even a trusted process rejects a fabricated grant: an approval shortcut
  // cannot be laundered into Admission by writing grant-shaped JSON.
  let fabricated = serde_json::json!({
    "schemaVersion": 1,
    "semantics": "tenet:admission-grant:v1",
    "proposal": proposal_id,
    "authority": authority_id,
    "mac": "0".repeat(64),
  });
  let forged_path = root.join("forged.json");
  fs::write(
    &forged_path,
    serde_json::to_vec(&fabricated).expect("forged"),
  )
  .expect("write");
  let forged = tenet_with_secret(
    root,
    &[
      "authority",
      "admit",
      "--proposal",
      &proposal_id,
      "--reconciliation",
      &reconciliation_id,
      "--authority",
      &authority_id,
      "--grant",
      &forged_path.to_string_lossy(),
      "--json",
    ],
    Some(SECRET),
  );
  let error = error_json(&forged);
  assert_eq!(error["code"], "admission_grant_invalid");

  // Nothing admitted: the derived phase is unchanged and the ref is absent.
  let status = producer_ok_json(root, &["status", "--json"]);
  assert_eq!(status["phase"], "AUTHORITY_ADMISSION");
  let blockers = producer_ok_json(root, &["blockers", "--json"]);
  assert!(
    blockers["blockers"]
      .as_array()
      .expect("blockers")
      .iter()
      .any(|blocker| blocker["code"] == "admission_missing")
  );
}

#[test]
fn trusted_helper_admits_the_prepared_authority_without_ids() {
  let repo = Repo::new();
  let root = repo.root();
  let (proposal_id, reconciliation_id, authority_id) = repo.prepare();

  // Trusted handoff: no Proposal, Reconciliation, or Authority ID is passed;
  // the helper derives and re-verifies the exact chain itself.
  let admitted = ok_json(root, &["authority", "admit-prepared", "--json"]);
  assert_eq!(admitted["stage"], "ADMISSION");
  assert_eq!(admitted["authorityId"], authority_id);
  assert_eq!(admitted["admission"]["proposal"], proposal_id);
  assert_eq!(admitted["admission"]["reconciliation"], reconciliation_id);
  let admission_id = admitted["admissionId"].as_str().expect("admission id");

  // The agent resumes from the phase Tenet derives, not from the approval.
  let status = producer_ok_json(root, &["status", "--json"]);
  assert_eq!(status["phase"], "IMPLEMENTATION");
  assert_eq!(status["activeAdmissionId"], admission_id);
  assert!(
    status["admission"].is_null(),
    "the preview must disappear once the chain is admitted"
  );
  let inspected = ok_json(root, &["authority", "inspect", "--json"]);
  assert_eq!(inspected["active"]["admissionId"], admission_id);
  assert_eq!(inspected["active"]["grantProposal"], proposal_id);
  assert_eq!(inspected["active"]["grantAuthority"], authority_id);

  // CLI-only workflow continues to completion through the same kernel.
  let checked = ok_json(root, &["requirement", "check", "--id", "R1", "--json"]);
  assert_eq!(checked["result"]["state"], "satisfied");
  let verified = ok_json(root, &["verify", "--json"]);
  assert_eq!(verified["verdict"], "DONE");
  assert_eq!(verified["admissionId"], admission_id);
}

#[test]
fn admit_prepared_fails_closed_on_mismatched_identities() {
  let repo = Repo::new();
  let root = repo.root();
  let (_proposal_one, reconciliation_one, _authority_one) = repo.prepare();

  // A second, genuinely different proposal replaces the active refs.
  let mut diverged = contract();
  diverged.requirements[0].statement = "a different requirement statement".into();
  let contract_path = repo.write_contract(&diverged);
  let proposal_two = producer_ok_json(
    root,
    &[
      "authority",
      "prepare",
      "--contract",
      &contract_path,
      "--json",
    ],
  );
  let proposal_two_id = proposal_two["proposalId"]
    .as_str()
    .expect("proposal id")
    .to_owned();
  let authority_two_id = proposal_two["authorityId"]
    .as_str()
    .expect("authority id")
    .to_owned();
  assert_ne!(proposal_two_id, _proposal_one);
  let reconciliation_two = producer_ok_json(
    root,
    &[
      "authority",
      "reconcile",
      "--proposal",
      &proposal_two_id,
      "--json",
    ],
  );
  assert!(reconciliation_two["reconciliationId"].is_string());

  // Same-user ref tampering: navigation points at the other proposal's
  // report. The helper must derive the mismatch and refuse.
  fs::write(
    root.join(".tenet/refs/reconciliation"),
    format!("{reconciliation_one}\n"),
  )
  .expect("tamper reconciliation ref");
  let handoff = tenet_with_secret(
    root,
    &["authority", "admit-prepared", "--json"],
    Some(SECRET),
  );
  let error = error_json(&handoff);
  assert_eq!(error["code"], "admission_identity_mismatch");

  // The manual kernel path rejects the same cross-identity admission even
  // with a genuine grant for the active proposal.
  let grant = ok_json(
    root,
    &[
      "authority",
      "grant",
      "--proposal",
      &proposal_two_id,
      "--authority",
      &authority_two_id,
      "--json",
    ],
  );
  let grant_path = root.join("grant.json");
  fs::write(&grant_path, serde_json::to_vec(&grant).expect("grant json")).expect("write grant");
  let admitted = tenet_with_secret(
    root,
    &[
      "authority",
      "admit",
      "--proposal",
      &proposal_two_id,
      "--reconciliation",
      &reconciliation_one,
      "--authority",
      &authority_two_id,
      "--grant",
      &grant_path.to_string_lossy(),
      "--json",
    ],
    Some(SECRET),
  );
  let error = error_json(&admitted);
  assert_eq!(error["code"], "admission_invalid");
  assert!(
    error["message"]
      .as_str()
      .expect("message")
      .contains("reconciliation targets another proposal"),
    "unexpected kernel message: {}",
    error["message"]
  );

  // Nothing was admitted under either identity.
  let inspected = producer_ok_json(root, &["authority", "inspect", "--json"]);
  assert!(
    inspected["active"].is_null(),
    "a mismatched chain must never become the active admission"
  );
}

#[test]
fn admit_prepared_without_trusted_credentials_or_preconditions_fails_closed() {
  let repo = Repo::new();
  let root = repo.root();

  // A fresh repository: even a trusted process has nothing prepared to admit.
  let empty = tenet_with_secret(
    root,
    &["authority", "admit-prepared", "--json"],
    Some(SECRET),
  );
  let error = error_json(&empty);
  assert_eq!(error["code"], "admission_precondition_missing");

  // Proposal without reconciliation: still precondition-missing, fail closed.
  let contract_path = repo.write_contract(&contract());
  producer_ok_json(
    root,
    &[
      "authority",
      "prepare",
      "--contract",
      &contract_path,
      "--json",
    ],
  );
  let unreconciled = tenet_with_secret(
    root,
    &["authority", "admit-prepared", "--json"],
    Some(SECRET),
  );
  let error = error_json(&unreconciled);
  assert_eq!(error["code"], "admission_precondition_missing");
  let status = producer_ok_json(root, &["status", "--json"]);
  assert_eq!(status["phase"], "AUTHORITY_RECONCILIATION");
}
