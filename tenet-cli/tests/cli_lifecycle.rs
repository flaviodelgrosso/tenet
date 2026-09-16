//! End-to-end canonical-lifecycle tests for the `tenet` binary.
//!
//! The complete lifecycle must be drivable through the CLI+JSON surface with
//! no MCP process involved, with kernel-identical semantics and exit codes
//! that distinguish success, NOT_DONE, infrastructure failure, and invalid
//! input.

use std::{
  fs,
  io::Write,
  path::Path,
  process::{Command, Stdio},
};

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

fn contract(verifier: &str) -> CompletionContractV1 {
  CompletionContractV1 {
    schema_version: 1,
    policy: CompletionPolicyId(COMPLETION_POLICY_V1.into()),
    requirements: vec![Requirement {
      id: tenet_domain::contract::RequirementId("R1".into()),
      statement: "the candidate is complete".into(),
      criteria: vec![Criterion {
        id: CriterionId("C1".into()),
        proposition: "criterion C1 holds".into(),
        verifiers: vec![Verifier {
          id: VerifierId(verifier.into()),
          material: VerifierMaterial::Candidate,
        }],
        evidence: EvidenceRequirementV1 {
          control: EvidenceControlRequirementV1::CandidateControlledPermitted,
          assurance: AssuranceRequirementV1::LocalOrStronger,
        },
      }],
    }],
  }
}

struct Repo {
  directory: tempfile::TempDir,
}

