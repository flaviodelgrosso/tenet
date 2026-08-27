use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
  contract::{AdmittedContract, ObligationId},
  evidence::{ArtifactValidity, EvidenceArtifact, EvidenceEffect},
  policy::VerificationPolicy,
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
  RepositoryNotInitialized,
  ContractMissing,
  SpecificationChanged,
  PolicyChanged,
  AuthorityRevisionNotAncestor,
  AuthoritySurfaceChanged,
  VerifierNotConfigured,
  MissingEvidence,
  ContradictionObserved,
  EvidenceStale,
  VerifierFailed,
  VerifierInfrastructureError,
  VerifierInconclusive,
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
  pub authority_revision: &'a str,
  pub revision: &'a str,
  pub spec_digest: &'a str,
  pub contract_digest: &'a str,
  pub policy_digest: &'a str,
  pub contract: &'a AdmittedContract,
  pub policy: &'a VerificationPolicy,
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
  let verifier_id = &obligation.evidence_contract.verifier_id;
  let Some(verifier) = context
    .policy
    .verifiers
    .iter()
    .find(|item| &item.id == verifier_id)
  else {
    return result(
      obligation_id,
      ObligationState::Unverifiable,
      BlockerCode::VerifierNotConfigured,
      Some(verifier_id),
      "admitted verifier is absent from policy",
    );
  };

  let matching: Vec<&EvidenceArtifact> = evidence
    .iter()
    .filter(|artifact| {
      artifact.obligation_id == *obligation_id && artifact.verifier_id == *verifier_id
    })
    .collect();
  if matching.is_empty() {
    return result(
      obligation_id,
      ObligationState::MissingEvidence,
      BlockerCode::MissingEvidence,
      Some(verifier_id),
      "no admissible observation exists",
    );
  }

  let mut saw_stale = false;
  let mut saw_support = false;
  let mut saw_contradiction = false;
  let mut saw_inconclusive = false;
  for artifact in matching {
    let binding_matches = artifact.authority_revision == context.authority_revision
      && artifact.revision == context.revision
      && artifact.spec_digest == context.spec_digest
      && artifact.contract_digest == context.contract_digest
      && artifact.policy_digest == context.policy_digest;
    if !binding_matches || artifact.validity == ArtifactValidity::Stale {
      saw_stale = true;
      continue;
    }
    if artifact.validity != ArtifactValidity::Valid
      || artifact.provenance != crate::evidence::ArtifactProvenance::TenetLocalVerifier
      || !artifact.authority.admits(&verifier.authority)
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
    return result(
      obligation_id,
      ObligationState::Contradicted,
      BlockerCode::ContradictionObserved,
      Some(verifier_id),
      "authoritative evidence contradicts the obligation",
    );
  }
  if saw_support {
    return ObligationResult {
      obligation_id: obligation_id.clone(),
      state: ObligationState::ContractSatisfied,
      blockers: Vec::new(),
    };
  }
  if saw_stale {
    return result(
      obligation_id,
      ObligationState::Stale,
      BlockerCode::EvidenceStale,
      Some(verifier_id),
      "available evidence is bound to different authority inputs",
    );
  }
  if saw_inconclusive {
    return result(
      obligation_id,
      ObligationState::Inconclusive,
      BlockerCode::VerifierInconclusive,
      Some(verifier_id),
      "authoritative observation was inconclusive",
    );
  }
  result(
    obligation_id,
    ObligationState::MissingEvidence,
    BlockerCode::MissingEvidence,
    Some(verifier_id),
    "available observations are not admitted by the evidence contract",
  )
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
