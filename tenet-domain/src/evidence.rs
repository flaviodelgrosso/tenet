use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
  contract::{AssuranceId, ObligationId},
  policy::{EnvironmentMode, VerifierAuthority},
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct ContentObjectId(pub String);

impl TryFrom<String> for ContentObjectId {
  type Error = String;

  fn try_from(value: String) -> Result<Self, Self::Error> {
    Self::new(value)
  }
}

impl From<ContentObjectId> for String {
  fn from(value: ContentObjectId) -> Self {
    value.0
  }
}

impl ContentObjectId {
  pub fn new(value: String) -> Result<Self, String> {
    let Some(digest) = value.strip_prefix("sha256:") else {
      return Err("content object ID must use sha256:<hex> form".into());
    };
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
      return Err(
        "content object ID must contain a 64-character hexadecimal SHA-256 digest".into(),
      );
    }
    Ok(Self(format!("sha256:{}", digest.to_ascii_lowercase())))
  }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct AuthorityId(pub ContentObjectId);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct CandidateId(pub ContentObjectId);

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
#[serde(
  tag = "authority",
  rename_all = "snake_case",
  rename_all_fields = "camelCase"
)]
pub enum OracleIdentity {
  Project {
    verifier_id: String,
    candidate_id: CandidateId,
    definition_digest: String,
  },
  AuthoritySnapshot {
    verifier_id: String,
    authority_id: AuthorityId,
    bundle_path: String,
    bundle_content_id: ContentObjectId,
    executable_content_id: ContentObjectId,
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
  CandidateSnapshot,
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
  pub authority_id: AuthorityId,
  pub candidate_id: CandidateId,
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
