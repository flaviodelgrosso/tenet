use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
  ids::{CriterionId, EvidenceId, ObligationId, RequirementId, VerificationRunId},
  verification::CommandResult,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImplementationState {
  Present,
  Partial,
  Absent,
  Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
  Verified,
  PartiallyVerified,
  Unverified,
  Stale,
  Contradicted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationKind {
  AutomatedTest,
  Build,
  Lint,
  Command,
  RepositoryObservation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
  AutomatedTest,
  Build,
  Lint,
  Command,
  RepositoryObservation,
  ModelAssertion,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceResult {
  Passed,
  Failed,
  Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum EvidenceProvenance {
  ControllerExecution {
    #[serde(rename = "verificationRunId")]
    verification_run_id: VerificationRunId,
  },
  ModelObservation {
    #[serde(rename = "workerRole")]
    worker_role: String,
  },
}

impl EvidenceProvenance {
  pub fn controller_execution(run_id: VerificationRunId) -> Self {
    Self::ControllerExecution {
      verification_run_id: run_id,
    }
  }

  pub fn model_observation(worker_role: impl Into<String>) -> Self {
    Self::ModelObservation {
      worker_role: worker_role.into(),
    }
  }

  pub fn is_controller_execution(&self) -> bool {
    matches!(self, Self::ControllerExecution { .. })
  }

  pub fn verification_run_id(&self) -> Option<VerificationRunId> {
    match self {
      Self::ControllerExecution {
        verification_run_id,
      } => Some(*verification_run_id),
      Self::ModelObservation { .. } => None,
    }
  }

  pub fn worker_role(&self) -> Option<&str> {
    match self {
      Self::ControllerExecution { .. } => None,
      Self::ModelObservation { worker_role } => Some(worker_role),
    }
  }
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

  pub fn superseded_by_revision(&self) -> Option<&str> {
    match self {
      Self::Valid => None,
      Self::Stale {
        superseded_by_revision,
        ..
      } => Some(superseded_by_revision),
    }
  }
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerificationObligation {
  pub id: ObligationId,
  #[serde(rename = "criterionId")]
  pub criterion_id: CriterionId,
  pub description: String,
  pub kind: VerificationKind,
  #[serde(default = "default_true")]
  pub required: bool,
  pub command: String,
  #[serde(rename = "dependencyScope")]
  pub dependency_scope: Vec<String>,
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
  pub kind: EvidenceKind,
  pub result: EvidenceResult,
  #[serde(rename = "checkIdentity")]
  pub check_identity: String,
  pub revision: String,
  #[serde(rename = "observedAt")]
  pub observed_at: DateTime<Utc>,
  pub provenance: EvidenceProvenance,
  pub output: String,
  pub validity: EvidenceValidity,
  #[serde(rename = "dependencyScope")]
  pub dependency_scope: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EvidencePolicy;

impl EvidencePolicy {
  pub fn authorizes(&self, evidence: &Evidence) -> bool {
    evidence.validity.is_valid()
      && evidence.result == EvidenceResult::Passed
      && evidence.provenance.is_controller_execution()
  }

  pub fn blocks(&self, evidence: &Evidence) -> bool {
    evidence.validity.is_valid()
      && evidence.result == EvidenceResult::Failed
      && evidence.provenance.is_controller_execution()
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
  pub evidence: BTreeMap<EvidenceId, Evidence>,
}

impl EvidenceGraphState {
  pub const VERSION: u32 = 1;

  pub fn new(specification_hash: impl Into<String>) -> Self {
    Self {
      version: Self::VERSION,
      specification_hash: specification_hash.into(),
      requirements: BTreeSet::new(),
      required_requirements: BTreeSet::new(),
      criteria: BTreeMap::new(),
      obligations: BTreeMap::new(),
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
    if obligation.description.trim().is_empty() || obligation.command.trim().is_empty() {
      return Err(EvidenceGraphError::BlankDescription(
        obligation.id.to_string(),
      ));
    }
    if obligation.dependency_scope.is_empty()
      || obligation
        .dependency_scope
        .iter()
        .any(|scope| scope.trim().is_empty())
    {
      return Err(EvidenceGraphError::EmptyDependencyScope(
        obligation.id.clone(),
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

  pub fn record_command_result(
    &mut self,
    obligation_id: &ObligationId,
    revision: impl Into<String>,
    run_id: VerificationRunId,
    observed_at: DateTime<Utc>,
    result: &CommandResult,
  ) -> Result<EvidenceId, EvidenceGraphError> {
    let obligation = self
      .obligations
      .get(obligation_id)
      .ok_or_else(|| EvidenceGraphError::UnknownObligation(obligation_id.clone()))?;
    let criterion = self
      .criteria
      .get(&obligation.criterion_id)
      .ok_or_else(|| EvidenceGraphError::UnknownCriterion(obligation.criterion_id.clone()))?;
    let id = EvidenceId::new();
    let output = match (result.stdout.is_empty(), result.stderr.is_empty()) {
      (false, false) => format!("{}\n{}", result.stdout, result.stderr),
      (false, true) => result.stdout.clone(),
      (true, false) => result.stderr.clone(),
      (true, true) => String::new(),
    };
    let evidence = Evidence {
      id,
      requirement_id: criterion.requirement_id.clone(),
      criterion_id: criterion.id.clone(),
      obligation_id: obligation.id.clone(),
      kind: evidence_kind(obligation.kind),
      result: if result.exit_code == Some(0) && !result.timed_out {
        EvidenceResult::Passed
      } else {
        EvidenceResult::Failed
      },
      check_identity: result.command.clone(),
      revision: revision.into(),
      observed_at,
      provenance: EvidenceProvenance::controller_execution(run_id),
      output,
      validity: EvidenceValidity::Valid,
      dependency_scope: obligation.dependency_scope.clone(),
    };
    self.establish_evidence(evidence)?;
    Ok(id)
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
    policy: &EvidencePolicy,
  ) -> Result<VerificationState, EvidenceGraphError> {
    let criterion = self
      .criteria
      .get(criterion_id)
      .ok_or_else(|| EvidenceGraphError::UnknownCriterion(criterion_id.clone()))?;
    let required: Vec<_> = self
      .obligations
      .values()
      .filter(|obligation| obligation.criterion_id == criterion.id && obligation.required)
      .collect();
    if required.is_empty() {
      return Ok(VerificationState::Unverified);
    }

    let states: Vec<_> = required
      .iter()
      .map(|obligation| self.obligation_state(&obligation.id, policy))
      .collect();
    if states.contains(&VerificationState::Contradicted) {
      return Ok(VerificationState::Contradicted);
    }
    if states
      .iter()
      .all(|state| *state == VerificationState::Verified)
    {
      return Ok(VerificationState::Verified);
    }
    if states.contains(&VerificationState::Stale) {
      return Ok(VerificationState::Stale);
    }
    if states.contains(&VerificationState::Verified) {
      return Ok(VerificationState::PartiallyVerified);
    }
    Ok(VerificationState::Unverified)
  }

  pub fn requirement_verification_state(
    &self,
    requirement_id: &RequirementId,
    policy: &EvidencePolicy,
  ) -> Result<VerificationState, EvidenceGraphError> {
    if !self.requirements.contains(requirement_id) {
      return Err(EvidenceGraphError::UnknownRequirement(
        requirement_id.clone(),
      ));
    }
    let mandatory: Vec<_> = self
      .criteria
      .values()
      .filter(|criterion| criterion.requirement_id == *requirement_id && criterion.mandatory)
      .collect();
    if mandatory.is_empty() {
      return Ok(VerificationState::Unverified);
    }
    let states: Result<Vec<_>, _> = mandatory
      .iter()
      .map(|criterion| self.criterion_verification_state(&criterion.id, policy))
      .collect();
    let states = states?;
    if states.contains(&VerificationState::Contradicted) {
      return Ok(VerificationState::Contradicted);
    }
    if states
      .iter()
      .all(|state| *state == VerificationState::Verified)
    {
      return Ok(VerificationState::Verified);
    }
    if states.contains(&VerificationState::Stale) {
      return Ok(VerificationState::Stale);
    }
    if states.contains(&VerificationState::Verified)
      || states.contains(&VerificationState::PartiallyVerified)
    {
      return Ok(VerificationState::PartiallyVerified);
    }
    Ok(VerificationState::Unverified)
  }

  pub fn all_required_verified(&self, policy: &EvidencePolicy) -> bool {
    self.required_requirements.iter().all(|requirement_id| {
      self.requirement_verification_state(requirement_id, policy) == Ok(VerificationState::Verified)
    })
  }

  pub fn projection(
    &self,
    requirement_id: &RequirementId,
    policy: &EvidencePolicy,
  ) -> Result<EvidenceProjection, EvidenceGraphError> {
    let verification_state = self.requirement_verification_state(requirement_id, policy)?;
    let criteria = self
      .criteria
      .values()
      .filter(|criterion| criterion.requirement_id == *requirement_id)
      .map(|criterion| {
        let obligations = self
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
          .collect();
        CriterionProjection {
          criterion: criterion.clone(),
          state: self
            .criterion_verification_state(&criterion.id, policy)
            .unwrap_or(VerificationState::Unverified),
          obligations,
        }
      })
      .collect();
    Ok(EvidenceProjection {
      requirement_id: requirement_id.clone(),
      verification_state,
      criteria,
    })
  }

  fn obligation_state(
    &self,
    obligation_id: &ObligationId,
    policy: &EvidencePolicy,
  ) -> VerificationState {
    let evidence: Vec<_> = self
      .evidence
      .values()
      .filter(|item| item.obligation_id == *obligation_id)
      .collect();
    if evidence.iter().any(|item| policy.blocks(item)) {
      return VerificationState::Contradicted;
    }
    if evidence.iter().any(|item| policy.authorizes(item)) {
      return VerificationState::Verified;
    }
    if evidence.iter().any(|item| {
      !item.validity.is_valid()
        && item.result == EvidenceResult::Passed
        && item.provenance.is_controller_execution()
    }) {
      return VerificationState::Stale;
    }
    VerificationState::Unverified
  }
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
  #[error("{0} has a blank description or command")]
  BlankDescription(String),
  #[error("verification obligation {0} has no dependency scope")]
  EmptyDependencyScope(ObligationId),
}

fn evidence_kind(kind: VerificationKind) -> EvidenceKind {
  match kind {
    VerificationKind::AutomatedTest => EvidenceKind::AutomatedTest,
    VerificationKind::Build => EvidenceKind::Build,
    VerificationKind::Lint => EvidenceKind::Lint,
    VerificationKind::Command => EvidenceKind::Command,
    VerificationKind::RepositoryObservation => EvidenceKind::RepositoryObservation,
  }
}

fn default_true() -> bool {
  true
}

#[cfg(test)]
mod tests {
  use chrono::TimeZone;
  use proptest::prelude::*;

  use super::*;

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
        description: "Submit an expired token and observe HTTP 401".into(),
        kind: VerificationKind::AutomatedTest,
        required: true,
        command: "cargo test expired_token_returns_401".into(),
        dependency_scope: vec!["src/auth.rs".into(), "tests/auth.rs".into()],
      })
      .expect("obligation");
    graph
  }

  fn command(exit_code: i32) -> CommandResult {
    CommandResult {
      command: "cargo test expired_token_returns_401".into(),
      exit_code: Some(exit_code),
      timed_out: false,
      duration_ms: 10,
      stdout: "test result".into(),
      stderr: String::new(),
    }
  }

  fn observed_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 16, 10, 0, 0).unwrap()
  }

  #[test]
  fn criterion_identity_is_stable_and_relationship_is_explicit() {
    let graph = graph();
    let criterion = &graph.criteria[&CriterionId::from("REQ-007/AC-01")];

    assert_eq!(criterion.requirement_id, RequirementId::from("REQ-007"));
  }

  #[test]
  fn obligation_rejects_an_unknown_criterion() {
    let mut graph = graph();
    let error = graph
      .add_obligation(VerificationObligation {
        id: ObligationId::from("REQ-007/AC-02/VO-01"),
        criterion_id: CriterionId::from("REQ-007/AC-02"),
        description: "Unknown".into(),
        kind: VerificationKind::Command,
        required: true,
        command: "true".into(),
        dependency_scope: vec!["**".into()],
      })
      .expect_err("unknown criterion");

    assert_eq!(
      error,
      EvidenceGraphError::UnknownCriterion(CriterionId::from("REQ-007/AC-02"))
    );
  }

  #[test]
  fn missing_required_evidence_keeps_requirement_unverified() {
    assert_eq!(
      graph()
        .requirement_verification_state(&RequirementId::from("REQ-007"), &EvidencePolicy)
        .expect("state"),
      VerificationState::Unverified
    );
  }

  #[test]
  fn controller_command_result_verifies_requirement() {
    let mut graph = graph();
    graph
      .record_command_result(
        &ObligationId::from("REQ-007/AC-01/VO-01"),
        "7ca311f",
        VerificationRunId::new(),
        observed_at(),
        &command(0),
      )
      .expect("evidence");

    assert_eq!(
      graph
        .requirement_verification_state(&RequirementId::from("REQ-007"), &EvidencePolicy)
        .expect("state"),
      VerificationState::Verified
    );
  }

  #[test]
  fn advisory_model_evidence_cannot_verify_requirement() {
    let mut graph = graph();
    graph
      .establish_evidence(Evidence {
        id: EvidenceId::new(),
        requirement_id: RequirementId::from("REQ-007"),
        criterion_id: CriterionId::from("REQ-007/AC-01"),
        obligation_id: ObligationId::from("REQ-007/AC-01/VO-01"),
        kind: EvidenceKind::ModelAssertion,
        result: EvidenceResult::Passed,
        check_identity: "model observation".into(),
        revision: "7ca311f".into(),
        observed_at: observed_at(),
        provenance: EvidenceProvenance::model_observation("reconcile"),
        output: "src/auth.rs appears correct".into(),
        validity: EvidenceValidity::Valid,
        dependency_scope: vec!["src/auth.rs".into()],
      })
      .expect("advisory evidence");

    assert_eq!(
      graph
        .requirement_verification_state(&RequirementId::from("REQ-007"), &EvidencePolicy)
        .expect("state"),
      VerificationState::Unverified
    );
  }

  #[test]
  fn stale_required_evidence_removes_verification() {
    let mut graph = graph();
    graph
      .record_command_result(
        &ObligationId::from("REQ-007/AC-01/VO-01"),
        "7ca311f",
        VerificationRunId::new(),
        observed_at(),
        &command(0),
      )
      .expect("evidence");
    graph.invalidate_where("abc123", observed_at(), |_| true);

    assert_eq!(
      graph
        .requirement_verification_state(&RequirementId::from("REQ-007"), &EvidencePolicy)
        .expect("state"),
      VerificationState::Stale
    );
  }

  #[test]
  fn contradictory_controller_evidence_is_preserved_and_blocks_verification() {
    let mut graph = graph();
    for exit_code in [0, 1] {
      graph
        .record_command_result(
          &ObligationId::from("REQ-007/AC-01/VO-01"),
          "7ca311f",
          VerificationRunId::new(),
          observed_at(),
          &command(exit_code),
        )
        .expect("evidence");
    }

    assert_eq!(
      graph
        .requirement_verification_state(&RequirementId::from("REQ-007"), &EvidencePolicy)
        .expect("state"),
      VerificationState::Contradicted
    );
    assert_eq!(graph.evidence.len(), 2);
  }

  #[test]
  fn mandatory_criterion_without_required_obligation_is_unverified() {
    let mut graph = EvidenceGraphState::new("spec-hash");
    graph.register_requirement(RequirementId::from("REQ-008"), true);
    graph
      .add_criterion(AcceptanceCriterion {
        id: CriterionId::from("REQ-008/AC-01"),
        requirement_id: RequirementId::from("REQ-008"),
        description: "Observable behavior".into(),
        mandatory: true,
      })
      .expect("criterion");

    assert_eq!(
      graph.requirement_verification_state(&RequirementId::from("REQ-008"), &EvidencePolicy),
      Ok(VerificationState::Unverified)
    );
  }

  #[test]
  fn projection_traces_requirement_to_revision_bound_evidence() {
    let mut graph = graph();
    graph
      .record_command_result(
        &ObligationId::from("REQ-007/AC-01/VO-01"),
        "7ca311f",
        VerificationRunId::new(),
        observed_at(),
        &command(0),
      )
      .expect("evidence");

    let projection = graph
      .projection(&RequirementId::from("REQ-007"), &EvidencePolicy)
      .expect("projection");

    assert_eq!(projection.verification_state, VerificationState::Verified);
    assert_eq!(
      projection.criteria[0].criterion.id,
      CriterionId::from("REQ-007/AC-01")
    );
    assert_eq!(
      projection.criteria[0].obligations[0].obligation.id,
      ObligationId::from("REQ-007/AC-01/VO-01")
    );
    assert_eq!(
      projection.criteria[0].obligations[0].evidence[0].revision,
      "7ca311f"
    );
  }

  proptest! {
    #[test]
    fn serialization_round_trip_preserves_graph_semantics(revision in "[a-f0-9]{7,40}") {
      let mut graph = graph();
      graph.record_command_result(
        &ObligationId::from("REQ-007/AC-01/VO-01"),
        revision,
        VerificationRunId::new(),
        observed_at(),
        &command(0),
      ).expect("evidence");
      let encoded = serde_json::to_vec(&graph).expect("serialize");
      let decoded: EvidenceGraphState = serde_json::from_slice(&encoded).expect("deserialize");

      prop_assert_eq!(
        decoded.requirement_verification_state(&RequirementId::from("REQ-007"), &EvidencePolicy),
        Ok(VerificationState::Verified)
      );
    }
  }
}
