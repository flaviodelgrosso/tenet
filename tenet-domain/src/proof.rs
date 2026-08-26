use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
  ids::{ArtifactId, ObligationId, VerificationRunId},
  verification::{VerificationAuthority, VerificationSpec},
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvidenceContract {
  Artifact { predicate: EvidencePredicate },
  All { requirements: Vec<EvidenceContract> },
  Any { requirements: Vec<EvidenceContract> },
  HumanAttestation { statement: String },
}
impl Default for EvidenceContract {
  fn default() -> Self {
    Self::HumanAttestation {
      statement: "Explicit human attestation required".into(),
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvidencePredicate {
  SourceInspection,
  ExecutableEvidence,
  ProjectVerification,
  NamedProjectCheck { name: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAuthority {
  Authoritative,
  Supporting,
  Advisory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "producer", rename_all = "snake_case")]
pub enum ArtifactProvenance {
  ControllerProjectVerification,
  ControllerConfiguredCheck,
  ControllerTrustedVerifier,
  ControllerSourceInspection,
  ControllerHumanAttestation { attestor: String },
  AgentProposedExecution { worker_role: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionDomain {
  Worker,
  CandidatePublicVerification,
  TrustedVerifier,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactObservation {
  Supports,
  Contradicts,
  Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ArtifactValidity {
  Valid,
  Stale {
    #[serde(rename = "invalidatedAt")]
    invalidated_at: DateTime<Utc>,
    #[serde(rename = "supersededByRevision")]
    superseded_by_revision: String,
    reason: String,
  },
}

impl ArtifactValidity {
  pub fn is_valid(&self) -> bool {
    matches!(self, Self::Valid)
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum DependencySurface {
  RepositoryWide,
  Paths {
    #[serde(rename = "blobHashes")]
    blob_hashes: BTreeMap<String, String>,
  },
  Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionObservation {
  pub command: String,
  #[serde(rename = "exitCode")]
  pub exit_code: Option<i32>,
  pub timed_out: bool,
  #[serde(rename = "durationMs")]
  pub duration_ms: u64,
  pub stdout: String,
  pub stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceArtifactKind {
  SourceSpan {
    path: String,
    #[serde(rename = "blobSha256")]
    blob_sha256: String,
    #[serde(rename = "contentSha256")]
    content_sha256: String,
    #[serde(rename = "startByte")]
    start_byte: u64,
    #[serde(rename = "endByte")]
    end_byte: u64,
  },
  CommandExecution {
    #[serde(rename = "checkName")]
    check_name: Option<String>,
    #[serde(rename = "verificationRunId")]
    run_id: VerificationRunId,
    spec: VerificationSpec,
    result: ExecutionObservation,
    domain: ExecutionDomain,
    #[serde(rename = "executionAuthority")]
    execution_authority: VerificationAuthority,
  },
  ProjectVerification {
    #[serde(rename = "verificationRunId")]
    run_id: VerificationRunId,
    #[serde(rename = "suiteHash")]
    suite_hash: String,
    passed: bool,
  },
  HumanAttestation {
    #[serde(rename = "statementHash")]
    statement_hash: String,
  },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceArtifact {
  pub id: ArtifactId,
  pub revision: String,
  #[serde(rename = "observedAt")]
  pub observed_at: DateTime<Utc>,
  pub authority: ArtifactAuthority,
  pub provenance: ArtifactProvenance,
  pub observation: ArtifactObservation,
  pub kind: EvidenceArtifactKind,
  #[serde(rename = "obligationIds")]
  pub obligation_ids: BTreeSet<ObligationId>,
  pub validity: ArtifactValidity,
  pub dependencies: DependencySurface,
  #[serde(default, rename = "compatibleRevisions")]
  pub compatible_revisions: BTreeSet<String>,
}

impl EvidenceArtifact {
  pub fn validate(&self) -> Result<(), ArtifactValidationError> {
    if self.revision.trim().is_empty() || self.obligation_ids.is_empty() {
      return Err(ArtifactValidationError::MissingBinding);
    }
    let valid = match (
      &self.provenance,
      self.authority,
      &self.kind,
      &self.dependencies,
    ) {
      (
        ArtifactProvenance::ControllerProjectVerification,
        ArtifactAuthority::Authoritative,
        EvidenceArtifactKind::ProjectVerification { .. },
        DependencySurface::RepositoryWide,
      ) => self.compatible_revisions.is_empty(),
      (
        ArtifactProvenance::ControllerConfiguredCheck,
        ArtifactAuthority::Authoritative,
        EvidenceArtifactKind::CommandExecution {
          domain: ExecutionDomain::CandidatePublicVerification,
          execution_authority: VerificationAuthority::ProjectConfigured,
          ..
        },
        DependencySurface::RepositoryWide,
      ) => self.compatible_revisions.is_empty(),
      (
        ArtifactProvenance::ControllerTrustedVerifier,
        ArtifactAuthority::Authoritative,
        EvidenceArtifactKind::CommandExecution {
          domain: ExecutionDomain::TrustedVerifier,
          execution_authority: VerificationAuthority::ControllerDerived,
          ..
        },
        DependencySurface::RepositoryWide,
      ) => self.compatible_revisions.is_empty(),
      (
        ArtifactProvenance::ControllerSourceInspection,
        ArtifactAuthority::Supporting,
        EvidenceArtifactKind::SourceSpan {
          path,
          blob_sha256,
          content_sha256,
          start_byte,
          end_byte,
        },
        DependencySurface::Paths { blob_hashes },
      ) => {
        !blob_sha256.is_empty()
          && !content_sha256.is_empty()
          && start_byte < end_byte
          && blob_hashes.len() == 1
          && blob_hashes.get(path) == Some(blob_sha256)
      }
      (
        ArtifactProvenance::ControllerHumanAttestation { attestor },
        ArtifactAuthority::Authoritative,
        EvidenceArtifactKind::HumanAttestation { statement_hash },
        DependencySurface::RepositoryWide,
      ) => {
        !attestor.trim().is_empty()
          && !statement_hash.is_empty()
          && self.compatible_revisions.is_empty()
      }
      (
        ArtifactProvenance::AgentProposedExecution { .. },
        ArtifactAuthority::Advisory,
        EvidenceArtifactKind::CommandExecution {
          execution_authority: VerificationAuthority::AgentProposed,
          domain: ExecutionDomain::Worker,
          ..
        },
        DependencySurface::Unknown,
      ) => self.compatible_revisions.is_empty(),
      _ => false,
    };
    if !valid || self.observation != expected_observation(&self.kind) {
      return Err(ArtifactValidationError::ForgedAuthority);
    }
    Ok(())
  }
  pub fn is_compatible_with(&self, revision: &str) -> bool {
    self.revision == revision || self.compatible_revisions.contains(revision)
  }

  pub fn transition_revision(
    &mut self,
    revision: &str,
    current_blob_hashes: Option<&BTreeMap<String, String>>,
  ) {
    if self.is_compatible_with(revision) || !self.validity.is_valid() {
      return;
    }
    let definitely_unaffected = match (&self.dependencies, current_blob_hashes) {
      (DependencySurface::Paths { blob_hashes }, Some(current)) if !blob_hashes.is_empty() => {
        blob_hashes
          .iter()
          .all(|(path, hash)| current.get(path) == Some(hash))
      }
      _ => false,
    };
    if definitely_unaffected {
      self.compatible_revisions.insert(revision.to_owned());
    } else {
      self.invalidate(
        revision,
        "repository impact is affected, possible, or unknown",
      );
    }
  }

  pub fn invalidate(&mut self, revision: &str, reason: impl Into<String>) {
    if self.revision != revision && self.validity.is_valid() {
      self.validity = ArtifactValidity::Stale {
        invalidated_at: Utc::now(),
        superseded_by_revision: revision.to_owned(),
        reason: reason.into(),
      };
    }
  }

  pub fn supersede(&mut self, revision: &str, reason: impl Into<String>) {
    if self.validity.is_valid() {
      self.validity = ArtifactValidity::Stale {
        invalidated_at: Utc::now(),
        superseded_by_revision: revision.to_owned(),
        reason: reason.into(),
      };
    }
  }

  pub fn matches(&self, predicate: &EvidencePredicate) -> bool {
    match (predicate, &self.kind) {
      (EvidencePredicate::SourceInspection, EvidenceArtifactKind::SourceSpan { .. }) => true,
      (EvidencePredicate::ExecutableEvidence, EvidenceArtifactKind::CommandExecution { .. }) => {
        true
      }
      (
        EvidencePredicate::NamedProjectCheck { name },
        EvidenceArtifactKind::CommandExecution {
          check_name: Some(actual),
          ..
        },
      ) => name == actual,
      _ => false,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvidenceRequestProposal {
  RunProjectCheck {
    name: String,
  },
  InspectSource {
    path: String,
    #[serde(rename = "startLine")]
    start_line: u32,
    #[serde(rename = "endLine")]
    end_line: u32,
  },
  Reproduce {
    program: String,
    #[serde(default)]
    args: Vec<String>,
  },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "judgment", rename_all = "snake_case")]
pub enum AssessmentJudgment {
  Supported {
    #[serde(rename = "artifactIds")]
    #[schemars(with = "Vec<String>")]
    artifact_ids: Vec<ArtifactId>,
    rationale: String,
  },
  Contradicted {
    #[serde(rename = "artifactIds")]
    #[schemars(with = "Vec<String>")]
    artifact_ids: Vec<ArtifactId>,
    rationale: String,
    #[serde(default)]
    proposals: Vec<EvidenceRequestProposal>,
  },
  Insufficient {
    reason: String,
    #[serde(default)]
    proposals: Vec<EvidenceRequestProposal>,
    #[serde(rename = "gapKind")]
    gap_kind: GapKind,
  },
}

impl AssessmentJudgment {
  pub fn artifact_ids(&self) -> &[ArtifactId] {
    match self {
      Self::Supported { artifact_ids, .. } | Self::Contradicted { artifact_ids, .. } => {
        artifact_ids
      }
      Self::Insufficient { .. } => &[],
    }
  }

  pub fn proposals(&self) -> &[EvidenceRequestProposal] {
    match self {
      Self::Contradicted { proposals, .. } | Self::Insufficient { proposals, .. } => proposals,
      Self::Supported { .. } => &[],
    }
  }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
  Implementation,
  Evidence,
  Specification,
  Environment,
  Verification,
  Integration,
  Dependency,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AssessmentRecord {
  #[serde(rename = "obligationId")]
  pub obligation_id: ObligationId,
  pub revision: String,
  #[serde(rename = "observedAt")]
  pub observed_at: DateTime<Utc>,
  #[serde(rename = "workerId")]
  pub worker_id: String,
  pub judgment: AssessmentJudgment,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProofState {
  Proven,
  Contradicted,
  Insufficient,
  Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ProofReason {
  Artifact {
    #[serde(rename = "artifactId")]
    artifact_id: ArtifactId,
    predicate: EvidencePredicate,
  },
  HumanAttestation {
    #[serde(rename = "artifactId")]
    artifact_id: ArtifactId,
    statement: String,
  },
  All {
    reasons: Vec<ProofReason>,
  },
  Any {
    reasons: Vec<ProofReason>,
  },
  Missing {
    requirement: String,
  },
  Stale {
    #[serde(rename = "artifactIds")]
    artifact_ids: Vec<ArtifactId>,
  },
  Contradiction {
    #[serde(rename = "artifactId")]
    artifact_id: ArtifactId,
  },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProofDerivation {
  #[serde(rename = "obligationId")]
  pub obligation_id: ObligationId,
  pub revision: String,
  pub state: ProofState,
  pub reason: ProofReason,
}

pub fn derive_proof_state<'a>(
  obligation_id: &ObligationId,
  contract: &EvidenceContract,
  artifacts: impl IntoIterator<Item = &'a EvidenceArtifact>,
  current_revision: &str,
) -> ProofDerivation {
  let artifacts: Vec<_> = artifacts
    .into_iter()
    .filter(|artifact| artifact.validate().is_ok())
    .filter(|artifact| artifact.obligation_ids.contains(obligation_id))
    .collect();
  let (state, reason) = evaluate(contract, &artifacts, current_revision);
  ProofDerivation {
    obligation_id: obligation_id.clone(),
    revision: current_revision.to_owned(),
    state,
    reason,
  }
}

fn evaluate(
  contract: &EvidenceContract,
  artifacts: &[&EvidenceArtifact],
  revision: &str,
) -> (ProofState, ProofReason) {
  match contract {
    EvidenceContract::Artifact { predicate } => evaluate_predicate(predicate, artifacts, revision),
    EvidenceContract::HumanAttestation { statement } => {
      let expected_hash = statement_hash(statement);
      let current = artifacts.iter().find(|artifact| {
        artifact.is_compatible_with(revision)
          && artifact.validity.is_valid()
          && artifact.authority == ArtifactAuthority::Authoritative
          && artifact.observation == ArtifactObservation::Supports
          && matches!(
            &artifact.kind,
            EvidenceArtifactKind::HumanAttestation { statement_hash } if statement_hash == &expected_hash
          )
      });
      current.map_or_else(
        || {
          (
            ProofState::Insufficient,
            ProofReason::Missing {
              requirement: format!("human attestation: {statement}"),
            },
          )
        },
        |artifact| {
          (
            ProofState::Proven,
            ProofReason::HumanAttestation {
              artifact_id: artifact.id,
              statement: statement.clone(),
            },
          )
        },
      )
    }
    EvidenceContract::All { requirements } => {
      let evaluated: Vec<_> = requirements
        .iter()
        .map(|item| evaluate(item, artifacts, revision))
        .collect();
      let state = combine_all(evaluated.iter().map(|(state, _)| *state));
      (
        state,
        ProofReason::All {
          reasons: evaluated.into_iter().map(|(_, reason)| reason).collect(),
        },
      )
    }
    EvidenceContract::Any { requirements } => {
      let evaluated: Vec<_> = requirements
        .iter()
        .map(|item| evaluate(item, artifacts, revision))
        .collect();
      let state = combine_any(evaluated.iter().map(|(state, _)| *state));
      (
        state,
        ProofReason::Any {
          reasons: evaluated.into_iter().map(|(_, reason)| reason).collect(),
        },
      )
    }
  }
}

fn evaluate_predicate(
  predicate: &EvidencePredicate,
  artifacts: &[&EvidenceArtifact],
  revision: &str,
) -> (ProofState, ProofReason) {
  let matching: Vec<_> = artifacts
    .iter()
    .copied()
    .filter(|artifact| artifact.matches(predicate))
    .collect();
  if let Some(artifact) = matching.iter().find(|artifact| {
    artifact.is_compatible_with(revision)
      && artifact.validity.is_valid()
      && artifact.authority == ArtifactAuthority::Authoritative
      && artifact.observation == ArtifactObservation::Contradicts
  }) {
    return (
      ProofState::Contradicted,
      ProofReason::Contradiction {
        artifact_id: artifact.id,
      },
    );
  }
  if let Some(artifact) = matching.iter().find(|artifact| {
    artifact.is_compatible_with(revision)
      && artifact.validity.is_valid()
      && artifact.authority == ArtifactAuthority::Authoritative
      && artifact.observation == ArtifactObservation::Supports
  }) {
    return (
      ProofState::Proven,
      ProofReason::Artifact {
        artifact_id: artifact.id,
        predicate: predicate.clone(),
      },
    );
  }
  let stale: Vec<_> = matching
    .iter()
    .filter(|artifact| {
      artifact.authority == ArtifactAuthority::Authoritative
        && (!artifact.validity.is_valid() || !artifact.is_compatible_with(revision))
    })
    .map(|artifact| artifact.id)
    .collect();
  if !stale.is_empty() {
    return (
      ProofState::Stale,
      ProofReason::Stale {
        artifact_ids: stale,
      },
    );
  }
  (
    ProofState::Insufficient,
    ProofReason::Missing {
      requirement: format!("{predicate:?}"),
    },
  )
}

fn combine_all(states: impl Iterator<Item = ProofState>) -> ProofState {
  let states: Vec<_> = states.collect();
  if states.contains(&ProofState::Contradicted) {
    ProofState::Contradicted
  } else if states.contains(&ProofState::Stale) {
    ProofState::Stale
  } else if !states.is_empty() && states.iter().all(|state| *state == ProofState::Proven) {
    ProofState::Proven
  } else {
    ProofState::Insufficient
  }
}

fn combine_any(states: impl Iterator<Item = ProofState>) -> ProofState {
  let states: Vec<_> = states.collect();
  if states.contains(&ProofState::Proven) {
    ProofState::Proven
  } else if !states.is_empty()
    && states
      .iter()
      .all(|state| *state == ProofState::Contradicted)
  {
    ProofState::Contradicted
  } else if states.contains(&ProofState::Stale) {
    ProofState::Stale
  } else {
    ProofState::Insufficient
  }
}

pub fn statement_hash(statement: &str) -> String {
  let digest = Sha256::digest(statement.as_bytes());
  let mut encoded = String::with_capacity(digest.len() * 2);
  for byte in digest {
    use std::fmt::Write as _;
    write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
  }
  encoded
}

fn expected_observation(kind: &EvidenceArtifactKind) -> ArtifactObservation {
  match kind {
    EvidenceArtifactKind::ProjectVerification { passed, .. } => {
      if *passed {
        ArtifactObservation::Supports
      } else {
        ArtifactObservation::Contradicts
      }
    }
    EvidenceArtifactKind::CommandExecution { result, .. } => {
      if result.exit_code == Some(0) && !result.timed_out {
        ArtifactObservation::Supports
      } else {
        ArtifactObservation::Contradicts
      }
    }
    EvidenceArtifactKind::SourceSpan { .. } | EvidenceArtifactKind::HumanAttestation { .. } => {
      ArtifactObservation::Supports
    }
  }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ArtifactValidationError {
  #[error("artifact is missing a revision or obligation binding")]
  MissingBinding,
  #[error("artifact provenance, authority, and kind form an unauthorized combination")]
  ForgedAuthority,
}

#[cfg(test)]
mod tests {
  use super::*;

  fn artifact(
    authority: ArtifactAuthority,
    revision: &str,
    observation: ArtifactObservation,
  ) -> EvidenceArtifact {
    EvidenceArtifact {
      id: ArtifactId::new(),
      revision: revision.into(),
      observed_at: Utc::now(),
      authority,
      provenance: if authority == ArtifactAuthority::Authoritative {
        ArtifactProvenance::ControllerConfiguredCheck
      } else {
        ArtifactProvenance::AgentProposedExecution {
          worker_role: "assess".into(),
        }
      },
      observation,
      kind: EvidenceArtifactKind::CommandExecution {
        check_name: Some("behavior".into()),
        spec: VerificationSpec {
          program: "test".into(),
          args: vec![],
          working_directory: ".".into(),
          environment: BTreeMap::new(),
        },
        run_id: VerificationRunId::new(),
        result: ExecutionObservation {
          command: "test".into(),
          exit_code: if observation == ArtifactObservation::Supports {
            Some(0)
          } else {
            Some(1)
          },
          timed_out: false,
          duration_ms: 1,
          stdout: String::new(),
          stderr: String::new(),
        },
        domain: if authority == ArtifactAuthority::Authoritative {
          ExecutionDomain::CandidatePublicVerification
        } else {
          ExecutionDomain::Worker
        },
        execution_authority: if authority == ArtifactAuthority::Authoritative {
          VerificationAuthority::ProjectConfigured
        } else {
          VerificationAuthority::AgentProposed
        },
      },
      obligation_ids: [ObligationId::from("VO-1")].into(),
      validity: ArtifactValidity::Valid,
      dependencies: if authority == ArtifactAuthority::Authoritative {
        DependencySurface::RepositoryWide
      } else {
        DependencySurface::Unknown
      },
      compatible_revisions: BTreeSet::new(),
    }
  }
  fn source_inspection_artifact() -> EvidenceArtifact {
    EvidenceArtifact {
      id: ArtifactId::new(),
      revision: "r1".into(),
      observed_at: Utc::now(),
      authority: ArtifactAuthority::Supporting,
      provenance: ArtifactProvenance::ControllerSourceInspection,
      observation: ArtifactObservation::Supports,
      kind: EvidenceArtifactKind::SourceSpan {
        path: "src/lib.rs".into(),
        blob_sha256: "blob-hash".into(),
        content_sha256: "span-hash".into(),
        start_byte: 0,
        end_byte: 10,
      },
      obligation_ids: [ObligationId::from("VO-1")].into(),
      validity: ArtifactValidity::Valid,
      dependencies: DependencySurface::Paths {
        blob_hashes: BTreeMap::from([("src/lib.rs".into(), "blob-hash".into())]),
      },
      compatible_revisions: BTreeSet::new(),
    }
  }

  fn human_attestation(statement: &str) -> EvidenceArtifact {
    EvidenceArtifact {
      id: ArtifactId::new(),
      revision: "r1".into(),
      observed_at: Utc::now(),
      authority: ArtifactAuthority::Authoritative,
      provenance: ArtifactProvenance::ControllerHumanAttestation {
        attestor: "registered-human".into(),
      },
      observation: ArtifactObservation::Supports,
      kind: EvidenceArtifactKind::HumanAttestation {
        statement_hash: statement_hash(statement),
      },
      obligation_ids: [ObligationId::from("VO-1")].into(),
      validity: ArtifactValidity::Valid,
      dependencies: DependencySurface::RepositoryWide,
      compatible_revisions: BTreeSet::new(),
    }
  }

  #[test]
  fn advisory_agent_success_cannot_prove() {
    let item = artifact(
      ArtifactAuthority::Advisory,
      "r1",
      ArtifactObservation::Supports,
    );
    let derivation = derive_proof_state(
      &ObligationId::from("VO-1"),
      &EvidenceContract::Artifact {
        predicate: EvidencePredicate::ExecutableEvidence,
      },
      [&item],
      "r1",
    );
    assert_eq!(derivation.state, ProofState::Insufficient);
  }
  #[test]
  fn supporting_source_inspection_cannot_prove_an_admissible_named_check_contract() {
    let item = source_inspection_artifact();
    let derivation = derive_proof_state(
      &ObligationId::from("VO-1"),
      &EvidenceContract::Artifact {
        predicate: EvidencePredicate::NamedProjectCheck {
          name: "behavior".into(),
        },
      },
      [&item],
      "r1",
    );

    assert_eq!(derivation.state, ProofState::Insufficient);
  }

  #[test]
  fn authoritative_execution_proves_without_assessment() {
    let item = artifact(
      ArtifactAuthority::Authoritative,
      "r1",
      ArtifactObservation::Supports,
    );
    let derivation = derive_proof_state(
      &ObligationId::from("VO-1"),
      &EvidenceContract::Artifact {
        predicate: EvidencePredicate::ExecutableEvidence,
      },
      [&item],
      "r1",
    );
    assert_eq!(derivation.state, ProofState::Proven);
  }

  #[test]
  fn stale_revision_fails_closed() {
    let item = artifact(
      ArtifactAuthority::Authoritative,
      "r1",
      ArtifactObservation::Supports,
    );
    let derivation = derive_proof_state(
      &ObligationId::from("VO-1"),
      &EvidenceContract::Artifact {
        predicate: EvidencePredicate::ExecutableEvidence,
      },
      [&item],
      "r2",
    );
    assert_eq!(derivation.state, ProofState::Stale);
  }

  #[test]
  fn forged_provenance_is_rejected_after_deserialization() {
    let mut item = artifact(
      ArtifactAuthority::Authoritative,
      "r1",
      ArtifactObservation::Supports,
    );
    item.provenance = ArtifactProvenance::AgentProposedExecution {
      worker_role: "assess".into(),
    };
    let encoded = serde_json::to_string(&item).unwrap();
    let decoded: EvidenceArtifact = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
      decoded.validate(),
      Err(ArtifactValidationError::ForgedAuthority)
    );
  }

  #[test]
  fn forged_observation_cannot_affect_proof() {
    let mut item = artifact(
      ArtifactAuthority::Authoritative,
      "r1",
      ArtifactObservation::Supports,
    );
    item.observation = ArtifactObservation::Contradicts;
    let derivation = derive_proof_state(
      &ObligationId::from("VO-1"),
      &EvidenceContract::Artifact {
        predicate: EvidencePredicate::ExecutableEvidence,
      },
      [&item],
      "r1",
    );
    assert_eq!(derivation.state, ProofState::Insufficient);
  }

  #[test]
  fn human_attestation_is_required_explicitly() {
    let item = artifact(
      ArtifactAuthority::Authoritative,
      "r1",
      ArtifactObservation::Supports,
    );
    let derivation = derive_proof_state(
      &ObligationId::from("VO-1"),
      &EvidenceContract::HumanAttestation {
        statement: "approved".into(),
      },
      [&item],
      "r1",
    );
    assert_eq!(derivation.state, ProofState::Insufficient);
  }

  #[test]
  fn human_attestation_is_bound_to_the_exact_statement() {
    let item = human_attestation("approved for release");
    let matching = derive_proof_state(
      &ObligationId::from("VO-1"),
      &EvidenceContract::HumanAttestation {
        statement: "approved for release".into(),
      },
      [&item],
      "r1",
    );
    let different = derive_proof_state(
      &ObligationId::from("VO-1"),
      &EvidenceContract::HumanAttestation {
        statement: "approved for production".into(),
      },
      [&item],
      "r1",
    );
    assert_eq!(matching.state, ProofState::Proven);
    assert_eq!(different.state, ProofState::Insufficient);
  }
  #[test]
  fn confirmed_authoritative_counterexample_contradicts() {
    let item = artifact(
      ArtifactAuthority::Authoritative,
      "r1",
      ArtifactObservation::Contradicts,
    );
    let derivation = derive_proof_state(
      &ObligationId::from("VO-1"),
      &EvidenceContract::Artifact {
        predicate: EvidencePredicate::ExecutableEvidence,
      },
      [&item],
      "r1",
    );
    assert_eq!(derivation.state, ProofState::Contradicted);
  }

  #[test]
  fn generic_project_verification_artifact_cannot_prove_an_obligation() {
    let mut item = artifact(
      ArtifactAuthority::Authoritative,
      "r1",
      ArtifactObservation::Supports,
    );
    item.provenance = ArtifactProvenance::ControllerProjectVerification;
    item.kind = EvidenceArtifactKind::ProjectVerification {
      run_id: VerificationRunId::new(),
      suite_hash: "suite".into(),
      passed: true,
    };
    let derivation = derive_proof_state(
      &ObligationId::from("VO-1"),
      &EvidenceContract::Artifact {
        predicate: EvidencePredicate::ProjectVerification,
      },
      [&item],
      "r1",
    );
    assert_eq!(derivation.state, ProofState::Insufficient);
  }

  #[test]
  fn all_contract_records_each_artifact_reason() {
    let item = artifact(
      ArtifactAuthority::Authoritative,
      "r1",
      ArtifactObservation::Supports,
    );
    let contract = EvidenceContract::All {
      requirements: vec![
        EvidenceContract::Artifact {
          predicate: EvidencePredicate::ExecutableEvidence,
        },
        EvidenceContract::Artifact {
          predicate: EvidencePredicate::NamedProjectCheck {
            name: "behavior".into(),
          },
        },
      ],
    };
    let first = derive_proof_state(&ObligationId::from("VO-1"), &contract, [&item], "r1");
    let second = derive_proof_state(&ObligationId::from("VO-1"), &contract, [&item], "r1");
    assert_eq!(first, second);
    assert_eq!(first.state, ProofState::Proven);
  }

  #[test]
  fn dependency_compatibility_requires_matching_current_blobs() {
    let mut item = artifact(
      ArtifactAuthority::Authoritative,
      "r1",
      ArtifactObservation::Supports,
    );
    item.dependencies = DependencySurface::Paths {
      blob_hashes: BTreeMap::from([("src/lib.rs".into(), "hash-1".into())]),
    };
    let current = BTreeMap::from([("src/lib.rs".into(), "hash-1".into())]);
    item.transition_revision("r2", Some(&current));
    assert!(item.is_compatible_with("r2"));
    assert!(item.validity.is_valid());
  }

  #[test]
  fn unknown_dependency_impact_becomes_stale() {
    let mut item = artifact(
      ArtifactAuthority::Authoritative,
      "r1",
      ArtifactObservation::Supports,
    );
    item.dependencies = DependencySurface::Unknown;
    item.transition_revision("r2", None);
    assert!(!item.validity.is_valid());
  }
  #[test]
  fn any_contract_accepts_one_authoritative_branch() {
    let item = artifact(
      ArtifactAuthority::Authoritative,
      "r1",
      ArtifactObservation::Supports,
    );
    let contract = EvidenceContract::Any {
      requirements: vec![
        EvidenceContract::HumanAttestation {
          statement: "optional human branch".into(),
        },
        EvidenceContract::Artifact {
          predicate: EvidencePredicate::NamedProjectCheck {
            name: "behavior".into(),
          },
        },
      ],
    };
    let derivation = derive_proof_state(&ObligationId::from("VO-1"), &contract, [&item], "r1");
    assert_eq!(derivation.state, ProofState::Proven);
  }

  #[test]
  fn any_proven_and_missing_is_proven_with_both_reasons() {
    let item = artifact(
      ArtifactAuthority::Authoritative,
      "r1",
      ArtifactObservation::Supports,
    );
    let contract = EvidenceContract::Any {
      requirements: vec![
        EvidenceContract::Artifact {
          predicate: EvidencePredicate::NamedProjectCheck {
            name: "behavior".into(),
          },
        },
        EvidenceContract::Artifact {
          predicate: EvidencePredicate::NamedProjectCheck {
            name: "missing".into(),
          },
        },
      ],
    };
    let derivation = derive_proof_state(&ObligationId::from("VO-1"), &contract, [&item], "r1");
    assert_eq!(derivation.state, ProofState::Proven);
    assert!(matches!(
      derivation.reason,
      ProofReason::Any { reasons }
        if matches!(reasons.as_slice(), [ProofReason::Artifact { .. }, ProofReason::Missing { .. }])
    ));
  }

  #[test]
  fn any_two_contradictions_is_contradicted_with_both_reasons() {
    let first = artifact(
      ArtifactAuthority::Authoritative,
      "r1",
      ArtifactObservation::Contradicts,
    );
    let mut second = first.clone();
    second.id = ArtifactId::new();
    if let EvidenceArtifactKind::CommandExecution { check_name, .. } = &mut second.kind {
      *check_name = Some("second".into());
    }
    let contract = EvidenceContract::Any {
      requirements: vec![
        EvidenceContract::Artifact {
          predicate: EvidencePredicate::NamedProjectCheck {
            name: "behavior".into(),
          },
        },
        EvidenceContract::Artifact {
          predicate: EvidencePredicate::NamedProjectCheck {
            name: "second".into(),
          },
        },
      ],
    };
    let derivation = derive_proof_state(
      &ObligationId::from("VO-1"),
      &contract,
      [&first, &second],
      "r1",
    );
    assert_eq!(derivation.state, ProofState::Contradicted);
    assert!(matches!(
      derivation.reason,
      ProofReason::Any { reasons }
        if reasons.iter().all(|reason| matches!(reason, ProofReason::Contradiction { .. }))
          && reasons.len() == 2
    ));
  }

  #[test]
  fn any_missing_and_stale_is_stale_with_both_reasons() {
    let item = artifact(
      ArtifactAuthority::Authoritative,
      "r1",
      ArtifactObservation::Supports,
    );
    let contract = EvidenceContract::Any {
      requirements: vec![
        EvidenceContract::Artifact {
          predicate: EvidencePredicate::NamedProjectCheck {
            name: "missing".into(),
          },
        },
        EvidenceContract::Artifact {
          predicate: EvidencePredicate::NamedProjectCheck {
            name: "behavior".into(),
          },
        },
      ],
    };
    let derivation = derive_proof_state(&ObligationId::from("VO-1"), &contract, [&item], "r2");
    assert_eq!(derivation.state, ProofState::Stale);
    assert!(matches!(
      derivation.reason,
      ProofReason::Any { reasons }
        if matches!(reasons.as_slice(), [ProofReason::Missing { .. }, ProofReason::Stale { .. }])
    ));
  }

  #[test]
  fn any_two_missing_branches_is_insufficient_with_both_reasons() {
    let contract = EvidenceContract::Any {
      requirements: vec![
        EvidenceContract::Artifact {
          predicate: EvidencePredicate::NamedProjectCheck {
            name: "first".into(),
          },
        },
        EvidenceContract::Artifact {
          predicate: EvidencePredicate::NamedProjectCheck {
            name: "second".into(),
          },
        },
      ],
    };
    let derivation = derive_proof_state(
      &ObligationId::from("VO-1"),
      &contract,
      std::iter::empty::<&EvidenceArtifact>(),
      "r1",
    );
    assert_eq!(derivation.state, ProofState::Insufficient);
    assert!(matches!(
      derivation.reason,
      ProofReason::Any { reasons }
        if reasons.iter().all(|reason| matches!(reason, ProofReason::Missing { .. }))
          && reasons.len() == 2
    ));
  }
}
