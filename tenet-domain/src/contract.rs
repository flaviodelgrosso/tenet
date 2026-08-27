use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::policy::VerificationPolicy;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RequirementId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ObligationId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContractProposal {
  pub schema_version: u32,
  pub spec_digest: String,
  pub policy_digest: String,
  pub requirements: Vec<Requirement>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Requirement {
  pub id: RequirementId,
  pub statement: String,
  pub obligations: Vec<VerificationObligation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerificationObligation {
  pub id: ObligationId,
  pub statement: String,
  pub evidence_contract: EvidenceContract,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceContract {
  pub verifier_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProposalRecord {
  pub proposal_id: String,
  pub proposal_digest: String,
  #[serde(flatten)]
  pub proposal: ContractProposal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmittedContract {
  pub schema_version: u32,
  pub proposal_id: String,
  pub proposal_digest: String,
  pub spec_digest: String,
  pub policy_digest: String,
  pub requirements: Vec<Requirement>,
}

impl From<ProposalRecord> for AdmittedContract {
  fn from(record: ProposalRecord) -> Self {
    Self {
      schema_version: record.proposal.schema_version,
      proposal_id: record.proposal_id,
      proposal_digest: record.proposal_digest,
      spec_digest: record.proposal.spec_digest,
      policy_digest: record.proposal.policy_digest,
      requirements: record.proposal.requirements,
    }
  }
}

impl AdmittedContract {
  pub fn obligations(&self) -> impl Iterator<Item = &VerificationObligation> {
    self.requirements.iter().flat_map(|item| &item.obligations)
  }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContractError {
  #[error("unsupported proposal schema version {0}")]
  UnsupportedVersion(u32),
  #[error("{kind} identifier must not be blank")]
  BlankId { kind: &'static str },
  #[error("duplicate {kind} identifier `{id}`")]
  DuplicateId { kind: &'static str, id: String },
  #[error("{kind} statement must not be blank for `{id}`")]
  BlankStatement { kind: &'static str, id: String },
  #[error("requirement `{0}` has no verification obligations")]
  MissingObligations(String),
  #[error("obligation `{obligation}` references unknown verifier `{verifier}`")]
  UnknownVerifier {
    obligation: String,
    verifier: String,
  },
  #[error("proposal contains no requirements")]
  MissingRequirements,
}

pub fn validate_proposal(
  proposal: &ContractProposal,
  policy: &VerificationPolicy,
) -> Result<(), ContractError> {
  if proposal.schema_version != 1 {
    return Err(ContractError::UnsupportedVersion(proposal.schema_version));
  }
  if proposal.requirements.is_empty() {
    return Err(ContractError::MissingRequirements);
  }

  let verifier_ids: BTreeSet<&str> = policy
    .verifiers
    .iter()
    .map(|verifier| verifier.id.as_str())
    .collect();
  let mut requirement_ids = BTreeSet::new();
  let mut obligation_ids = BTreeSet::new();

  for requirement in &proposal.requirements {
    validate_id("requirement", &requirement.id.0)?;
    if !requirement_ids.insert(requirement.id.0.as_str()) {
      return Err(ContractError::DuplicateId {
        kind: "requirement",
        id: requirement.id.0.clone(),
      });
    }
    validate_statement("requirement", &requirement.id.0, &requirement.statement)?;
    if requirement.obligations.is_empty() {
      return Err(ContractError::MissingObligations(requirement.id.0.clone()));
    }
    for obligation in &requirement.obligations {
      validate_id("obligation", &obligation.id.0)?;
      if !obligation_ids.insert(obligation.id.0.as_str()) {
        return Err(ContractError::DuplicateId {
          kind: "obligation",
          id: obligation.id.0.clone(),
        });
      }
      validate_statement("obligation", &obligation.id.0, &obligation.statement)?;
      if !verifier_ids.contains(obligation.evidence_contract.verifier_id.as_str()) {
        return Err(ContractError::UnknownVerifier {
          obligation: obligation.id.0.clone(),
          verifier: obligation.evidence_contract.verifier_id.clone(),
        });
      }
    }
  }
  Ok(())
}

fn validate_id(kind: &'static str, id: &str) -> Result<(), ContractError> {
  if id.trim().is_empty() {
    return Err(ContractError::BlankId { kind });
  }
  Ok(())
}

fn validate_statement(kind: &'static str, id: &str, value: &str) -> Result<(), ContractError> {
  if value.trim().is_empty() {
    return Err(ContractError::BlankStatement {
      kind,
      id: id.to_owned(),
    });
  }
  Ok(())
}
