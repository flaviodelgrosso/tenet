use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tenet_domain::{
  algebra::{
    CompletionContractV1, CompletionEvaluation, CompletionPolicyId, CompletionState, Evaluation,
    EvaluationId,
  },
  authority::{
    Admission, AdmissionId, AuthorityProposal, Clarification, ClarificationId, Finding, Issue,
    ProposalId, ReconciliationReport, ReconciliationReportId,
  },
  completion::Verdict,
  contract::RequirementId,
  evidence::{AuthorityId, CandidateId, ContentObjectId, ExecutionEnvironmentIdentity},
  protocol::WorkflowPhase,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TenetError {
  pub code: String,
  pub message: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub verifier_id: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub path: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub details: Option<Box<serde_json::Value>>,
}

impl TenetError {
  pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      code: code.into(),
      message: message.into(),
      verifier_id: None,
      path: None,
      details: None,
    }
  }

  pub fn with_context(
    mut self,
    verifier_id: Option<String>,
    path: Option<String>,
    details: Option<serde_json::Value>,
  ) -> Self {
    self.verifier_id = verifier_id;
    self.path = path;
    self.details = details.map(Box::new);
    self
  }
}

impl fmt::Display for TenetError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "{}: {}", self.code, self.message)
  }
}

impl std::error::Error for TenetError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InitResult {
  pub schema_version: u32,
  pub initialized: bool,
  pub created: bool,
  pub spec_path: String,
  pub spec_digest: String,
  pub skill_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
  tag = "stage",
  rename_all = "SCREAMING_SNAKE_CASE",
  rename_all_fields = "camelCase"
)]
pub enum AuthoritySubmissionResult {
  Proposal {
    schema_version: u32,
    proposal_id: ProposalId,
    authority_id: AuthorityId,
    proposal: AuthorityProposal,
    contract: CompletionContractV1,
  },
  Reconciliation {
    schema_version: u32,
    reconciliation_id: ReconciliationReportId,
    report: ReconciliationReport,
  },
  Clarification {
    schema_version: u32,
    clarification_id: ClarificationId,
    clarification: Clarification,
  },
  Admission {
    schema_version: u32,
    admission_id: AdmissionId,
    authority_id: AuthorityId,
    admission: Admission,
  },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementStatus {
  pub requirement_id: RequirementId,
  pub evaluation_id: EvaluationId,
  pub candidate_id: CandidateId,
  pub state: CompletionState,
}

/// Read-only project facts needed before an Authority Proposal can bind the
/// configured Candidate and verifier surfaces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoringReadiness {
  pub config_path: String,
  pub candidate_capture_configured: bool,
  pub verifier_ids: Vec<String>,
  pub missing_prerequisites: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextResult {
  pub schema_version: u32,
  pub phase: WorkflowPhase,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub active_admission_id: Option<AdmissionId>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub authority_id: Option<AuthorityId>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub completion_policy_id: Option<CompletionPolicyId>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub current_candidate_id: Option<CandidateId>,
  pub requirement_checks: Vec<RequirementStatus>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub authoring: Option<AuthoringReadiness>,
  pub next_action: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementCheckResult {
  pub schema_version: u32,
  pub admission_id: AdmissionId,
  pub authority_id: AuthorityId,
  pub candidate_id: CandidateId,
  pub evaluation_id: EvaluationId,
  pub evaluation: Evaluation,
  pub result: CompletionEvaluation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerifyResult {
  pub schema_version: u32,
  pub admission_id: AdmissionId,
  pub authority_id: AuthorityId,
  pub candidate_id: CandidateId,
  pub evaluation_id: EvaluationId,
  pub evaluation: Evaluation,
  pub verdict: Verdict,
  pub result: CompletionEvaluation,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub reason: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub current_candidate_id: Option<CandidateId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReceiptVerificationResult {
  pub schema_version: u32,
  pub receipt_id: EvaluationId,
  pub authority_id: AuthorityId,
  pub candidate_id: CandidateId,
  pub contract_digest: ContentObjectId,
  pub completion_policy_id: CompletionPolicyId,
  pub evidence_set_digest: ContentObjectId,
  pub verification_environment_ids: Vec<ExecutionEnvironmentIdentity>,
  pub verdict: Verdict,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DoctorCheck {
  pub name: String,
  pub passed: bool,
  pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DoctorResult {
  pub schema_version: u32,
  pub healthy: bool,
  pub checks: Vec<DoctorCheck>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorResult {
  pub schema_version: u32,
  pub code: String,
  pub message: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub verifier_id: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub path: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub details: Option<serde_json::Value>,
}

impl From<TenetError> for ErrorResult {
  fn from(error: TenetError) -> Self {
    Self {
      schema_version: 1,
      code: error.code,
      message: error.message,
      verifier_id: error.verifier_id,
      path: error.path,
      details: error.details.map(|details| *details),
    }
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProposalInspection {
  pub proposal_id: ProposalId,
  pub authority_id: AuthorityId,
  pub issues: Vec<Issue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconciliationInspection {
  pub reconciliation_id: ReconciliationReportId,
  pub proposal_id: ProposalId,
  pub findings: Vec<Finding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActiveAdmissionInspection {
  pub admission_id: AdmissionId,
  pub authority_id: AuthorityId,
  pub spec_path: String,
  pub spec_digest: String,
  pub contract_digest: ContentObjectId,
  pub completion_policy_id: CompletionPolicyId,
  pub verifiers: Vec<String>,
  pub grant_proposal: ProposalId,
  pub grant_authority: AuthorityId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityInspectionResult {
  pub schema_version: u32,
  pub proposal: Option<ProposalInspection>,
  pub reconciliation: Option<ReconciliationInspection>,
  pub active: Option<ActiveAdmissionInspection>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Blocker {
  pub code: String,
  pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlockersResult {
  pub schema_version: u32,
  pub phase: WorkflowPhase,
  pub blockers: Vec<Blocker>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceReport {
  pub schema_version: u32,
  pub evaluation_id: EvaluationId,
  pub evaluation: Evaluation,
  pub result: CompletionEvaluation,
}
