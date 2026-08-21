use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
  ids::{CriterionId, EvidenceId, ObligationId, RequirementId, VerificationRunId},
  verification::{ProjectCheckResult, ProjectVerificationRun},
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ObligationAssessment {
  Satisfied {
    rationale: String,
    #[serde(rename = "evidenceRefs")]
    evidence_refs: Vec<String>,
  },
  Gap {
    description: String,
  },
  Uncertain {
    reason: String,
    #[serde(default, rename = "specificationAmbiguous")]
    specification_ambiguous: bool,
  },
}

impl ObligationAssessment {
  fn result(&self) -> EvidenceResult {
    match self {
      Self::Satisfied { .. } => EvidenceResult::Passed,
      Self::Gap { .. } => EvidenceResult::Failed,
      Self::Uncertain { .. } => EvidenceResult::Inconclusive,
    }
  }

  fn rationale(&self) -> &str {
    match self {
      Self::Satisfied { rationale, .. } => rationale,
      Self::Gap { description } => description,
      Self::Uncertain { reason, .. } => reason,
    }
  }

  fn evidence_refs(&self) -> &[String] {
    match self {
      Self::Satisfied { evidence_refs, .. } => evidence_refs,
      Self::Gap { .. } | Self::Uncertain { .. } => &[],
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ObligationAssessmentResult {
  #[serde(rename = "obligationId")]
  pub obligation_id: ObligationId,
  pub assessment: ObligationAssessment,
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
  pub evidence: BTreeMap<EvidenceId, Evidence>,
}

impl EvidenceGraphState {
  pub const VERSION: u32 = 3;

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
  ) -> Result<Vec<EvidenceId>, EvidenceGraphError> {
    if report.summary.trim().is_empty() {
      return Err(EvidenceGraphError::BlankSemanticSummary);
    }
    let expected: BTreeSet<_> = self
      .obligations
      .values()
      .filter(|obligation| obligation.required)
      .map(|obligation| obligation.id.clone())
      .collect();
    let actual: BTreeSet<_> = report
      .assessments
      .iter()
      .map(|assessment| assessment.obligation_id.clone())
      .collect();
    if expected != actual || actual.len() != report.assessments.len() {
      return Err(EvidenceGraphError::SemanticAssessmentCoverageMismatch);
    }

    let mut ids = Vec::with_capacity(report.assessments.len());
    for item in &report.assessments {
      if item.assessment.rationale().trim().is_empty() {
        return Err(EvidenceGraphError::BlankAssessment(
          item.obligation_id.clone(),
        ));
      }
      let obligation = self
        .obligations
        .get(&item.obligation_id)
        .ok_or_else(|| EvidenceGraphError::UnknownObligation(item.obligation_id.clone()))?;
      let criterion = self
        .criteria
        .get(&obligation.criterion_id)
        .ok_or_else(|| EvidenceGraphError::UnknownCriterion(obligation.criterion_id.clone()))?;
      let id = EvidenceId::new();
      self.establish_evidence(Evidence {
        id,
        requirement_id: criterion.requirement_id.clone(),
        criterion_id: criterion.id.clone(),
        obligation_id: obligation.id.clone(),
        source: EvidenceSource::SemanticAssessment,
        result: item.assessment.result(),
        revision: revision.to_owned(),
        observed_at,
        provenance: EvidenceProvenance::independent_assessment(worker_id),
        rationale: item.assessment.rationale().to_owned(),
        evidence_refs: item.assessment.evidence_refs().to_vec(),
        validity: EvidenceValidity::Valid,
      })?;
      ids.push(id);
    }
    Ok(ids)
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
    let evidence: Vec<_> = self
      .evidence
      .values()
      .filter(|evidence| evidence.obligation_id == *obligation_id)
      .collect();
    if evidence.iter().any(|evidence| policy.blocks(evidence)) {
      return VerificationState::Contradicted;
    }
    if evidence
      .iter()
      .any(|evidence| policy.is_uncertain(evidence))
    {
      return VerificationState::Uncertain;
    }
    if evidence
      .iter()
      .any(|evidence| policy.authorizes(self, evidence))
    {
      return VerificationState::Verified;
    }
    if evidence.iter().any(|evidence| {
      evidence.result == EvidenceResult::Passed
        && evidence.source == EvidenceSource::SemanticAssessment
        && (!evidence.validity.is_valid() || evidence.revision != policy.revision)
    }) {
      return VerificationState::Stale;
    }
    VerificationState::Unverified
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
  use chrono::TimeZone;

  use super::*;
  use crate::verification::{CommandResult, ProjectCheckResult};

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

  fn assessment(assessment: ObligationAssessment) -> SemanticAssessmentReport {
    SemanticAssessmentReport {
      summary: "Independent assessment".into(),
      assessments: vec![ObligationAssessmentResult {
        obligation_id: ObligationId::from("REQ-007/AC-01/VO-01"),
        assessment,
      }],
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
  fn semantic_satisfaction_alone_does_not_verify_when_project_checks_fail() {
    let mut graph = graph();
    graph.record_project_verification(&project_run("abc", "suite", false));
    graph
      .record_semantic_assessment(
        "abc",
        now(),
        "assess-1",
        &assessment(ObligationAssessment::Satisfied {
          rationale: "Implementation rejects the token".into(),
          evidence_refs: vec!["src/auth.rs".into()],
        }),
      )
      .expect("semantic evidence");

    assert_eq!(state(&graph, "abc", "suite"), VerificationState::Unverified);
  }

  #[test]
  fn project_pass_and_semantic_satisfaction_verify_obligation() {
    let mut graph = graph();
    graph.record_project_verification(&project_run("abc", "suite", true));
    graph
      .record_semantic_assessment(
        "abc",
        now(),
        "assess-1",
        &assessment(ObligationAssessment::Satisfied {
          rationale: "Implementation rejects the token".into(),
          evidence_refs: vec!["src/auth.rs".into()],
        }),
      )
      .expect("semantic evidence");

    assert_eq!(state(&graph, "abc", "suite"), VerificationState::Verified);
  }

  #[test]
  fn semantic_gap_contradicts_project_pass() {
    let mut graph = graph();
    graph.record_project_verification(&project_run("abc", "suite", true));
    graph
      .record_semantic_assessment(
        "abc",
        now(),
        "assess-1",
        &assessment(ObligationAssessment::Gap {
          description: "Expired tokens are accepted".into(),
        }),
      )
      .expect("semantic evidence");

    assert_eq!(
      state(&graph, "abc", "suite"),
      VerificationState::Contradicted
    );
  }

  #[test]
  fn semantic_uncertainty_fails_closed() {
    let mut graph = graph();
    graph.record_project_verification(&project_run("abc", "suite", true));
    graph
      .record_semantic_assessment(
        "abc",
        now(),
        "assess-1",
        &assessment(ObligationAssessment::Uncertain {
          reason: "Criterion is ambiguous".into(),
          specification_ambiguous: true,
        }),
      )
      .expect("semantic evidence");

    assert_eq!(state(&graph, "abc", "suite"), VerificationState::Uncertain);
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
  fn evidence_is_bound_to_repository_revision() {
    let mut graph = graph();
    graph.record_project_verification(&project_run("abc", "suite", true));
    graph
      .record_semantic_assessment(
        "abc",
        now(),
        "assess-1",
        &assessment(ObligationAssessment::Satisfied {
          rationale: "Satisfied at abc".into(),
          evidence_refs: Vec::new(),
        }),
      )
      .expect("semantic evidence");

    assert_ne!(state(&graph, "def", "suite"), VerificationState::Verified);
  }

  #[test]
  fn changing_project_suite_invalidates_verified_eligibility() {
    let mut graph = graph();
    graph.record_project_verification(&project_run("abc", "suite-a", true));
    graph
      .record_semantic_assessment(
        "abc",
        now(),
        "assess-1",
        &assessment(ObligationAssessment::Satisfied {
          rationale: "Satisfied".into(),
          evidence_refs: Vec::new(),
        }),
      )
      .expect("semantic evidence");

    assert_ne!(state(&graph, "abc", "suite-b"), VerificationState::Verified);
  }

  #[test]
  fn old_semantic_assessment_is_stale_after_revision_change() {
    let mut graph = graph();
    graph.record_project_verification(&project_run("abc", "suite", true));
    graph
      .record_semantic_assessment(
        "abc",
        now(),
        "assess-1",
        &assessment(ObligationAssessment::Satisfied {
          rationale: "Satisfied".into(),
          evidence_refs: Vec::new(),
        }),
      )
      .expect("semantic evidence");
    graph.invalidate_where("def", now(), |_| true);

    assert_eq!(state(&graph, "def", "suite"), VerificationState::Stale);
  }

  #[test]
  fn contradictory_project_results_do_not_use_optimistic_voting() {
    let mut graph = graph();
    graph.record_project_verification(&project_run("abc", "suite", true));
    graph.record_project_verification(&project_run("abc", "suite", false));
    graph
      .record_semantic_assessment(
        "abc",
        now(),
        "assess-1",
        &assessment(ObligationAssessment::Satisfied {
          rationale: "Satisfied".into(),
          evidence_refs: Vec::new(),
        }),
      )
      .expect("semantic evidence");

    assert_ne!(state(&graph, "abc", "suite"), VerificationState::Verified);
  }
}
