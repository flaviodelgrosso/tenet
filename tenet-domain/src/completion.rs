use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
  contract::{AdmittedContract, ObligationId},
  evidence::{
    ArtifactProvenance, ArtifactValidity, AuthorityId, CandidateId, EvidenceArtifact,
    EvidenceEffect, OracleIdentity, VerifierEvidence,
  },
  policy::{VerificationPolicy, VerifierAuthority},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObligationState {
  ContractSatisfied,
  Contradicted,
  MissingEvidence,
  Stale,
  Inconclusive,
  Unverifiable,
  InfrastructureError,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
  Done,
  NotDone,
  Inconclusive,
  InfrastructureError,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlockerCode {
  ProjectNotInitialized,
  ContractMissing,
  SpecificationStale,
  PolicyStale,
  VerifierNotConfigured,
  MissingEvidence,
  ContradictionObserved,
  EvidenceStale,
  VerifierFailed,
  VerifierInfrastructureError,
  VerifierInconclusive,
  OracleAssuranceMissing,
  OracleAssuranceFailed,
  OracleAssuranceStale,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Blocker {
  pub code: BlockerCode,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub obligation_id: Option<ObligationId>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub verifier_id: Option<String>,
  pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObligationResult {
  pub obligation_id: ObligationId,
  pub state: ObligationState,
  pub blockers: Vec<Blocker>,
}

pub struct DerivationContext<'a> {
  pub authority_id: &'a AuthorityId,
  pub candidate_id: &'a CandidateId,
  pub spec_digest: &'a str,
  pub contract_digest: &'a str,
  pub policy_digest: &'a str,
  pub contract: &'a AdmittedContract,
  pub policy: &'a VerificationPolicy,
  pub oracle_identities: &'a BTreeMap<String, OracleIdentity>,
  pub infrastructure_errors: &'a BTreeMap<String, String>,
}

pub fn derive_obligation_state(
  context: &DerivationContext<'_>,
  obligation_id: &ObligationId,
  evidence: &[EvidenceArtifact],
) -> ObligationResult {
  let Some(obligation) = context
    .contract
    .obligations()
    .find(|item| &item.id == obligation_id)
  else {
    return result(
      obligation_id,
      ObligationState::Unverifiable,
      BlockerCode::VerifierNotConfigured,
      None,
      "obligation is not present in the admitted contract",
    );
  };
  let claim = &obligation.evidence_contract.claim;
  let Some(primary_oracle) = configured_oracle(context, &claim.verifier_id, claim.authority) else {
    if let Some(message) = context.infrastructure_errors.get(&claim.verifier_id) {
      return result(
        obligation_id,
        ObligationState::InfrastructureError,
        BlockerCode::VerifierInfrastructureError,
        Some(&claim.verifier_id),
        message,
      );
    }
    return result(
      obligation_id,
      ObligationState::Unverifiable,
      BlockerCode::VerifierNotConfigured,
      Some(&claim.verifier_id),
      "admitted primary verifier is absent, mismatched, or has no oracle identity",
    );
  };

  let primary = evaluate_claim(context, obligation_id, claim, primary_oracle, evidence);
  if primary == EvidenceEvaluation::Contradicts {
    return result(
      obligation_id,
      ObligationState::Contradicted,
      BlockerCode::ContradictionObserved,
      Some(&claim.verifier_id),
      "primary oracle evidence contradicts the obligation",
    );
  }
  for assurance in &obligation.evidence_contract.assurances {
    if let Some(message) = context.infrastructure_errors.get(&assurance.verifier_id) {
      return result(
        obligation_id,
        ObligationState::InfrastructureError,
        BlockerCode::VerifierInfrastructureError,
        Some(&assurance.verifier_id),
        message,
      );
    }
  }
  if let Some(message) = context.infrastructure_errors.get(&claim.verifier_id) {
    return result(
      obligation_id,
      ObligationState::InfrastructureError,
      BlockerCode::VerifierInfrastructureError,
      Some(&claim.verifier_id),
      message,
    );
  }
  match primary {
    EvidenceEvaluation::Supports => {}
    EvidenceEvaluation::Stale => {
      return result(
        obligation_id,
        ObligationState::Stale,
        BlockerCode::EvidenceStale,
        Some(&claim.verifier_id),
        "primary evidence is bound to different authority inputs",
      );
    }
    EvidenceEvaluation::Inconclusive => {
      return result(
        obligation_id,
        ObligationState::Inconclusive,
        BlockerCode::VerifierInconclusive,
        Some(&claim.verifier_id),
        "primary oracle observation was inconclusive",
      );
    }
    EvidenceEvaluation::Missing => {
      return result(
        obligation_id,
        ObligationState::MissingEvidence,
        BlockerCode::MissingEvidence,
        Some(&claim.verifier_id),
        "no admissible primary claim observation exists",
      );
    }
    EvidenceEvaluation::Contradicts => unreachable!(),
  }

  let mut stale_assurance = None;
  let mut failed_assurance = None;
  let mut inconclusive_assurance = None;
  for assurance in &obligation.evidence_contract.assurances {
    let Some(assurance_oracle) =
      configured_oracle(context, &assurance.verifier_id, assurance.authority)
    else {
      return result(
        obligation_id,
        ObligationState::Unverifiable,
        BlockerCode::VerifierNotConfigured,
        Some(&assurance.verifier_id),
        "admitted assurance verifier is absent, mismatched, or has no oracle identity",
      );
    };
    match evaluate_assurance(
      context,
      obligation_id,
      assurance,
      assurance_oracle,
      primary_oracle,
      evidence,
    ) {
      EvidenceEvaluation::Supports => {}
      EvidenceEvaluation::Stale => stale_assurance = Some(assurance),
      EvidenceEvaluation::Contradicts | EvidenceEvaluation::Missing => {
        failed_assurance = Some(assurance)
      }
      EvidenceEvaluation::Inconclusive => inconclusive_assurance = Some(assurance),
    }
  }
  if let Some(assurance) = stale_assurance {
    return result(
      obligation_id,
      ObligationState::Stale,
      BlockerCode::OracleAssuranceStale,
      Some(&assurance.verifier_id),
      "oracle-assurance evidence does not qualify the current primary oracle identity",
    );
  }
  if let Some(assurance) = failed_assurance {
    return result(
      obligation_id,
      ObligationState::Unverifiable,
      if evidence.iter().any(|artifact| {
        matches!(artifact, EvidenceArtifact::OracleAssurance { assurance_id, evidence, .. }
          if assurance_id == &assurance.id && evidence.verifier_id == assurance.verifier_id)
      }) {
        BlockerCode::OracleAssuranceFailed
      } else {
        BlockerCode::OracleAssuranceMissing
      },
      Some(&assurance.verifier_id),
      "required oracle-assurance support is missing or failed",
    );
  }
  if let Some(assurance) = inconclusive_assurance {
    return result(
      obligation_id,
      ObligationState::Inconclusive,
      BlockerCode::VerifierInconclusive,
      Some(&assurance.verifier_id),
      "required oracle-assurance observation was inconclusive",
    );
  }
  ObligationResult {
    obligation_id: obligation_id.clone(),
    state: ObligationState::ContractSatisfied,
    blockers: Vec::new(),
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EvidenceEvaluation {
  Supports,
  Contradicts,
  Inconclusive,
  Stale,
  Missing,
}

fn configured_oracle<'a>(
  context: &'a DerivationContext<'_>,
  verifier_id: &str,
  authority: VerifierAuthority,
) -> Option<&'a OracleIdentity> {
  context
    .policy
    .verifiers
    .iter()
    .find(|item| item.id == verifier_id && item.authority == authority)?;
  context.oracle_identities.get(verifier_id)
}

fn evaluate_claim(
  context: &DerivationContext<'_>,
  obligation_id: &ObligationId,
  claim: &crate::contract::ClaimEvidenceContract,
  oracle_identity: &OracleIdentity,
  artifacts: &[EvidenceArtifact],
) -> EvidenceEvaluation {
  evaluate(
    context,
    artifacts.iter().filter_map(|artifact| match artifact {
      EvidenceArtifact::Claim { evidence }
        if evidence.obligation_id == *obligation_id
          && evidence.verifier_id == claim.verifier_id =>
      {
        Some(evidence)
      }
      _ => None,
    }),
    claim.authority,
    oracle_identity,
  )
}

fn evaluate_assurance(
  context: &DerivationContext<'_>,
  obligation_id: &ObligationId,
  assurance: &crate::contract::OracleAssuranceContract,
  assurance_oracle: &OracleIdentity,
  qualified_oracle: &OracleIdentity,
  artifacts: &[EvidenceArtifact],
) -> EvidenceEvaluation {
  let mut qualification_stale = false;
  let evaluation = evaluate(
    context,
    artifacts.iter().filter_map(|artifact| match artifact {
      EvidenceArtifact::OracleAssurance {
        assurance_id,
        assurance_criterion,
        qualified_oracle_identity,
        evidence,
      } if evidence.obligation_id == *obligation_id
        && evidence.verifier_id == assurance.verifier_id
        && assurance_id == &assurance.id
        && assurance_criterion == &assurance.criterion =>
      {
        if qualified_oracle_identity == qualified_oracle {
          Some(evidence)
        } else {
          qualification_stale = true;
          None
        }
      }
      _ => None,
    }),
    assurance.authority,
    assurance_oracle,
  );
  if qualification_stale && evaluation == EvidenceEvaluation::Missing {
    EvidenceEvaluation::Stale
  } else {
    evaluation
  }
}

fn evaluate<'a>(
  context: &DerivationContext<'_>,
  artifacts: impl Iterator<Item = &'a VerifierEvidence>,
  authority: VerifierAuthority,
  oracle_identity: &OracleIdentity,
) -> EvidenceEvaluation {
  let mut saw_stale = false;
  let mut saw_support = false;
  let mut saw_contradiction = false;
  let mut saw_inconclusive = false;
  for artifact in artifacts {
    let binding_matches = &artifact.authority_id == context.authority_id
      && &artifact.candidate_id == context.candidate_id
      && artifact.spec_digest == context.spec_digest
      && artifact.contract_digest == context.contract_digest
      && artifact.policy_digest == context.policy_digest
      && artifact.oracle_identity == *oracle_identity;
    if !binding_matches || artifact.validity == ArtifactValidity::Stale {
      saw_stale = true;
      continue;
    }
    if artifact.validity != ArtifactValidity::Valid
      || artifact.provenance != ArtifactProvenance::TenetLocalVerifier
      || !artifact.authority.admits(authority)
    {
      continue;
    }
    match artifact.effect {
      EvidenceEffect::Supports => saw_support = true,
      EvidenceEffect::Contradicts => saw_contradiction = true,
      EvidenceEffect::Inconclusive => saw_inconclusive = true,
    }
  }
  if saw_contradiction {
    EvidenceEvaluation::Contradicts
  } else if saw_support {
    EvidenceEvaluation::Supports
  } else if saw_stale {
    EvidenceEvaluation::Stale
  } else if saw_inconclusive {
    EvidenceEvaluation::Inconclusive
  } else {
    EvidenceEvaluation::Missing
  }
}

pub fn derive_completion(obligations: &[ObligationResult]) -> Verdict {
  if obligations.is_empty() {
    Verdict::NotDone
  } else if obligations
    .iter()
    .any(|item| item.state == ObligationState::InfrastructureError)
  {
    Verdict::InfrastructureError
  } else if obligations.iter().any(|item| {
    matches!(
      item.state,
      ObligationState::Contradicted
        | ObligationState::MissingEvidence
        | ObligationState::Stale
        | ObligationState::Unverifiable
    )
  }) {
    Verdict::NotDone
  } else if obligations
    .iter()
    .any(|item| item.state == ObligationState::Inconclusive)
  {
    Verdict::Inconclusive
  } else {
    Verdict::Done
  }
}

fn result(
  obligation_id: &ObligationId,
  state: ObligationState,
  code: BlockerCode,
  verifier_id: Option<&String>,
  message: &str,
) -> ObligationResult {
  ObligationResult {
    obligation_id: obligation_id.clone(),
    state,
    blockers: vec![Blocker {
      code,
      obligation_id: Some(obligation_id.clone()),
      verifier_id: verifier_id.cloned(),
      message: message.into(),
    }],
  }
}
