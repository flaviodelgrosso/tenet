use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::policy::{VerificationPolicy, VerifierAuthority};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RequirementId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ObligationId(pub String);
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct AssuranceId(pub String);

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
  pub claim: ClaimEvidenceContract,
  #[serde(default)]
  pub assurances: Vec<OracleAssuranceContract>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaimEvidenceContract {
  pub verifier_id: String,
  pub authority: VerifierAuthority,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OracleAssuranceContract {
  pub id: AssuranceId,
  pub criterion: String,
  pub verifier_id: String,
  pub authority: VerifierAuthority,
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
  #[error(
    "obligation `{obligation}` requires verifier authority `{required:?}` but verifier `{verifier}` has authority `{configured:?}`"
  )]
  VerifierAuthorityMismatch {
    obligation: String,
    verifier: String,
    required: VerifierAuthority,
    configured: VerifierAuthority,
  },
  #[error(
    "assurance `{assurance}` for obligation `{obligation}` must use authority_snapshot authority"
  )]
  AssuranceNotIndependent {
    obligation: String,
    assurance: String,
  },
  #[error("assurance `{assurance}` for obligation `{obligation}` cannot use the primary oracle `{verifier}`")]
  OracleSelfCertification {
    obligation: String,
    assurance: String,
    verifier: String,
  },
  #[error("proposal contains no requirements")]
  MissingRequirements,
}

pub fn validate_proposal(
  proposal: &ContractProposal,
  policy: &VerificationPolicy,
) -> Result<(), ContractError> {
  if proposal.schema_version != 2 {
    return Err(ContractError::UnsupportedVersion(proposal.schema_version));
  }
  if proposal.requirements.is_empty() {
    return Err(ContractError::MissingRequirements);
  }

  let mut requirement_ids = BTreeSet::new();
  let mut obligation_ids = BTreeSet::new();
  let mut assurance_ids = BTreeSet::new();

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
      let primary = &obligation.evidence_contract.claim;
      let verifier = validate_verifier_mapping(
        &obligation.id.0,
        primary.verifier_id.as_str(),
        primary.authority,
        policy,
      )?;
      for assurance in &obligation.evidence_contract.assurances {
        validate_id("assurance", &assurance.id.0)?;
        if !assurance_ids.insert(assurance.id.0.as_str()) {
          return Err(ContractError::DuplicateId {
            kind: "assurance",
            id: assurance.id.0.clone(),
          });
        }
        validate_statement("assurance criterion", &assurance.id.0, &assurance.criterion)?;
        let assurance_verifier = validate_verifier_mapping(
          &obligation.id.0,
          assurance.verifier_id.as_str(),
          assurance.authority,
          policy,
        )?;
        if assurance.authority != VerifierAuthority::AuthoritySnapshot
          || assurance_verifier.authority != VerifierAuthority::AuthoritySnapshot
        {
          return Err(ContractError::AssuranceNotIndependent {
            obligation: obligation.id.0.clone(),
            assurance: assurance.id.0.clone(),
          });
        }
        let same_verifier = primary.verifier_id == assurance.verifier_id;
        let same_snapshot_oracle = verifier.authority == VerifierAuthority::AuthoritySnapshot
          && (same_relative_path(&verifier.oracle_path, &assurance_verifier.oracle_path)
            || same_oracle_executable(verifier, assurance_verifier));
        if same_verifier || same_snapshot_oracle {
          return Err(ContractError::OracleSelfCertification {
            obligation: obligation.id.0.clone(),
            assurance: assurance.id.0.clone(),
            verifier: assurance.verifier_id.clone(),
          });
        }
      }
    }
  }
  Ok(())
}
fn validate_verifier_mapping<'a>(
  obligation: &str,
  verifier_id: &str,
  authority: VerifierAuthority,
  policy: &'a VerificationPolicy,
) -> Result<&'a crate::policy::VerifierSpec, ContractError> {
  let Some(verifier) = policy
    .verifiers
    .iter()
    .find(|verifier| verifier.id == verifier_id)
  else {
    return Err(ContractError::UnknownVerifier {
      obligation: obligation.into(),
      verifier: verifier_id.into(),
    });
  };
  if authority != verifier.authority {
    return Err(ContractError::VerifierAuthorityMismatch {
      obligation: obligation.into(),
      verifier: verifier.id.clone(),
      required: authority,
      configured: verifier.authority,
    });
  }
  Ok(verifier)
}
fn same_relative_path(left: &Option<String>, right: &Option<String>) -> bool {
  match (left, right) {
    (Some(left), Some(right)) => normalized_relative_path(left) == normalized_relative_path(right),
    _ => false,
  }
}

fn same_oracle_executable(
  left: &crate::policy::VerifierSpec,
  right: &crate::policy::VerifierSpec,
) -> bool {
  left
    .oracle_executable_path()
    .is_some_and(|left| Some(left) == right.oracle_executable_path())
}

fn normalized_relative_path(value: &str) -> std::path::PathBuf {
  std::path::Path::new(value)
    .components()
    .filter(|component| *component != std::path::Component::CurDir)
    .collect()
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
