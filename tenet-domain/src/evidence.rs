use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
  ids::{ArtifactId, CriterionId, EvidenceId, ObligationId, RequirementId, VerificationRunId},
  proof::{
    derive_proof_state, ArtifactAuthority, ArtifactObservation, ArtifactProvenance,
    ArtifactValidity, AssessmentJudgment, AssessmentRecord, DependencySurface, EvidenceArtifact,
    EvidenceArtifactKind, EvidenceContract, EvidencePredicate, ExecutionDomain,
    ExecutionObservation, ProofDerivation, ProofState,
  },
  trusted_verifier::{TrustedExecutionRecord, TrustedVerificationSpec},
  verification::{ProjectCheckResult, ProjectVerificationRun, VerificationAuthority},
};

/// Implementation completeness observed in the repository, independent from verification and evidence state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImplementationState {
  /// All required implementation for the requirement exists.
  Present,
  /// Some required implementation exists, but required behavior is missing or incomplete.
  Partial,
  /// The required implementation does not exist.
  Absent,
  /// Repository inspection cannot determine whether the required implementation exists.
  Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
  Verified,
  PartiallyVerified,
  Unverified,
  Uncertain,
  Stale,
  Contradicted,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceCriterion {
  pub id: CriterionId,
  #[serde(rename = "requirementId")]
  pub requirement_id: RequirementId,
  pub description: String,
  #[serde(default = "default_true")]
  pub mandatory: bool,
}

/// A semantic claim that must be established for an acceptance criterion.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerificationObligation {
  pub id: ObligationId,
  #[serde(rename = "criterionId")]
  pub criterion_id: CriterionId,
  pub description: String,
  #[serde(default = "default_true")]
  pub required: bool,
  #[serde(rename = "evidenceContract")]
  pub evidence_contract: EvidenceContract,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
  ProjectVerification,
  SemanticAssessment,
  AgentSuggestion,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceResult {
  Passed,
  Failed,
  Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "authority", rename_all = "snake_case")]
pub enum EvidenceProvenance {
  IndependentAssessment {
    #[serde(rename = "workerId")]
    worker_id: String,
  },
  AgentProposal {
    #[serde(rename = "workerRole")]
    worker_role: String,
  },
}

impl EvidenceProvenance {
  pub fn independent_assessment(worker_id: impl Into<String>) -> Self {
    Self::IndependentAssessment {
      worker_id: worker_id.into(),
    }
  }

  pub fn agent_proposal(worker_role: impl Into<String>) -> Self {
    Self::AgentProposal {
      worker_role: worker_role.into(),
    }
  }

