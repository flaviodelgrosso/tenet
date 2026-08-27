use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
  contract::{AssuranceId, ObligationId},
  policy::{EnvironmentMode, VerifierAuthority},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAuthority {
  AgentAssertion,
  AgentExploratoryExecution,
  TenetObservedProjectVerifier,
  TenetObservedAuthoritySnapshotVerifier,
}

impl ArtifactAuthority {
  pub fn admits(&self, expected: VerifierAuthority) -> bool {
    matches!(
      (self, expected),
      (
        Self::TenetObservedProjectVerifier,
        VerifierAuthority::Project
      ) | (
        Self::TenetObservedAuthoritySnapshotVerifier,
        VerifierAuthority::AuthoritySnapshot
      )
    )
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct GitObjectId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
  tag = "authority",
  rename_all = "snake_case",
  rename_all_fields = "camelCase"
)]
pub enum OracleIdentity {
  Project {
    verifier_id: String,
    candidate_revision: String,
    definition_digest: String,
  },
  AuthoritySnapshot {
    verifier_id: String,
    authority_revision: String,
    bundle_path: String,
    bundle_object_id: GitObjectId,
    executable_object_id: GitObjectId,
    definition_digest: String,
  },
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
#[serde(transparent)]
pub struct RunnerIdentity(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ExecutionEnvironmentIdentity(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionProvenance {
  pub runner_identity: RunnerIdentity,
  pub tenet_version: String,
  pub os: String,
  pub architecture: String,
  pub environment_mode: EnvironmentMode,
  pub execution_environment_identity: ExecutionEnvironmentIdentity,
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
pub struct VerifierEvidence {
  pub obligation_id: ObligationId,
  pub authority_revision: String,
  pub revision: String,
  pub verifier_id: String,
  pub policy_digest: String,
  pub spec_digest: String,
  pub contract_digest: String,
  pub authority: ArtifactAuthority,
  pub oracle_identity: OracleIdentity,
  pub provenance: ArtifactProvenance,
  pub execution: ExecutionProvenance,
  pub effect: EvidenceEffect,
  pub validity: ArtifactValidity,
  pub dependency_surface: DependencySurface,
  pub observation: VerifierObservation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
  tag = "evidenceKind",
  rename_all = "snake_case",
  rename_all_fields = "camelCase"
)]
pub enum EvidenceArtifact {
  Claim {
    #[serde(flatten)]
    evidence: VerifierEvidence,
  },
  OracleAssurance {
    assurance_id: AssuranceId,
    assurance_criterion: String,
    qualified_oracle_identity: OracleIdentity,
    #[serde(flatten)]
    evidence: VerifierEvidence,
  },
}

impl EvidenceArtifact {
  pub fn evidence(&self) -> &VerifierEvidence {
    match self {
      Self::Claim { evidence } | Self::OracleAssurance { evidence, .. } => evidence,
    }
  }
}
