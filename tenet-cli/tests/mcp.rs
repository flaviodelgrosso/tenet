use std::fs;

use tenet_application::{
  application::{EvidenceRequest, GateRequest, InitializeRequest, Tenet},
  response::{AuthoritySealResult, CandidateCaptureResult, ContractState},
};
use tenet_domain::evidence::{AuthorityId, CandidateId, ContentObjectId};

fn object(value: char) -> ContentObjectId {
  ContentObjectId(format!("sha256:{}", value.to_string().repeat(64)))
}

#[test]
fn public_requests_use_distinct_content_addressed_authority_and_candidate_ids() {
  let authority = AuthorityId(object('a'));
  let candidate = CandidateId(object('b'));
  let gate: GateRequest = serde_json::from_value(serde_json::json!({
    "authorityId": authority.0.0,
    "candidateId": candidate.0.0,
  }))
  .expect("deserialize gate");
  assert_eq!(gate.authority_id, authority);
  assert_eq!(gate.candidate_id, candidate.clone());
  let evidence: EvidenceRequest = serde_json::from_value(serde_json::json!({
    "authorityId": authority.0.0,
    "candidateId": candidate.0.0,
  }))
  .expect("deserialize evidence");
  assert_eq!(evidence.candidate_id, candidate);
  let _seal: Option<AuthoritySealResult> = None;
  let _capture: Option<CandidateCaptureResult> = None;
}

#[test]
fn policy_schema_documents_distinct_verifier_execution_roots() {
  let schema = Tenet::new(std::path::PathBuf::new()).policy_schema();
  let encoded = serde_json::to_string(&schema).expect("serialize policy schema");
  assert!(encoded.contains("ordinary PATH/absolute-path semantics apply"));
  assert!(encoded.contains("sealed oracle bundle"));
}

#[test]
fn fresh_empty_verifier_project_skill_constructs_authority_before_candidate() {
  let directory = tempfile::tempdir().expect("project");
  let root = directory.path();
  fs::write(
    root.join("SPEC.md"),
    "# Completion specification\n\nThe candidate must satisfy the stated behavior.\n",
  )
  .expect("specification");

  let tenet = Tenet::new(root.to_path_buf());
  let initialized = tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("initialize");
  assert_eq!(initialized.contract_state, ContractState::Missing);
  assert_eq!(
    fs::read_to_string(root.join(".tenet/tenet.toml"))
      .expect("read policy")
      .lines()
      .find(|line| line.starts_with("verifiers"))
      .expect("verifier list"),
    "verifiers = []"
  );
  assert!(!root.join("candidate").exists());

  let skill = fs::read_to_string(root.join(".agents/skills/tenet/SKILL.md")).expect("read skill");
  for instruction in [
    "Keep **authority construction** separate from **candidate engineering**",
    "Before `tenet_contract_propose`, ensure `.tenet/tenet.toml` contains suitable verifier definitions",
    "call `tenet_policy_schema` and use its Rust-derived schema as the authoritative policy format",
    "create the required authority-owned oracle assets",
    "re-read `tenet_status` after every authority change",
    "propose the contract using those verifier IDs",
    "Do not implement candidate product behavior during authority construction",
    "Editing verification policy and creating authority-owned oracle assets are **authority-definition work** and are allowed before A is sealed",
    "weak configuration is surfaced through the returned verification profile and warnings",
    "Project-authority verifiers remain valid",
  ] {
    assert!(
      skill.contains(instruction),
      "missing skill instruction: {instruction}"
    );
  }
  assert!(
    skill
      .find("## Authority construction")
      .expect("authority section")
      < skill
        .find("## Sealed-authority lifecycle")
        .expect("lifecycle section")
  );
  assert!(!skill.contains("cargo test"));
  assert!(!skill.contains("go test"));
  assert!(!skill.contains("pytest"));
}