impl Repo {
  /// `mode` selects the verifier program: `"pass"`/`"fail"` write a candidate
  /// script; anything else is a literal program that cannot be resolved.
  fn new(mode: &str) -> Self {
    let directory = tempfile::tempdir().expect("repository");
    let root = directory.path();
    fs::write(root.join("SPEC.md"), "# Specification\n").expect("spec");
    let output = tenet_with_secret(root, &["init", "--json"], Some(SECRET));
    assert!(output.status.success(), "init failed");
    fs::write(root.join("candidate.txt"), "original candidate").expect("candidate");
    let (argv, include) = match mode {
      "pass" | "fail" => {
        let script = format!("#!/bin/sh\nexit {}\n", if mode == "pass" { 0 } else { 1 });
        std::fs::write(root.join("verify.sh"), script).expect("verify script");
        std::fs::set_permissions(
          root.join("verify.sh"),
          std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("executable");
        (
          vec![tenet_domain::policy::CommandArgument::CandidatePath(
            "verify.sh".into(),
          )],
          vec!["candidate.txt".to_string(), "verify.sh".to_string()],
        )
      }
      missing => (
        vec![tenet_domain::policy::CommandArgument::Literal(
          missing.into(),
        )],
        vec!["candidate.txt".to_string()],
      ),
    };
    let policy = ProjectConfig {
      version: 1,
      spec_path: "SPEC.md".into(),
      candidate: CandidateCapturePolicy {
        root: ".".into(),
        include,
        exclude: vec![],
      },
      verifiers: vec![tenet_domain::policy::VerifierSpec {
        id: "V1".into(),
        command: tenet_domain::policy::CommandSpec {
          argv,
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
      }],
    };
    fs::write(
      root.join(".tenet/tenet.toml"),
      toml::to_string_pretty(&policy).expect("policy"),
    )
    .expect("write policy");
    fs::write(
      root.join("contract.json"),
      serde_json::to_vec_pretty(&contract("V1")).expect("contract"),
    )
    .expect("write contract");
    Self { directory }
  }

  fn root(&self) -> &Path {
    self.directory.path()
  }

  fn contract_path(&self) -> String {
    self
      .root()
      .join("contract.json")
      .to_string_lossy()
      .into_owned()
  }

  fn grant_path(&self) -> String {
    self
      .root()
      .join("grant.json")
      .to_string_lossy()
      .into_owned()
  }

  /// Drive the full lifecycle through the CLI: prepare, reconcile, grant,
  /// admit. Returns `(proposal_id, reconciliation_id, authority_id,
  /// admission_id)`.
  fn admit(&self) -> (String, String, String, String) {
    let root = self.root();
    let contract_path = self.contract_path();
    let proposal = ok_json(
      root,
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
    let reconciliation = ok_json(
      root,
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
    let grant = ok_json(
      root,
      &[
        "authority",
        "grant",
        "--proposal",
        &proposal_id,
        "--authority",
        &authority_id,
        "--json",
      ],
    );
    let grant_path = self.grant_path();
    fs::write(&grant_path, serde_json::to_vec(&grant).expect("grant json")).expect("write grant");
    let admitted = ok_json(
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
        &grant_path,
        "--json",
      ],
    );
    let admission_id = admitted["admissionId"]
      .as_str()
      .expect("admission id")
      .to_owned();
    (proposal_id, reconciliation_id, authority_id, admission_id)
  }
}

#[test]
fn full_lifecycle_completes_through_the_cli_without_mcp() {
  let repo = Repo::new("pass");
  let (proposal_id, _, authority_id, admission_id) = repo.admit();

  let checked = ok_json(
    repo.root(),
    &["requirement", "check", "--id", "R1", "--json"],
  );
  assert_eq!(checked["result"]["state"], "satisfied");

  let verified = ok_json(repo.root(), &["verify", "--json"]);
  assert_eq!(verified["verdict"], "DONE");
  assert_eq!(verified["admissionId"], admission_id);
  assert_eq!(verified["authorityId"], authority_id);
  let evaluation_id = verified["evaluationId"]
    .as_str()
    .expect("evaluation id")
    .to_owned();
  let candidate_id = verified["candidateId"]
    .as_str()
    .expect("candidate id")
    .to_owned();

  let receipt = ok_json(
    repo.root(),
    &["receipt", "verify", "--id", &evaluation_id, "--json"],
  );
  assert_eq!(receipt["verdict"], "DONE");
  assert_eq!(receipt["candidateId"], candidate_id);

  let blockers = ok_json(repo.root(), &["blockers", "--json"]);
  assert_eq!(blockers["phase"], "COMPLETED");
  assert_eq!(blockers["blockers"].as_array().expect("blockers").len(), 0);

  let evidence = ok_json(repo.root(), &["evidence", "--json"]);
  assert_eq!(evidence["evaluationId"], evaluation_id);
  assert_eq!(evidence["result"]["verdict"], "DONE");

  let status = ok_json(repo.root(), &["status", "--json"]);
  assert_eq!(status["phase"], "COMPLETED");
  assert_eq!(status["currentCandidateId"], candidate_id);

  let inspected = ok_json(repo.root(), &["authority", "inspect", "--json"]);
  assert_eq!(inspected["active"]["admissionId"], admission_id);
  assert_eq!(inspected["active"]["grantProposal"], proposal_id);
  assert_eq!(inspected["active"]["grantAuthority"], authority_id);
}

#[test]
fn failing_verifier_yields_not_done_exit_code() {
  let repo = Repo::new("fail");
  repo.admit();
  let output = tenet_with_secret(repo.root(), &["verify", "--json"], Some(SECRET));
  assert_eq!(output.status.code(), Some(2));
  let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
  assert_eq!(result["verdict"], "NOT_DONE");
}

#[test]
fn infrastructure_failure_yields_exit_code_four() {
  let repo = Repo::new("tenet-definitely-not-a-real-program-xyz");
  repo.admit();
  let output = tenet_with_secret(repo.root(), &["verify", "--json"], Some(SECRET));
  assert_eq!(output.status.code(), Some(4));
  let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
  assert_eq!(result["verdict"], "INFRASTRUCTURE_ERROR");
}

#[test]
fn invalid_identity_input_yields_exit_code_one() {
  let repo = Repo::new("pass");
  let output = tenet_with_secret(
    repo.root(),
    &[
      "authority",
      "reconcile",
      "--proposal",
      "not-a-content-id",
      "--json",
    ],
    Some(SECRET),
  );
  assert_eq!(output.status.code(), Some(1));
  let error: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
  assert!(
    error["message"]
      .as_str()
      .expect("message")
      .contains("sha256:")
  );
}

#[test]
fn grant_and_admission_without_the_secret_fail_closed() {
  let repo = Repo::new("pass");
  let root = repo.root();
  let proposal = ok_json(
    root,
    &[
      "authority",
      "prepare",
      "--contract",
      &repo.contract_path(),
      "--json",
    ],
  );
  let proposal_id = proposal["proposalId"].as_str().unwrap().to_owned();
  let authority_id = proposal["authorityId"].as_str().unwrap().to_owned();
  ok_json(
    root,
    &[
      "authority",
      "reconcile",
      "--proposal",
      &proposal_id,
      "--json",
    ],
  );

  let grant = tenet_with_secret(
    root,
    &[
      "authority",
      "grant",
      "--proposal",
      &proposal_id,
      "--authority",
      &authority_id,
      "--json",
    ],
    None,
  );
  assert_eq!(grant.status.code(), Some(1));
  let error: serde_json::Value = serde_json::from_slice(&grant.stdout).expect("JSON");
  assert_eq!(error["code"], "admission_secret_unavailable");

  // A producer that captured a structurally valid grant cannot admit either:
  // the admitting process also needs the trusted secret.
  let fabricated = serde_json::json!({
    "schemaVersion": 1,
    "semantics": "tenet:admission-grant:v1",
    "proposal": proposal_id,
    "authority": authority_id,
    "mac": "0".repeat(64),
  });
  let forged_path = root.join("forged.json");
  fs::write(&forged_path, serde_json::to_vec(&fabricated).unwrap()).expect("forged");
  let reconciliation =
    ok_json(root, &["authority", "inspect", "--json"])["reconciliation"]["reconciliationId"]
      .as_str()
      .expect("reconciliation id")
      .to_owned();
  let admitted = tenet_with_secret(
    root,
    &[
      "authority",
      "admit",
      "--proposal",
      &proposal_id,
      "--reconciliation",
      &reconciliation,
      "--authority",
      &authority_id,
      "--grant",
      &forged_path.to_string_lossy(),
      "--json",
    ],
    None,
  );
  assert_eq!(admitted.status.code(), Some(1));
  let error: serde_json::Value = serde_json::from_slice(&admitted.stdout).expect("JSON");
  assert_eq!(error["code"], "admission_secret_unavailable");
}

#[test]
fn verification_without_the_secret_fails_closed_after_admission() {
  let repo = Repo::new("pass");
  repo.admit();
  // A producer process without the trusted secret cannot derive completion
  // from persisted state: the grant mac cannot be re-verified, so every
  // verification-affecting operation fails closed instead of trusting the
  // persisted chain.
  for args in [
    vec!["verify", "--json"],
    vec!["requirement", "check", "--id", "R1", "--json"],
  ] {
    let output = tenet_with_secret(repo.root(), &args, None);
    assert_eq!(
      output.status.code(),
      Some(1),
      "args {args:?} must fail closed"
    );
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(error["code"], "admission_secret_unavailable");
  }
  // A receipt is the historical `DONE` surface; it also requires the secret.
  let verified = ok_json(repo.root(), &["verify", "--json"]);
  let evaluation_id = verified["evaluationId"].as_str().expect("evaluation id");
  let receipt = tenet_with_secret(
    repo.root(),
    &["receipt", "verify", "--id", evaluation_id, "--json"],
    None,
  );
  assert_eq!(receipt.status.code(), Some(1));
  let error: serde_json::Value = serde_json::from_slice(&receipt.stdout).expect("JSON");
  assert_eq!(error["code"], "admission_secret_unavailable");
}

#[test]
fn forged_grant_via_cli_is_rejected_with_the_trusted_secret() {
  let repo = Repo::new("pass");
  let root = repo.root();
  let proposal = ok_json(
    root,
    &[
      "authority",
      "prepare",
      "--contract",
      &repo.contract_path(),
      "--json",
    ],
  );
  let proposal_id = proposal["proposalId"].as_str().unwrap().to_owned();
  let authority_id = proposal["authorityId"].as_str().unwrap().to_owned();
  ok_json(
    root,
    &[
      "authority",
      "reconcile",
      "--proposal",
      &proposal_id,
      "--json",
    ],
  );
  let reconciliation =
    ok_json(root, &["authority", "inspect", "--json"])["reconciliation"]["reconciliationId"]
      .as_str()
      .expect("reconciliation id")
      .to_owned();
  let fabricated = serde_json::json!({
    "schemaVersion": 1,
    "semantics": "tenet:admission-grant:v1",
    "proposal": proposal_id,
    "authority": authority_id,
    "mac": "ab".repeat(32),
  });
  let forged_path = root.join("forged.json");
  fs::write(&forged_path, serde_json::to_vec(&fabricated).unwrap()).expect("forged");
  let admitted = tenet_with_secret(
    root,
    &[
      "authority",
      "admit",
      "--proposal",
      &proposal_id,
      "--reconciliation",
      &reconciliation,
      "--authority",
      &authority_id,
      "--grant",
      &forged_path.to_string_lossy(),
      "--json",
    ],
    Some(SECRET),
  );
  assert_eq!(admitted.status.code(), Some(1));
  let error: serde_json::Value = serde_json::from_slice(&admitted.stdout).expect("JSON");
  assert_eq!(error["code"], "admission_grant_invalid");
}

#[test]
fn mcp_verify_matches_the_cli_completion_derivation() {
  let repo = Repo::new("pass");
  repo.admit();
  let verified = ok_json(repo.root(), &["verify", "--json"]);
  assert_eq!(verified["verdict"], "DONE");

  // A separate MCP process over the same repository derives the same verdict
  // and Candidate identity: adapters share kernel semantics, not state.
  // The MCP server is a trusted operator process: deriving `DONE` re-verifies
  // the persisted grant mac, so the operator supplies the trusted secret to it.
  let mut child = Command::new(env!("CARGO_BIN_EXE_tenet"))
    .arg("--cwd")
    .arg(repo.root())
    .env("TENET_ADMISSION_SECRET", SECRET)
    .arg("mcp")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .spawn()
    .expect("spawn mcp");
  let input = concat!(
    "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n",
    "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
    "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"tenet_verify\",\"arguments\":{}}}\n",
  );
  child
    .stdin
    .take()
    .expect("stdin")
    .write_all(input.as_bytes())
    .expect("write");
  let output = child.wait_with_output().expect("mcp output");
  let response: serde_json::Value = String::from_utf8_lossy(&output.stdout)
    .lines()
    .filter_map(|line| serde_json::from_str(line).ok())
    .find(|message: &serde_json::Value| message.get("id") == Some(&serde_json::json!(2)))
    .expect("tools/call response");
  let text = response["result"]["content"][0]["text"]
    .as_str()
    .expect("tool result text");
  let verdict: serde_json::Value = serde_json::from_str(text).expect("verify result JSON");
  assert_eq!(verdict["verdict"], "DONE");
  assert_eq!(verdict["candidateId"], verified["candidateId"]);
  assert_eq!(verdict["admissionId"], verified["admissionId"]);
}

#[test]
fn mcp_admission_enforces_the_same_grant_boundary() {
  let repo = Repo::new("pass");
  let root = repo.root();
  let proposal = ok_json(
    root,
    &[
      "authority",
      "prepare",
      "--contract",
      &repo.contract_path(),
      "--json",
    ],
  );
  let proposal_id = proposal["proposalId"].as_str().unwrap().to_owned();
  let authority_id = proposal["authorityId"].as_str().unwrap().to_owned();
  ok_json(
    root,
    &[
      "authority",
      "reconcile",
      "--proposal",
      &proposal_id,
      "--json",
    ],
  );

  let submission = serde_json::json!({
    "stage": "ADMISSION",
    "proposalId": proposal_id,
    "reconciliationId": format!("sha256:{}", "b".repeat(64)),
    "authorityId": authority_id,
    "grant": {
      "schemaVersion": 1,
      "semantics": "tenet:admission-grant:v1",
      "proposal": proposal_id,
      "authority": authority_id,
      "mac": "cd".repeat(32),
    },
  });
  let call = serde_json::json!({
    "jsonrpc": "2.0",
    "id": 2,
    "method": "tools/call",
    "params": { "name": "tenet_authority_submit", "arguments": { "submission": submission } },
  });
  let mut child = Command::new(env!("CARGO_BIN_EXE_tenet"))
    .arg("--cwd")
    .arg(root)
    .env("TENET_ADMISSION_SECRET", SECRET)
    .arg("mcp")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .spawn()
    .expect("spawn mcp");
  let input = format!(
    concat!(
      "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{{}},\"clientInfo\":{{\"name\":\"test\",\"version\":\"1\"}}}}}}\n",
      "{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}}\n",
      "{call}\n",
    ),
    call = call
  );
  child
    .stdin
    .take()
    .expect("stdin")
    .write_all(input.as_bytes())
    .expect("write");
  let output = child.wait_with_output().expect("mcp output");
  let response: serde_json::Value = String::from_utf8_lossy(&output.stdout)
    .lines()
    .filter_map(|line| serde_json::from_str(line).ok())
    .find(|message: &serde_json::Value| message.get("id") == Some(&serde_json::json!(2)))
    .expect("tools/call response");
  let encoded = response.to_string();
  assert!(
    encoded.contains("admission_grant_invalid"),
    "expected the same kernel grant rejection through MCP: {encoded}"
  );
  // The producer-facing surface never admits through a forged grant.
  let status = ok_json(root, &["status", "--json"]);
  assert_eq!(status["phase"], "AUTHORITY_ADMISSION");
}
