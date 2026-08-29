use tenet_application::{
  application::{EvidenceRequest, GateRequest, Tenet},
  response::{AuthoritySealResult, CandidateCaptureResult},
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
