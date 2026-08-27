use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{contract::ObligationId, policy::VerifierAuthority};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAuthority {
  AgentAssertion,
  AgentExploratoryExecution,
  TenetObservedProjectVerifier,
}

impl ArtifactAuthority {
  pub fn admits(&self, expected: &VerifierAuthority) -> bool {
    matches!(
      (self, expected),
      (
        Self::TenetObservedProjectVerifier,
        VerifierAuthority::Project
      )
    )
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactProvenance {
  AgentReported,
  TenetLocalVerifier,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceEffect {
  Supports,
  Contradicts,
  Inconclusive,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactValidity {
  Valid,
  Stale,
  Invalid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DependencySurface {
  RepositoryWide,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerifierObservation {
  pub exit_code: Option<i32>,
  pub stdout: String,
  pub stderr: String,
  pub timed_out: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceArtifact {
  pub obligation_id: ObligationId,
  pub authority_revision: String,
  pub revision: String,
  pub verifier_id: String,
  pub policy_digest: String,
  pub spec_digest: String,
  pub contract_digest: String,
  pub authority: ArtifactAuthority,
  pub provenance: ArtifactProvenance,
  pub effect: EvidenceEffect,
  pub validity: ArtifactValidity,
  pub dependency_surface: DependencySurface,
  pub observation: VerifierObservation,
}
