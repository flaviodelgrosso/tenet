use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
  ids::{CriterionId, EvidenceId, ObligationId, RequirementId, VerificationRunId},
  verification::{
    DependencyScopeAuthority, VerificationAuthority, VerificationExecutionResult, VerificationSpec,
  },
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
  pub spec: VerificationSpec,
  pub authority: VerificationAuthority,
  #[serde(rename = "dependencyScope")]
  pub dependency_scope: Vec<String>,
  #[serde(rename = "dependencyScopeAuthority")]
  pub dependency_scope_authority: DependencyScopeAuthority,
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
  #[serde(rename = "verificationAuthority")]
  pub verification_authority: VerificationAuthority,
  pub output: String,
  pub validity: EvidenceValidity,
  #[serde(rename = "dependencyScope")]
  pub dependency_scope: Vec<String>,
  #[serde(rename = "dependencyScopeAuthority")]
  pub dependency_scope_authority: DependencyScopeAuthority,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EvidencePolicy;

impl EvidencePolicy {
  pub fn authorizes(&self, evidence: &Evidence) -> bool {
    evidence.validity.is_valid()
      && evidence.result == EvidenceResult::Passed
      && evidence.provenance.is_controller_execution()
      && evidence.verification_authority.is_trusted()
  }

  pub fn blocks(&self, evidence: &Evidence) -> bool {
    evidence.validity.is_valid()
      && evidence.result == EvidenceResult::Failed
      && evidence.provenance.is_controller_execution()
      && evidence.verification_authority.is_trusted()
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
  pub const VERSION: u32 = 2;

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
    if obligation.description.trim().is_empty() || obligation.spec.program.trim().is_empty() {
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

  pub fn record_execution_result(
    &mut self,
    revision: impl Into<String>,
    observed_at: DateTime<Utc>,
    execution: &VerificationExecutionResult,
  ) -> Result<EvidenceId, EvidenceGraphError> {
    let obligation = self
      .obligations
      .get(&execution.obligation_id)
      .ok_or_else(|| EvidenceGraphError::UnknownObligation(execution.obligation_id.clone()))?;
    if obligation.spec != execution.spec
      || obligation.authority != execution.authority
      || execution.result.command != execution.spec.identity()
    {
      return Err(EvidenceGraphError::ExecutionMismatch(
        execution.obligation_id.clone(),
      ));
    }
    let criterion = self
      .criteria
      .get(&obligation.criterion_id)
      .ok_or_else(|| EvidenceGraphError::UnknownCriterion(obligation.criterion_id.clone()))?;
    let result = &execution.result;
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
      check_identity: execution.spec.identity(),
      revision: revision.into(),
      observed_at,
      provenance: EvidenceProvenance::controller_execution(execution.run_id),
      verification_authority: execution.authority,
      output,
      validity: EvidenceValidity::Valid,
      dependency_scope: obligation.dependency_scope.clone(),
      dependency_scope_authority: obligation.dependency_scope_authority,
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
  #[error("verification execution does not match obligation {0}")]
  ExecutionMismatch(ObligationId),
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
  use crate::verification::{
    CommandResult, DependencyScopeAuthority, VerificationAuthority, VerificationExecutionResult,
    VerificationSpec,
  };

  fn verification_spec() -> VerificationSpec {
    VerificationSpec {
      program: "cargo".into(),
      args: vec!["test".into(), "expired_token_returns_401".into()],
      working_directory: ".".into(),
      environment: Default::default(),
    }
  }

  fn graph(authority: VerificationAuthority) -> EvidenceGraphState {
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
        spec: verification_spec(),
        authority,
        dependency_scope: vec!["src/auth.rs".into(), "tests/auth.rs".into()],
        dependency_scope_authority: DependencyScopeAuthority::ProjectConfigured,
      })
      .expect("obligation");
    graph
  }

  fn command(exit_code: i32) -> CommandResult {
    CommandResult {
      command: verification_spec().identity(),
      exit_code: Some(exit_code),
      timed_out: false,
      duration_ms: 10,
      stdout: "test result".into(),
      stderr: String::new(),
    }
  }

  fn execution(authority: VerificationAuthority, exit_code: i32) -> VerificationExecutionResult {
    VerificationExecutionResult {
      run_id: VerificationRunId::new(),
      obligation_id: ObligationId::from("REQ-007/AC-01/VO-01"),
      spec: verification_spec(),
      authority,
      result: command(exit_code),
    }
  }

  fn observed_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 16, 10, 0, 0).unwrap()
  }

  #[test]
  fn mandatory_criterion_without_trusted_evidence_is_unverified() {
    assert_eq!(
      graph(VerificationAuthority::ProjectConfigured)
        .requirement_verification_state(&RequirementId::from("REQ-007"), &EvidencePolicy),
      Ok(VerificationState::Unverified)
    );
  }

  #[test]
  fn trusted_project_execution_verifies_requirement() {
    let mut graph = graph(VerificationAuthority::ProjectConfigured);
    graph
      .record_execution_result(
        "7ca311f",
        observed_at(),
        &execution(VerificationAuthority::ProjectConfigured, 0),
      )
      .expect("evidence");

    assert_eq!(
      graph.requirement_verification_state(&RequirementId::from("REQ-007"), &EvidencePolicy),
      Ok(VerificationState::Verified)
    );
  }

  #[test]
  fn agent_proposed_true_cannot_verify_requirement() {
    let mut graph = graph(VerificationAuthority::AgentProposed);
    let true_spec = VerificationSpec {
      program: "true".into(),
      args: Vec::new(),
      working_directory: ".".into(),
      environment: Default::default(),
    };
    graph
      .obligations
      .get_mut(&ObligationId::from("REQ-007/AC-01/VO-01"))
      .expect("obligation")
      .spec = true_spec.clone();
    let mut execution = execution(VerificationAuthority::AgentProposed, 0);
    execution.spec = true_spec.clone();
    execution.result.command = true_spec.identity();
    graph
      .record_execution_result("7ca311f", observed_at(), &execution)
      .expect("advisory evidence");

    assert_eq!(
      graph.requirement_verification_state(&RequirementId::from("REQ-007"), &EvidencePolicy),
      Ok(VerificationState::Unverified)
    );
  }

  #[test]
  fn result_bound_to_wrong_obligation_is_rejected() {
    let mut graph = graph(VerificationAuthority::ProjectConfigured);
    let mut execution = execution(VerificationAuthority::ProjectConfigured, 0);
    execution.obligation_id = ObligationId::from("REQ-999/AC-01/VO-01");

    assert_eq!(
      graph
        .record_execution_result("7ca311f", observed_at(), &execution)
        .expect_err("wrong obligation rejected"),
      EvidenceGraphError::UnknownObligation(ObligationId::from("REQ-999/AC-01/VO-01"))
    );
  }

  #[test]
  fn mismatched_execution_spec_is_rejected() {
    let mut graph = graph(VerificationAuthority::ProjectConfigured);
    let mut execution = execution(VerificationAuthority::ProjectConfigured, 0);
    execution.spec.args.push("unrelated".into());

    assert_eq!(
      graph
        .record_execution_result("7ca311f", observed_at(), &execution)
        .expect_err("mismatched spec rejected"),
      EvidenceGraphError::ExecutionMismatch(ObligationId::from("REQ-007/AC-01/VO-01"))
    );
  }

  #[test]
  fn contradictory_trusted_executions_are_preserved_and_block_verification() {
    let mut graph = graph(VerificationAuthority::ProjectConfigured);
    for exit_code in [0, 1] {
      graph
        .record_execution_result(
          "7ca311f",
          observed_at(),
          &execution(VerificationAuthority::ProjectConfigured, exit_code),
        )
        .expect("evidence");
    }

    assert_eq!(
      graph.requirement_verification_state(&RequirementId::from("REQ-007"), &EvidencePolicy),
      Ok(VerificationState::Contradicted)
    );
    assert_eq!(graph.evidence.len(), 2);
  }

  #[test]
  fn advisory_model_evidence_cannot_upgrade_requirement() {
    let mut graph = graph(VerificationAuthority::ProjectConfigured);
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
        verification_authority: VerificationAuthority::AgentProposed,
        output: "appears correct".into(),
        validity: EvidenceValidity::Valid,
        dependency_scope: vec!["src/auth.rs".into()],
        dependency_scope_authority: DependencyScopeAuthority::AgentProposed,
      })
      .expect("advisory evidence");

    assert_eq!(
      graph.requirement_verification_state(&RequirementId::from("REQ-007"), &EvidencePolicy),
      Ok(VerificationState::Unverified)
    );
  }

  proptest! {
    #[test]
    fn adding_untrusted_passing_evidence_never_upgrades_to_verified(count in 1usize..20) {
      let mut graph = graph(VerificationAuthority::AgentProposed);
      for _ in 0..count {
        graph.record_execution_result(
          "7ca311f",
          observed_at(),
          &execution(VerificationAuthority::AgentProposed, 0),
        ).expect("advisory evidence");
      }
      prop_assert_ne!(
        graph.requirement_verification_state(&RequirementId::from("REQ-007"), &EvidencePolicy),
        Ok(VerificationState::Verified)
      );
    }

    #[test]
    fn adding_trusted_failure_never_preserves_verified(failures in 1usize..20) {
      let mut graph = graph(VerificationAuthority::ProjectConfigured);
      graph.record_execution_result(
        "7ca311f",
        observed_at(),
        &execution(VerificationAuthority::ProjectConfigured, 0),
      ).expect("passing evidence");
      for _ in 0..failures {
        graph.record_execution_result(
          "7ca311f",
          observed_at(),
          &execution(VerificationAuthority::ProjectConfigured, 1),
        ).expect("failing evidence");
      }
      prop_assert_ne!(
        graph.requirement_verification_state(&RequirementId::from("REQ-007"), &EvidencePolicy),
        Ok(VerificationState::Verified)
      );
    }

    #[test]
    fn serialization_round_trip_preserves_trust_authority(revision in "[a-f0-9]{7,40}") {
      let mut graph = graph(VerificationAuthority::ProjectConfigured);
      graph.record_execution_result(
        revision,
        observed_at(),
        &execution(VerificationAuthority::ProjectConfigured, 0),
      ).expect("evidence");
      let encoded = serde_json::to_vec(&graph).expect("serialize");
      let decoded: EvidenceGraphState = serde_json::from_slice(&encoded).expect("deserialize");
      prop_assert_eq!(decoded, graph);
    }
  }
}