  pub fn is_independent_assessment(&self) -> bool {
    matches!(self, Self::IndependentAssessment { .. })
  }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectEvidenceProvenance {
  ControllerExecution,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EvidenceValidity {
  Valid,
  Stale {
    #[serde(rename = "invalidatedAt")]
    invalidated_at: DateTime<Utc>,
    #[serde(rename = "supersededByRevision")]
    superseded_by_revision: String,
  },
}

impl EvidenceValidity {
  pub fn is_valid(&self) -> bool {
    matches!(self, Self::Valid)
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectVerificationEvidence {
  #[serde(rename = "verificationRunId")]
  pub run_id: VerificationRunId,
  pub revision: String,
  #[serde(rename = "suiteHash")]
  pub suite_hash: String,
  pub result: EvidenceResult,
  #[serde(rename = "checkResults")]
  pub check_results: Vec<ProjectCheckResult>,
  #[serde(rename = "observedAt")]
  pub observed_at: DateTime<Utc>,
  pub source: EvidenceSource,
  pub provenance: ProjectEvidenceProvenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
  pub id: EvidenceId,
  #[serde(rename = "requirementId")]
  pub requirement_id: RequirementId,
  #[serde(rename = "criterionId")]
  pub criterion_id: CriterionId,
  #[serde(rename = "obligationId")]
  pub obligation_id: ObligationId,
  pub source: EvidenceSource,
  pub result: EvidenceResult,
  pub revision: String,
  #[serde(rename = "observedAt")]
  pub observed_at: DateTime<Utc>,
  pub provenance: EvidenceProvenance,
  pub rationale: String,
  #[serde(rename = "evidenceRefs")]
  pub evidence_refs: Vec<String>,
  pub validity: EvidenceValidity,
}

/// Agent-facing advisory judgment keyed by a controller-generated obligation handle.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentObligationAssessment {
  #[serde(rename = "obligationHandle")]
  pub obligation_handle: String,
  pub judgment: AssessmentJudgment,
}

/// Agent-facing semantic judgments keyed by controller-selected obligation handles.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticAssessmentProposal {
  pub summary: String,
  pub assessments: Vec<AgentObligationAssessment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ObligationAssessmentResult {
  #[serde(rename = "obligationId")]
  pub obligation_id: ObligationId,
  pub assessment: AssessmentJudgment,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticAssessmentReport {
  pub summary: String,
  pub assessments: Vec<ObligationAssessmentResult>,
}

#[derive(Debug, Clone, Copy)]
pub struct EvidencePolicy<'a> {
  pub revision: &'a str,
  pub suite_hash: &'a str,
}

impl<'a> EvidencePolicy<'a> {
  pub fn new(revision: &'a str, suite_hash: &'a str) -> Self {
    Self {
      revision,
      suite_hash,
    }
  }

  fn project_passes(self, graph: &EvidenceGraphState) -> bool {
    let mut saw_pass = false;
    for evidence in graph.project_evidence.values().filter(|evidence| {
      evidence.revision == self.revision
        && evidence.suite_hash == self.suite_hash
        && evidence.source == EvidenceSource::ProjectVerification
        && evidence.provenance == ProjectEvidenceProvenance::ControllerExecution
    }) {
      if evidence.result != EvidenceResult::Passed {
        return false;
      }
      saw_pass = true;
    }
    saw_pass
  }

  pub fn authorizes(self, graph: &EvidenceGraphState, evidence: &Evidence) -> bool {
    self.project_passes(graph)
      && evidence.revision == self.revision
      && evidence.validity.is_valid()
      && evidence.source == EvidenceSource::SemanticAssessment
      && evidence.result == EvidenceResult::Passed
      && evidence.provenance.is_independent_assessment()
  }

  pub fn blocks(self, evidence: &Evidence) -> bool {
    evidence.revision == self.revision
      && evidence.validity.is_valid()
      && evidence.source == EvidenceSource::SemanticAssessment
      && evidence.result == EvidenceResult::Failed
      && evidence.provenance.is_independent_assessment()
  }

  pub fn is_uncertain(self, evidence: &Evidence) -> bool {
    evidence.revision == self.revision
      && evidence.validity.is_valid()
      && evidence.source == EvidenceSource::SemanticAssessment
      && evidence.result == EvidenceResult::Inconclusive
      && evidence.provenance.is_independent_assessment()
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceGraphState {
  pub version: u32,
  #[serde(rename = "specificationHash")]
  pub specification_hash: String,
  #[serde(default)]
  pub requirements: BTreeSet<RequirementId>,
  #[serde(rename = "requiredRequirements")]
  pub required_requirements: BTreeSet<RequirementId>,
  pub criteria: BTreeMap<CriterionId, AcceptanceCriterion>,
  pub obligations: BTreeMap<ObligationId, VerificationObligation>,
  #[serde(default, rename = "projectEvidence")]
  pub project_evidence: BTreeMap<VerificationRunId, ProjectVerificationEvidence>,
  #[serde(default)]
  pub evidence: BTreeMap<EvidenceId, Evidence>,
  #[serde(default)]
  pub artifacts: BTreeMap<ArtifactId, EvidenceArtifact>,
  #[serde(default)]
  pub assessments: Vec<AssessmentRecord>,
  #[serde(default, rename = "proofDerivations")]
  pub proof_derivations: BTreeMap<ObligationId, ProofDerivation>,
}

impl EvidenceGraphState {
  pub const VERSION: u32 = 4;

  pub fn new(specification_hash: impl Into<String>) -> Self {
    Self {
      version: Self::VERSION,
      specification_hash: specification_hash.into(),
      requirements: BTreeSet::new(),
      required_requirements: BTreeSet::new(),
      criteria: BTreeMap::new(),
      obligations: BTreeMap::new(),
      project_evidence: BTreeMap::new(),
      evidence: BTreeMap::new(),
      artifacts: BTreeMap::new(),
      assessments: Vec::new(),
      proof_derivations: BTreeMap::new(),
    }
  }

  pub fn register_requirement(&mut self, requirement_id: RequirementId, required: bool) {
    self.requirements.insert(requirement_id.clone());
    if required {
      self.required_requirements.insert(requirement_id);
    } else {
      self.required_requirements.remove(&requirement_id);
    }
  }

  pub fn add_criterion(
    &mut self,
    criterion: AcceptanceCriterion,
  ) -> Result<(), EvidenceGraphError> {
    if !self.requirements.contains(&criterion.requirement_id) {
      return Err(EvidenceGraphError::UnknownRequirement(
        criterion.requirement_id.clone(),
      ));
    }
    if criterion.description.trim().is_empty() {
      return Err(EvidenceGraphError::BlankDescription(
        criterion.id.to_string(),
      ));
    }
    if self.criteria.contains_key(&criterion.id) {
      return Err(EvidenceGraphError::DuplicateCriterion);
    }
    self.criteria.insert(criterion.id.clone(), criterion);
    Ok(())
  }

  pub fn add_obligation(
    &mut self,
    obligation: VerificationObligation,
  ) -> Result<(), EvidenceGraphError> {
    if !self.criteria.contains_key(&obligation.criterion_id) {
      return Err(EvidenceGraphError::UnknownCriterion(
        obligation.criterion_id.clone(),
      ));
    }
    if obligation.description.trim().is_empty() {
      return Err(EvidenceGraphError::BlankDescription(
        obligation.id.to_string(),
      ));
    }
    if self.obligations.contains_key(&obligation.id) {
      return Err(EvidenceGraphError::DuplicateObligation);
    }
    self.obligations.insert(obligation.id.clone(), obligation);
    Ok(())
  }

  pub fn establish_evidence(&mut self, evidence: Evidence) -> Result<(), EvidenceGraphError> {
    let obligation = self
      .obligations
      .get(&evidence.obligation_id)
      .ok_or_else(|| EvidenceGraphError::UnknownObligation(evidence.obligation_id.clone()))?;
    let criterion = self
      .criteria
      .get(&evidence.criterion_id)
      .ok_or_else(|| EvidenceGraphError::UnknownCriterion(evidence.criterion_id.clone()))?;
    if obligation.criterion_id != evidence.criterion_id
      || criterion.requirement_id != evidence.requirement_id
    {
      return Err(EvidenceGraphError::RelationshipMismatch(evidence.id));
    }
    if self.evidence.contains_key(&evidence.id) {
      return Err(EvidenceGraphError::DuplicateEvidence);
    }
    self.evidence.insert(evidence.id, evidence);
    Ok(())
  }
  pub fn establish_artifact(
    &mut self,
    artifact: EvidenceArtifact,
  ) -> Result<(), EvidenceGraphError> {
    artifact
      .validate()
      .map_err(|error| EvidenceGraphError::InvalidArtifact(error.to_string()))?;
    for obligation_id in &artifact.obligation_ids {
      if !self.obligations.contains_key(obligation_id) {
        return Err(EvidenceGraphError::UnknownObligation(obligation_id.clone()));
      }
    }
    if self.artifacts.contains_key(&artifact.id) {
      return Err(EvidenceGraphError::DuplicateArtifact);
    }
    self.artifacts.insert(artifact.id, artifact);
    Ok(())
  }

  pub fn record_project_artifacts(
    &mut self,
    run: &ProjectVerificationRun,
  ) -> Result<Vec<ArtifactId>, EvidenceGraphError> {
    let mut ids = Vec::new();
    for check in &run.checks {
      let predicate = EvidencePredicate::NamedProjectCheck {
        name: check.name.clone(),
      };
      let bindings: BTreeSet<_> = self
        .obligations
        .values()
        .filter(|obligation| contract_contains(&obligation.evidence_contract, &predicate))
        .map(|obligation| obligation.id.clone())
        .collect();
      if bindings.is_empty() {
        continue;
      }
      for existing in self.artifacts.values_mut().filter(|artifact| {
        artifact.revision == run.revision
          && matches!(
            &artifact.kind,
            EvidenceArtifactKind::CommandExecution {
              check_name: Some(existing_name),
              ..
            } if existing_name == &check.name
          )
          && matches!(
            artifact.provenance,
            ArtifactProvenance::ControllerConfiguredCheck
          )
      }) {
        existing.supersede(
          &run.revision,
          "replaced by a newer configured-check observation",
        );
      }
      let passed = check.result.exit_code == Some(0) && !check.result.timed_out;
      let artifact = EvidenceArtifact {
        id: ArtifactId::new(),
        revision: run.revision.clone(),
        observed_at: run.finished_at,
        authority: ArtifactAuthority::Authoritative,
        provenance: ArtifactProvenance::ControllerConfiguredCheck,
        observation: if passed {
          ArtifactObservation::Supports
        } else {
          ArtifactObservation::Contradicts
        },
        kind: EvidenceArtifactKind::CommandExecution {
          check_name: Some(check.name.clone()),
          run_id: run.run_id,
          spec: check.spec.clone(),
          result: ExecutionObservation {
            command: check.result.command.clone(),
            exit_code: check.result.exit_code,
            timed_out: check.result.timed_out,
            duration_ms: u64::try_from(check.result.duration_ms).unwrap_or(u64::MAX),
            stdout: check.result.stdout.clone(),
            stderr: check.result.stderr.clone(),
          },
          domain: ExecutionDomain::CandidatePublicVerification,
          execution_authority: VerificationAuthority::ProjectConfigured,
        },
        obligation_ids: bindings,
        validity: ArtifactValidity::Valid,
        dependencies: DependencySurface::RepositoryWide,
        compatible_revisions: BTreeSet::new(),
      };
      ids.push(artifact.id);
      self.establish_artifact(artifact)?;
    }
    Ok(ids)
  }
  pub fn record_trusted_execution(
    &mut self,
    record: &TrustedExecutionRecord,
    spec: &TrustedVerificationSpec,
  ) -> Result<Option<ArtifactId>, EvidenceGraphError> {
    if !record.can_issue_authority(spec) {
      return Ok(None);
    }
    let predicate = EvidencePredicate::TrustedVerifierCheck {
      name: spec.name.clone(),
    };
    let bindings: BTreeSet<_> = self
      .obligations
      .values()
      .filter(|obligation| contract_contains(&obligation.evidence_contract, &predicate))
      .map(|obligation| obligation.id.clone())
      .collect();
    let observed_bindings: BTreeSet<_> = record.obligation_ids.iter().cloned().collect();
    if bindings.is_empty() || bindings != observed_bindings {
      return Err(EvidenceGraphError::TrustedExecutionBindingMismatch);
    }
    let observation = record
      .result
      .authoritative_observation()
      .ok_or(EvidenceGraphError::TrustedExecutionNotAuthoritative)?;
    let isolation_report = record
      .isolation_report
      .clone()
      .ok_or(EvidenceGraphError::TrustedExecutionNotAuthoritative)?;
    let artifact = EvidenceArtifact {
      id: ArtifactId::new(),
      revision: record.revision.clone(),
      observed_at: record.finished_at,
      authority: ArtifactAuthority::Authoritative,
      provenance: ArtifactProvenance::ControllerTrustedVerifier,
      observation,
      kind: EvidenceArtifactKind::TrustedExecution {
        run_id: record.id,
        verifier_name: record.verifier_name.clone(),
        spec_hash: record.spec_hash.clone(),
        isolation_policy_hash: record.isolation_policy_hash.clone(),
        execution_record_hash: record
          .record_hash()
          .map_err(|error| EvidenceGraphError::InvalidArtifact(error.to_string()))?,
        isolation_report: Box::new(isolation_report),
        result: record.observation.clone(),
      },
      obligation_ids: bindings,
      validity: ArtifactValidity::Valid,
      dependencies: DependencySurface::RepositoryWide,
      compatible_revisions: BTreeSet::new(),
    };
    let id = artifact.id;
    self.establish_artifact(artifact)?;
    Ok(Some(id))
  }

  pub fn record_assessment_judgments(
    &mut self,
    revision: &str,
    observed_at: DateTime<Utc>,
    worker_id: &str,
    judgments: Vec<(ObligationId, AssessmentJudgment)>,
  ) -> Result<(), EvidenceGraphError> {
    let expected: BTreeSet<_> = self
      .obligations
      .values()
      .filter(|item| item.required)
      .map(|item| item.id.clone())
      .collect();
    let actual: BTreeSet<_> = judgments.iter().map(|(id, _)| id.clone()).collect();
    if expected != actual || actual.len() != judgments.len() {
      return Err(EvidenceGraphError::SemanticAssessmentCoverageMismatch);
    }
    for (obligation_id, judgment) in judgments {
      if matches!(judgment, AssessmentJudgment::Supported { ref artifact_ids, .. } if artifact_ids.is_empty())
      {
        return Err(EvidenceGraphError::MissingArtifactReference(obligation_id));
      }
      for artifact_id in judgment.artifact_ids() {
        let artifact = self
          .artifacts
          .get(artifact_id)
          .ok_or(EvidenceGraphError::UnknownArtifact(*artifact_id))?;
        if !artifact.obligation_ids.contains(&obligation_id) {
          return Err(EvidenceGraphError::ArtifactBindingMismatch(
            *artifact_id,
            obligation_id,
          ));
        }
      }
      self.assessments.push(AssessmentRecord {
        obligation_id,
        revision: revision.to_owned(),
        observed_at,
        worker_id: worker_id.to_owned(),
        judgment,
      });
    }
    Ok(())
  }

  pub fn derive_proofs(&mut self, revision: &str) {
    self.proof_derivations = self
      .obligations
      .values()
      .filter(|obligation| obligation.required)
      .map(|obligation| {
        let derivation = derive_proof_state(
          &obligation.id,
          &obligation.evidence_contract,
          self.artifacts.values(),
          revision,
        );
        (obligation.id.clone(), derivation)
      })
      .collect();
  }

  pub fn record_project_verification(&mut self, run: &ProjectVerificationRun) {
    self.project_evidence.insert(
      run.run_id,
      ProjectVerificationEvidence {
        run_id: run.run_id,
        revision: run.revision.clone(),
        suite_hash: run.suite_hash.clone(),
        result: if run.passed {
          EvidenceResult::Passed
        } else {
          EvidenceResult::Failed
        },
        check_results: run.checks.clone(),
        observed_at: run.finished_at,
        source: EvidenceSource::ProjectVerification,
        provenance: ProjectEvidenceProvenance::ControllerExecution,
      },
    );
  }

  pub fn record_semantic_assessment(
    &mut self,
    revision: &str,
    observed_at: DateTime<Utc>,
    worker_id: &str,
    report: &SemanticAssessmentReport,
  ) -> Result<(), EvidenceGraphError> {
    if report.summary.trim().is_empty() {
      return Err(EvidenceGraphError::BlankSemanticSummary);
    }
    self.record_assessment_judgments(
      revision,
      observed_at,
      worker_id,
      report
        .assessments
        .iter()
        .map(|item| (item.obligation_id.clone(), item.assessment.clone()))
        .collect(),
    )
  }

  pub fn invalidate_where(
    &mut self,
    revision: &str,
    invalidated_at: DateTime<Utc>,
    mut affected: impl FnMut(&Evidence) -> bool,
  ) -> Vec<EvidenceId> {
    let mut invalidated = Vec::new();
    for evidence in self.evidence.values_mut() {
      if evidence.validity.is_valid() && evidence.revision != revision && affected(evidence) {
        evidence.validity = EvidenceValidity::Stale {
          invalidated_at,
          superseded_by_revision: revision.to_owned(),
        };
        invalidated.push(evidence.id);
      }
    }
    invalidated
  }
  pub fn transition_artifacts(
    &mut self,
    revision: &str,
    current_blob_hashes: Option<&BTreeMap<String, String>>,
  ) -> Vec<ArtifactId> {
    let mut invalidated = Vec::new();
    for artifact in self.artifacts.values_mut() {
      let was_valid = artifact.validity.is_valid();
      artifact.transition_revision(revision, current_blob_hashes);
      if was_valid && !artifact.validity.is_valid() {
        invalidated.push(artifact.id);
      }
    }
    self.derive_proofs(revision);
    invalidated
  }

  pub fn criterion_verification_state(
    &self,
    criterion_id: &CriterionId,
    policy: EvidencePolicy<'_>,
  ) -> Result<VerificationState, EvidenceGraphError> {
    let criterion = self
      .criteria
      .get(criterion_id)
      .ok_or_else(|| EvidenceGraphError::UnknownCriterion(criterion_id.clone()))?;
    let states: Vec<_> = self
      .obligations
      .values()
      .filter(|obligation| obligation.criterion_id == criterion.id && obligation.required)
      .map(|obligation| self.obligation_state(&obligation.id, policy))
      .collect();
    Ok(combine_states(&states))
  }

  pub fn requirement_verification_state(
    &self,
    requirement_id: &RequirementId,
    policy: EvidencePolicy<'_>,
  ) -> Result<VerificationState, EvidenceGraphError> {
    if !self.requirements.contains(requirement_id) {
      return Err(EvidenceGraphError::UnknownRequirement(
        requirement_id.clone(),
      ));
    }
    let states: Result<Vec<_>, _> = self
      .criteria
      .values()
      .filter(|criterion| criterion.requirement_id == *requirement_id && criterion.mandatory)
      .map(|criterion| self.criterion_verification_state(&criterion.id, policy))
      .collect();
    Ok(combine_states(&states?))
  }

  pub fn all_required_verified(&self, policy: EvidencePolicy<'_>) -> bool {
    self.required_requirements.iter().all(|requirement_id| {
      self.requirement_verification_state(requirement_id, policy) == Ok(VerificationState::Verified)
    })
  }

  pub fn projection(
    &self,
    requirement_id: &RequirementId,
    policy: EvidencePolicy<'_>,
  ) -> Result<EvidenceProjection, EvidenceGraphError> {
    let verification_state = self.requirement_verification_state(requirement_id, policy)?;
    let criteria = self
      .criteria
      .values()
      .filter(|criterion| criterion.requirement_id == *requirement_id)
      .map(|criterion| CriterionProjection {
        criterion: criterion.clone(),
        state: self
          .criterion_verification_state(&criterion.id, policy)
          .unwrap_or(VerificationState::Unverified),
        obligations: self
          .obligations
          .values()
          .filter(|obligation| obligation.criterion_id == criterion.id)
          .map(|obligation| ObligationProjection {
            obligation: obligation.clone(),
            state: self.obligation_state(&obligation.id, policy),
            evidence: self
              .evidence
              .values()
              .filter(|evidence| evidence.obligation_id == obligation.id)
              .cloned()
              .collect(),
            artifacts: self
              .artifacts
              .values()
              .filter(|artifact| artifact.obligation_ids.contains(&obligation.id))
              .cloned()
              .collect(),
            proof: self.proof_derivations.get(&obligation.id).cloned(),
          })
          .collect(),
      })
      .collect();
    Ok(EvidenceProjection {
      requirement_id: requirement_id.clone(),
      verification_state,
      criteria,
    })
  }

  pub fn semantic_counts(&self, policy: EvidencePolicy<'_>) -> SemanticCounts {
    let mut counts = SemanticCounts {
      total: self
        .obligations
        .values()
        .filter(|obligation| obligation.required)
        .count(),
      ..SemanticCounts::default()
    };
    for obligation in self
      .obligations
      .values()
      .filter(|obligation| obligation.required)
    {
      match self.obligation_state(&obligation.id, policy) {
        VerificationState::Verified => counts.satisfied += 1,
        VerificationState::Contradicted => counts.gaps += 1,
        VerificationState::Uncertain => counts.uncertain += 1,
        VerificationState::Stale => counts.stale += 1,
        VerificationState::PartiallyVerified | VerificationState::Unverified => {}
      }
    }
    counts
  }

  fn obligation_state(
    &self,
    obligation_id: &ObligationId,
    policy: EvidencePolicy<'_>,
  ) -> VerificationState {
    match self.proof_derivations.get(obligation_id) {
      Some(derivation) if derivation.revision == policy.revision => match derivation.state {
        ProofState::Proven => VerificationState::Verified,
        ProofState::Contradicted => VerificationState::Contradicted,
        ProofState::Insufficient => VerificationState::Unverified,
        ProofState::Stale => VerificationState::Stale,
      },
      Some(_) => VerificationState::Stale,
      None => VerificationState::Unverified,
    }
  }
}

fn contract_contains(contract: &EvidenceContract, predicate: &EvidencePredicate) -> bool {
  match contract {
    EvidenceContract::Artifact { predicate: actual } => actual == predicate,
    EvidenceContract::All { requirements } | EvidenceContract::Any { requirements } => requirements
      .iter()
      .any(|item| contract_contains(item, predicate)),
    EvidenceContract::HumanAttestation { .. } => false,
  }
}

fn combine_states(states: &[VerificationState]) -> VerificationState {
  if states.is_empty() {
    return VerificationState::Unverified;
  }
  if states.contains(&VerificationState::Contradicted) {
    return VerificationState::Contradicted;
  }
  if states.contains(&VerificationState::Uncertain) {
    return VerificationState::Uncertain;
  }
  if states
    .iter()
    .all(|state| *state == VerificationState::Verified)
  {
    return VerificationState::Verified;
  }
  if states.contains(&VerificationState::Stale) {
    return VerificationState::Stale;
  }
  if states.contains(&VerificationState::Verified) {
    return VerificationState::PartiallyVerified;
  }
  VerificationState::Unverified
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticCounts {
  pub total: usize,
  pub satisfied: usize,
  pub gaps: usize,
  pub uncertain: usize,
  pub stale: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceProjection {
  #[serde(rename = "requirementId")]
  pub requirement_id: RequirementId,
  #[serde(rename = "verificationState")]
  pub verification_state: VerificationState,
  pub criteria: Vec<CriterionProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CriterionProjection {
  pub criterion: AcceptanceCriterion,
  pub state: VerificationState,
  pub obligations: Vec<ObligationProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObligationProjection {
  pub obligation: VerificationObligation,
  pub state: VerificationState,
  pub evidence: Vec<Evidence>,
  pub artifacts: Vec<EvidenceArtifact>,
  pub proof: Option<ProofDerivation>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EvidenceGraphError {
  #[error("unknown requirement {0}")]
  UnknownRequirement(RequirementId),
  #[error("unknown acceptance criterion {0}")]
  UnknownCriterion(CriterionId),
  #[error("unknown verification obligation {0}")]
  UnknownObligation(ObligationId),
  #[error("duplicate acceptance criterion")]
  DuplicateCriterion,
  #[error("duplicate verification obligation")]
  DuplicateObligation,
  #[error("duplicate evidence id")]
  DuplicateEvidence,
  #[error("duplicate evidence artifact id")]
  DuplicateArtifact,
  #[error("invalid evidence artifact: {0}")]
  InvalidArtifact(String),
  #[error("trusted execution does not bind exactly the configured verifier obligations")]
  TrustedExecutionBindingMismatch,
  #[error("trusted execution cannot issue authoritative evidence")]
  TrustedExecutionNotAuthoritative,
  #[error("unknown evidence artifact {0}")]
  UnknownArtifact(ArtifactId),
  #[error("assessment for {0} must reference at least one existing artifact")]
  MissingArtifactReference(ObligationId),
  #[error("artifact {0} is not bound to assessment obligation {1}")]
  ArtifactBindingMismatch(ArtifactId, ObligationId),
  #[error("evidence {0} does not match its criterion and obligation relationships")]
  RelationshipMismatch(EvidenceId),
  #[error("{0} has a blank description")]
  BlankDescription(String),
  #[error("semantic assessment summary must not be blank")]
  BlankSemanticSummary,
  #[error("semantic assessment for {0} must provide a rationale")]
  BlankAssessment(ObligationId),
  #[error("semantic assessment must cover every required obligation exactly once")]
  SemanticAssessmentCoverageMismatch,
}

fn default_true() -> bool {
  true
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::verification::{CommandResult, ProjectCheckResult};
  use chrono::TimeZone;

  fn graph() -> EvidenceGraphState {
    let mut graph = EvidenceGraphState::new("spec-hash");
    graph.register_requirement(RequirementId::from("REQ-007"), true);
    graph
      .add_criterion(AcceptanceCriterion {
        id: CriterionId::from("REQ-007/AC-01"),
        requirement_id: RequirementId::from("REQ-007"),
        description: "Expired tokens return HTTP 401".into(),
        mandatory: true,
      })
      .expect("criterion");
    graph
      .add_obligation(VerificationObligation {
        id: ObligationId::from("REQ-007/AC-01/VO-01"),
        criterion_id: CriterionId::from("REQ-007/AC-01"),
        description: "An expired token is rejected with HTTP 401".into(),
        required: true,
        evidence_contract: Default::default(),
      })
      .expect("obligation");
    graph
  }

  fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 18, 10, 0, 0).unwrap()
  }

  fn project_run(revision: &str, suite_hash: &str, passed: bool) -> ProjectVerificationRun {
    ProjectVerificationRun {
      run_id: VerificationRunId::new(),
      revision: revision.into(),
      suite_hash: suite_hash.into(),
      checks: vec![ProjectCheckResult {
        name: "quality".into(),
        spec: crate::verification::VerificationSpec {
          program: "true".into(),
          args: Vec::new(),
          working_directory: ".".into(),
          environment: BTreeMap::new(),
        },
        timeout_secs: 10,
        result: CommandResult {
          command: "true".into(),
          exit_code: Some(if passed { 0 } else { 1 }),
          timed_out: false,
          duration_ms: 1,
          stdout: String::new(),
          stderr: String::new(),
        },
      }],
      passed,
      started_at: now(),
      finished_at: now(),
    }
  }

  fn state(graph: &EvidenceGraphState, revision: &str, suite: &str) -> VerificationState {
    graph
      .requirement_verification_state(
        &RequirementId::from("REQ-007"),
        EvidencePolicy::new(revision, suite),
      )
      .expect("state")
  }

  #[test]
  fn project_pass_alone_does_not_verify_obligation() {
    let mut graph = graph();
    graph.record_project_verification(&project_run("abc", "suite", true));

    assert_eq!(state(&graph, "abc", "suite"), VerificationState::Unverified);
  }

  #[test]
  fn internal_adjudication_model_suspicion_cannot_create_authoritative_contradiction() {
    let mut graph = graph();
    graph
      .record_assessment_judgments(
        "abc",
        now(),
        "assess",
        vec![(
          ObligationId::from("REQ-007/AC-01/VO-01"),
          AssessmentJudgment::Contradicted {
            artifact_ids: Vec::new(),
            rationale: "model suspects a counterexample".into(),
            proposals: Vec::new(),
          },
        )],
      )
      .expect("record advisory suspicion");
    graph.derive_proofs("abc");

    assert_eq!(
      graph.proof_derivations[&ObligationId::from("REQ-007/AC-01/VO-01")].state,
      ProofState::Insufficient
    );
  }

  #[test]
  fn agent_suggestion_cannot_authorize_obligation() {
    let mut graph = graph();
    graph.record_project_verification(&project_run("abc", "suite", true));
    graph
      .establish_evidence(Evidence {
        id: EvidenceId::new(),
        requirement_id: RequirementId::from("REQ-007"),
        criterion_id: CriterionId::from("REQ-007/AC-01"),
        obligation_id: ObligationId::from("REQ-007/AC-01/VO-01"),
        source: EvidenceSource::AgentSuggestion,
        result: EvidenceResult::Passed,
        revision: "abc".into(),
        observed_at: now(),
        provenance: EvidenceProvenance::agent_proposal("architect"),
        rationale: "Run true".into(),
        evidence_refs: Vec::new(),
        validity: EvidenceValidity::Valid,
      })
      .expect("suggestion");

    assert_eq!(state(&graph, "abc", "suite"), VerificationState::Unverified);
  }
  #[test]
  fn newer_same_revision_verification_supersedes_failed_artifacts() {
    let mut graph = graph();
    graph
      .obligations
      .get_mut(&ObligationId::from("REQ-007/AC-01/VO-01"))
      .expect("obligation")
      .evidence_contract = EvidenceContract::Artifact {
      predicate: EvidencePredicate::NamedProjectCheck {
        name: "quality".into(),
      },
    };
    let failed = project_run("abc", "suite", false);
    graph
      .record_project_artifacts(&failed)
      .expect("failed artifacts");
    graph.derive_proofs("abc");
    assert_eq!(
      graph.proof_derivations[&ObligationId::from("REQ-007/AC-01/VO-01")].state,
      ProofState::Contradicted
    );

    let passed = project_run("abc", "suite", true);
    graph
      .record_project_artifacts(&passed)
      .expect("replacement artifacts");
    graph.derive_proofs("abc");
    assert_eq!(
      graph.proof_derivations[&ObligationId::from("REQ-007/AC-01/VO-01")].state,
      ProofState::Proven
    );
    assert_eq!(
      graph
        .artifacts
        .values()
        .filter(|artifact| artifact.validity.is_valid())
        .count(),
      1
    );
  }
}
