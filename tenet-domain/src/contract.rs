use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::policy::{VerificationPolicy, VerifierAuthority};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RequirementId(#[schemars(length(min = 1))] pub String);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ObligationId(#[schemars(length(min = 1))] pub String);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct AssuranceId(#[schemars(length(min = 1))] pub String);

pub const CONTRACT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContractProposalInput {
  #[schemars(length(min = 1))]
  pub spec_digest: String,
  #[schemars(length(min = 1))]
  pub policy_digest: String,
  #[schemars(length(min = 1))]
  pub requirements: Vec<RequirementInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementInput {
  pub id: RequirementId,
  #[schemars(
    length(min = 1),
    description = "Semantic requirement derived from the specification: state what the product or system must do, not how Tenet will verify it."
  )]
  pub statement: String,
  #[schemars(length(min = 1))]
  pub obligations: Vec<VerificationObligationInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerificationObligationInput {
  pub id: ObligationId,
  #[schemars(
    length(min = 1),
    description = "One independently falsifiable property of Candidate Snapshot R required for completion. Describe candidate or system behavior only; do not describe tests, verifier execution, or evidence collection."
  )]
  pub statement: String,
  pub evidence_contract: EvidenceContractInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceContractInput {
  #[schemars(
    description = "Evidence mechanism for the semantic obligation: declares how Tenet obtains evidence for the property, separately from the obligation statement."
  )]
  pub claim: ClaimEvidenceContractInput,
  #[serde(default)]
  #[schemars(
    description = "Optional assurance mechanisms and criteria about evidence quality or the primary oracle; these are not additional statements of candidate behavior."
  )]
  pub oracle_assurances: Vec<OracleAssuranceContractInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaimEvidenceContractInput {
  #[schemars(
    length(min = 1),
    description = "Primary verifier mechanism used to obtain evidence for the obligation. This identifies evidence collection, not the semantic claim about Candidate Snapshot R."
  )]
  pub verifier_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OracleAssuranceContractInput {
  pub id: AssuranceId,
  #[schemars(
    length(min = 1),
    description = "Criterion about the primary oracle or evidence quality, not another statement of candidate or system behavior."
  )]
  pub criterion: String,
  #[schemars(length(min = 1))]
  pub verifier_id: String,
}

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
  #[schemars(length(min = 1))]
  pub statement: String,
  #[schemars(length(min = 1))]
  pub obligations: Vec<VerificationObligation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerificationObligation {
  pub id: ObligationId,
  #[schemars(length(min = 1))]
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
  #[schemars(length(min = 1))]
  pub verifier_id: String,
  pub authority: VerifierAuthority,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OracleAssuranceContract {
  pub id: AssuranceId,
  #[schemars(length(min = 1))]
  pub criterion: String,
  #[schemars(length(min = 1))]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerificationProfile {
  pub obligation_count: usize,
  pub primary_verifier_count: usize,
  pub configured_verifier_count: usize,
  pub used_verifier_count: usize,
  pub project_authority_obligations: usize,
  pub authority_snapshot_obligations: usize,
  pub oracle_assured_obligations: usize,
  pub oracle_assurance_count: usize,
  pub primary_verifier_obligations: BTreeMap<String, usize>,
  pub unused_configured_verifiers: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProposalWarningCode {
  ProjectOracleOnly,
  OracleConcentration,
  NoOracleAssurance,
  UnusedVerifier,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProposalWarning {
  pub code: ProposalWarningCode,
  pub message: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub verifier_id: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub affected_obligation_count: Option<usize>,
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
  #[error(
    "assurance `{assurance}` for obligation `{obligation}` cannot use the primary oracle `{verifier}`"
  )]
  OracleSelfCertification {
    obligation: String,
    assurance: String,
    verifier: String,
  },
  #[error("proposal contains no requirements")]
  MissingRequirements,
}
pub fn canonicalize_proposal(
  input: ContractProposalInput,
  policy: &VerificationPolicy,
) -> Result<ContractProposal, ContractError> {
  let mut requirements = Vec::with_capacity(input.requirements.len());
  for requirement in input.requirements {
    let mut obligations = Vec::with_capacity(requirement.obligations.len());
    for obligation in requirement.obligations {
      let primary = configured_verifier(
        &obligation.id.0,
        &obligation.evidence_contract.claim.verifier_id,
        policy,
      )?;
      let claim = ClaimEvidenceContract {
        verifier_id: obligation.evidence_contract.claim.verifier_id,
        authority: primary.authority,
      };
      let mut assurances = Vec::with_capacity(obligation.evidence_contract.oracle_assurances.len());
      for assurance in obligation.evidence_contract.oracle_assurances {
        let verifier = configured_verifier(&obligation.id.0, &assurance.verifier_id, policy)?;
        if verifier.authority != VerifierAuthority::AuthoritySnapshot {
          return Err(ContractError::AssuranceNotIndependent {
            obligation: obligation.id.0,
            assurance: assurance.id.0,
          });
        }
        assurances.push(OracleAssuranceContract {
          id: assurance.id,
          criterion: assurance.criterion,
          verifier_id: assurance.verifier_id,
          authority: verifier.authority,
        });
      }
      obligations.push(VerificationObligation {
        id: obligation.id,
        statement: obligation.statement,
        evidence_contract: EvidenceContract { claim, assurances },
      });
    }
    requirements.push(Requirement {
      id: requirement.id,
      statement: requirement.statement,
      obligations,
    });
  }
  let proposal = ContractProposal {
    schema_version: CONTRACT_SCHEMA_VERSION,
    spec_digest: input.spec_digest,
    policy_digest: input.policy_digest,
    requirements,
  };
  validate_proposal(&proposal, policy)?;
  Ok(proposal)
}

pub fn analyze_verification(
  proposal: &ContractProposal,
  policy: &VerificationPolicy,
) -> (VerificationProfile, Vec<ProposalWarning>) {
  let mut primary_verifier_obligations = BTreeMap::<String, usize>::new();
  let mut used_verifiers = BTreeSet::new();
  let mut obligation_count = 0;
  let mut project_authority_obligations = 0;
  let mut authority_snapshot_obligations = 0;
  let mut oracle_assured_obligations = 0;
  let mut oracle_assurance_count = 0;

  for requirement in &proposal.requirements {
    for obligation in &requirement.obligations {
      obligation_count += 1;
      let claim = &obligation.evidence_contract.claim;
      *primary_verifier_obligations
        .entry(claim.verifier_id.clone())
        .or_default() += 1;
      used_verifiers.insert(claim.verifier_id.as_str());
      match claim.authority {
        VerifierAuthority::Project => project_authority_obligations += 1,
        VerifierAuthority::AuthoritySnapshot => authority_snapshot_obligations += 1,
      }
      if !obligation.evidence_contract.assurances.is_empty() {
        oracle_assured_obligations += 1;
      }
      oracle_assurance_count += obligation.evidence_contract.assurances.len();
      used_verifiers.extend(
        obligation
          .evidence_contract
          .assurances
          .iter()
          .map(|assurance| assurance.verifier_id.as_str()),
      );
    }
  }

  let unused_configured_verifiers = policy
    .verifiers
    .iter()
    .map(|verifier| verifier.id.as_str())
    .filter(|verifier| !used_verifiers.contains(verifier))
    .map(str::to_owned)
    .collect::<BTreeSet<_>>()
    .into_iter()
    .collect::<Vec<_>>();
  let profile = VerificationProfile {
    obligation_count,
    primary_verifier_count: primary_verifier_obligations.len(),
    configured_verifier_count: policy.verifiers.len(),
    used_verifier_count: used_verifiers.len(),
    project_authority_obligations,
    authority_snapshot_obligations,
    oracle_assured_obligations,
    oracle_assurance_count,
    primary_verifier_obligations,
    unused_configured_verifiers,
  };

  let mut warnings = Vec::new();
  if obligation_count > 0 && project_authority_obligations == obligation_count {
    warnings.push(ProposalWarning {
      code: ProposalWarningCode::ProjectOracleOnly,
      message: format!(
        "All {obligation_count} obligations use project-authority primary verifiers; candidate content may influence both implementation and its primary oracle."
      ),
      verifier_id: None,
      affected_obligation_count: Some(obligation_count),
    });
  }
  if let Some((verifier_id, affected)) = profile
    .primary_verifier_obligations
    .iter()
    .max_by_key(|(_, count)| **count)
    && affected.saturating_mul(5) >= obligation_count.saturating_mul(4)
  {
    warnings.push(ProposalWarning {
      code: ProposalWarningCode::OracleConcentration,
      message: format!(
        "Primary verifier `{verifier_id}` is used by {affected} of {obligation_count} obligations."
      ),
      verifier_id: Some(verifier_id.clone()),
      affected_obligation_count: Some(*affected),
    });
  }
  if oracle_assured_obligations == 0 {
    warnings.push(ProposalWarning {
      code: ProposalWarningCode::NoOracleAssurance,
      message: "No obligation has an authority-snapshot oracle assurance.".into(),
      verifier_id: None,
      affected_obligation_count: Some(obligation_count),
    });
  }
  for verifier_id in &profile.unused_configured_verifiers {
    warnings.push(ProposalWarning {
      code: ProposalWarningCode::UnusedVerifier,
      message: format!("Configured verifier `{verifier_id}` is unused by this proposal."),
      verifier_id: Some(verifier_id.clone()),
      affected_obligation_count: Some(0),
    });
  }

  (profile, warnings)
}

pub fn validate_proposal(
  proposal: &ContractProposal,
  policy: &VerificationPolicy,
) -> Result<(), ContractError> {
  if proposal.schema_version != CONTRACT_SCHEMA_VERSION {
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
  let verifier = configured_verifier(obligation, verifier_id, policy)?;
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

fn configured_verifier<'a>(
  obligation: &str,
  verifier_id: &str,
  policy: &'a VerificationPolicy,
) -> Result<&'a crate::policy::VerifierSpec, ContractError> {
  policy
    .verifiers
    .iter()
    .find(|verifier| verifier.id == verifier_id)
    .ok_or_else(|| ContractError::UnknownVerifier {
      obligation: obligation.into(),
      verifier: verifier_id.into(),
    })
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
