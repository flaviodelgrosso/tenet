use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tenet_domain::{
  completion::{Blocker, ObligationResult, Verdict},
  contract::{ContractProposal, ProposalWarning, VerificationProfile},
  evidence::{AuthorityId, CandidateId, EvidenceArtifact},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InitResult {
  pub schema_version: u32,
  pub initialized: bool,
  pub created: bool,
  pub spec_path: String,
  pub spec_digest: String,
  pub contract_state: ContractState,
  pub skill_path: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContractState {
  Missing,
  PendingApproval,
  Admitted,
  Stale,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProposalResult {
  pub schema_version: u32,
  pub proposal_id: String,
  pub proposal_digest: String,
  pub approval_required: bool,
  pub proposal: ContractProposal,
  pub verification_profile: VerificationProfile,
  pub warnings: Vec<ProposalWarning>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalResult {
  pub schema_version: u32,
  pub proposal_id: String,
  pub proposal_digest: String,
  pub contract_digest: String,
  pub contract_path: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoritySealResult {
  pub schema_version: u32,
  pub authority_id: AuthorityId,
  pub specification_digest: String,
  pub policy_digest: String,
  pub contract_digest: String,
  pub oracle_bundle_paths: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateCaptureResult {
  pub schema_version: u32,
  pub candidate_id: CandidateId,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatusResult {
  pub schema_version: u32,
  pub initialized: bool,
  pub spec_path: Option<String>,
  pub spec_digest: Option<String>,
  pub policy_digest: Option<String>,
  pub contract_state: ContractState,
  pub contract_digest: Option<String>,
  pub last_gated_authority_id: Option<AuthorityId>,
  pub last_gated_candidate_id: Option<CandidateId>,
  pub last_verdict: Option<Verdict>,
  pub unresolved_obligations: Vec<ObligationResult>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GateResult {
  pub schema_version: u32,
  pub authority_id: AuthorityId,
  pub candidate_id: CandidateId,
  pub spec_digest: String,
  pub contract_digest: String,
  pub policy_digest: String,
  pub verdict: Verdict,
  pub obligations: Vec<ObligationResult>,
  pub blockers: Vec<Blocker>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceResult {
  pub schema_version: u32,
  pub candidate_id: Option<CandidateId>,
  pub artifacts: Vec<EvidenceArtifact>,
  pub gates: Vec<GateResult>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorResult {
  pub schema_version: u32,
  pub code: String,
  pub message: String,
}
