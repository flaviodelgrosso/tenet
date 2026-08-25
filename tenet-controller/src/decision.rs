use std::{
  collections::{BTreeMap, BTreeSet},
  fmt::Write as _,
};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use tenet_domain::{
  error::DomainValidationError,
  evidence::{
    ImplementationState, ObligationAssessmentResult, SemanticAssessmentProposal,
    SemanticAssessmentReport,
  },
  ids::{ObligationId, RequirementId},
  model::{
    AgentReconciliationProposal, ReconcileResult, RequirementAssessment, RequirementCatalog,
    RunStatus, WorkUnit,
  },
};

/// Authoritative domain action requested by deterministic controller policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextAction {
  VerifyProject,
  ExecuteFrontier(Vec<WorkUnit>),
  IntegrateCandidates(Vec<tenet_domain::model::WorkExecution>),
  BeginNextCycle,
  Finish,
  Block(String),
}

fn is_terminal(status: &RunStatus) -> bool {
  matches!(
    status,
    RunStatus::Done | RunStatus::Blocked | RunStatus::Failed | RunStatus::Stopped
  )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagnationDecision {
  pub fingerprint: String,
  pub count: u32,
  pub blocked: bool,
}

/// Advances stagnation from explicit authoritative progress inputs.
pub fn advance_stagnation(
  previous_fingerprint: Option<&str>,
  previous_count: u32,
  fingerprint: String,
  limit: u32,
) -> StagnationDecision {
  let count = if previous_fingerprint == Some(fingerprint.as_str()) {
    previous_count.saturating_add(1)
  } else {
    0
  };
  StagnationDecision {
    fingerprint,
    count,
    blocked: count >= limit,
  }
}

/// Hashes explicit authoritative progress facts without observing external state.
pub fn progress_fingerprint(
  revision: &str,
  catalog_hash: &str,
  reconciliation: &ReconcileResult,
  active_discoveries: &[String],
) -> Result<String> {
  let mut active_discoveries = active_discoveries.to_vec();
  active_discoveries.sort();
  active_discoveries.dedup();
  let bytes = serde_json::to_vec(&(revision, catalog_hash, reconciliation, active_discoveries))?;
  let digest = Sha256::digest(bytes);
  let mut fingerprint = String::with_capacity(digest.len() * 2);
  for byte in digest {
    write!(fingerprint, "{byte:02x}")?;
  }
  Ok(fingerprint)
}

/// Admits canonical, deduplicated discoveries from explicit worker observations.
pub fn record_discoveries(
  state: &mut tenet_domain::model::State,
  catalog_hash: &str,
  repository_revision: &str,
  work_unit_id: &str,
  discoveries: &[tenet_domain::model::WorkerDiscovery],
) -> Result<()> {
  for discovered in discoveries {
    let identity = serde_json::to_vec(&(
      catalog_hash,
      repository_revision,
      work_unit_id,
      discovered.role,
      &discovered.discovery,
    ))?;
    let digest = Sha256::digest(identity);
    let mut fingerprint = String::with_capacity(digest.len() * 2);
    for byte in digest {
      write!(fingerprint, "{byte:02x}")?;
    }
    if state.discoveries.iter().any(|record| {
      record.fingerprint == fingerprint
        && record.status == tenet_domain::model::DiscoveryStatus::Active
    }) {
      continue;
    }
    state
      .discoveries
      .push(tenet_domain::model::DiscoveryRecord {
        fingerprint,
        discovery: discovered.discovery.clone(),
        catalog_hash: catalog_hash.into(),
        repository_revision: repository_revision.into(),
        work_unit_id: work_unit_id.into(),
        role: discovered.role,
        cycle: state.cycle,
        status: tenet_domain::model::DiscoveryStatus::Active,
      });
  }
  Ok(())
}

/// Chooses the next authoritative step after a validated reconciliation.
pub fn decide_reconciliation(
  status: RunStatus,
  has_work: bool,
  mut frontier: Vec<WorkUnit>,
  stagnation_limit_reached: Option<u32>,
) -> NextAction {
  if is_terminal(&status) {
    return NextAction::Finish;
  }
  if status != RunStatus::Running {
    return NextAction::Block("Run is not active".into());
  }
  if let Some(limit) = stagnation_limit_reached {
    return NextAction::Block(format!(
      "Stagnation limit ({limit}) reached without meaningful repository progress"
    ));
  }
  if !has_work {
    return NextAction::VerifyProject;
  }
  if frontier.is_empty() {
    return NextAction::Block(
      "Reconciliation found gaps but the validated work graph has no ready work units".into(),
    );
  }
  frontier.sort_by(|left, right| left.id.cmp(&right.id));
  NextAction::ExecuteFrontier(frontier)
}

/// Chooses deterministic integration or another cycle from scheduler outcomes.
pub fn decide_execution(
  status: RunStatus,
  mut candidates: Vec<tenet_domain::model::WorkExecution>,
) -> NextAction {
  if is_terminal(&status) {
    return NextAction::Finish;
  }
  if status != RunStatus::Running {
    return NextAction::Block("Run is not active".into());
  }
  tenet_runtime::integration::deterministic_order(&mut candidates);
  if candidates.is_empty() {
    NextAction::BeginNextCycle
  } else {
    NextAction::IntegrateCandidates(candidates)
  }
}

/// Applies a deterministic completion decision without consulting telemetry or external state.
pub fn apply_completion_decision(
  state: &mut tenet_domain::model::State,
  decision: &crate::completion::CompletionDecision,
) -> NextAction {
  if is_terminal(&state.status) {
    return NextAction::Finish;
  }
  if state.status != RunStatus::Running {
    return NextAction::Block("Run is not active".into());
  }
  match decision {
    crate::completion::CompletionDecision::Done => {
      state.status = RunStatus::Done;
      state.phase = tenet_domain::model::Phase::Complete;
      state.verification_layers.completion_eligible = true;
      state.last_summary = "Project checks pass and every required semantic obligation is independently satisfied at the current revision".into();
      NextAction::Finish
    }
    crate::completion::CompletionDecision::NotReady(blockers) => NextAction::Block(
      blockers
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; "),
    ),
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredCandidateClassification {
  pub retained: Vec<tenet_domain::model::DeferredCandidate>,
  pub stale_refs: Vec<String>,
}

/// Separates current deferred authority from candidates bound to stale catalog or revision facts.
pub fn classify_deferred_candidates(
  mut candidates: Vec<tenet_domain::model::DeferredCandidate>,
  catalog_hash: &str,
  revision: &str,
) -> DeferredCandidateClassification {
  candidates.sort_by(|left, right| left.git_ref.cmp(&right.git_ref));
  let (retained, stale): (Vec<_>, Vec<_>) = candidates.into_iter().partition(|candidate| {
    candidate.catalog_hash == catalog_hash && candidate.base_revision == revision
  });
  DeferredCandidateClassification {
    retained,
    stale_refs: stale
      .into_iter()
      .map(|candidate| candidate.git_ref)
      .collect(),
  }
}

/// Plans fallible deferred-ref cleanup without destroying recovery metadata.
pub fn plan_spec_invalidation(state: &tenet_domain::model::State) -> Result<Vec<String>> {
  if !state.active_leases.is_empty() || !state.candidate_integrations.is_empty() {
    bail!("cannot invalidate specification authority while work or integration is active");
  }
  let mut references: Vec<_> = state
    .deferred_candidates
    .iter()
    .map(|candidate| candidate.git_ref.clone())
    .collect();
  references.sort();
  Ok(references)
}

/// Commits in-memory invalidation after every fallible Git cleanup succeeds.
pub fn apply_spec_invalidation(state: &mut tenet_domain::model::State) {
  state.deferred_candidates.clear();
  state.completed_work_units.clear();
  state.current_repair = None;
  for status in state.work_statuses.values_mut() {
    *status = tenet_domain::model::WorkStatus::Invalidated;
  }
  state.discoveries.clear();
  state.requirement_counts = Default::default();
  state.verification_layers = Default::default();
  state.progress_fingerprint = None;
  state.stagnation_count = 0;
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionAuthorization {
  pub work_units: Vec<WorkUnit>,
  pub deferred_candidates: Vec<Option<tenet_domain::model::DeferredCandidate>>,
}
/// Selects current, scope-authorized deferred candidates independently of input ordering.
pub fn authorize_frontier(
  mut frontier: Vec<WorkUnit>,
  candidates: &[tenet_domain::model::DeferredCandidate],
  catalog_hash: &str,
  revision: &str,
) -> Result<ExecutionAuthorization> {
  frontier.sort_by(|left, right| left.id.cmp(&right.id));
  let mut candidates: Vec<_> = candidates.iter().collect();
  candidates.sort_by(|left, right| left.git_ref.cmp(&right.git_ref));
  let mut work_units = Vec::with_capacity(frontier.len());
  let mut deferred_candidates = Vec::with_capacity(frontier.len());
  for unit in frontier {
    let mut saw_matching = false;
    let mut authorized = None;
    for candidate in &candidates {
      if candidate.catalog_hash != catalog_hash
        || candidate.base_revision != revision
        || !tenet_runtime::scheduler::deferred_candidate_targets_unit(candidate, &unit)?
      {
        continue;
      }
      saw_matching = true;
      if tenet_runtime::scheduler::deferred_candidate_authorized(candidate, &unit)? {
        authorized = Some((*candidate).clone());
        break;
      }
    }
    if !saw_matching || authorized.is_some() {
      work_units.push(unit);
      deferred_candidates.push(authorized);
    }
  }
  Ok(ExecutionAuthorization {
    work_units,
    deferred_candidates,
  })
}

/// Returns the canonical catalog projection supplied to reconciliation agents.
pub fn reconciliation_catalog(catalog: &RequirementCatalog) -> RequirementCatalog {
  let mut ordered = catalog.clone();
  ordered
    .requirements
    .sort_by(|left, right| left.id.cmp(&right.id));
  ordered
    .acceptance_criteria
    .sort_by(|left, right| left.id.cmp(&right.id));
  ordered
    .verification_obligations
    .sort_by(|left, right| left.id.cmp(&right.id));
  ordered
}

/// Selects required obligations and their controller-owned ancestors in canonical order.
pub fn semantic_assessment_catalog(catalog: &RequirementCatalog) -> RequirementCatalog {
  let mut verification_obligations: Vec<_> = catalog
    .verification_obligations
    .iter()
    .filter(|obligation| obligation.required)
    .cloned()
    .collect();
  verification_obligations.sort_by(|left, right| left.id.cmp(&right.id));
  let criterion_ids: BTreeSet<_> = verification_obligations
    .iter()
    .map(|obligation| obligation.criterion_id.clone())
    .collect();
  let mut acceptance_criteria: Vec<_> = catalog
    .acceptance_criteria
    .iter()
    .filter(|criterion| criterion_ids.contains(&criterion.id))
    .cloned()
    .collect();
  acceptance_criteria.sort_by(|left, right| left.id.cmp(&right.id));
  let requirement_ids: BTreeSet<_> = acceptance_criteria
    .iter()
    .map(|criterion| criterion.requirement_id.clone())
    .collect();
  let mut requirements: Vec<_> = catalog
    .requirements
    .iter()
    .filter(|requirement| requirement_ids.contains(&requirement.id))
    .cloned()
    .collect();
  requirements.sort_by(|left, right| left.id.cmp(&right.id));
  let fragment_ids: BTreeSet<_> = requirements
    .iter()
    .flat_map(|requirement| requirement.source_refs.iter())
    .map(|reference| reference.fragment_id.clone())
    .collect();
  RequirementCatalog {
    spec_hash: catalog.spec_hash.clone(),
    requirements,
    acceptance_criteria,
    verification_obligations,
    coverage: tenet_domain::worker::CatalogCoverage {
      normative_fragments: catalog
        .coverage
        .normative_fragments
        .iter()
        .filter(|fragment| fragment_ids.contains(&fragment.id))
        .cloned()
        .collect(),
      uncovered_fragment_ids: Vec::new(),
    },
  }
}

/// Binds an agent proposal to controller-owned catalog identities and relationships.
pub fn materialize_reconciliation(
  catalog: &RequirementCatalog,
  proposal: AgentReconciliationProposal,
) -> Result<ReconcileResult> {
  let ordered = reconciliation_catalog(catalog);
  if proposal.requirements.len() != ordered.requirements.len() {
    bail!(
      "reconciliation returned {} requirement judgments for {} controller-selected requirements",
      proposal.requirements.len(),
      ordered.requirements.len()
    );
  }

  let criterion_requirements: BTreeMap<_, _> = ordered
    .acceptance_criteria
    .iter()
    .map(|criterion| (criterion.id.clone(), criterion.requirement_id.clone()))
    .collect();
  let obligation_criteria: BTreeMap<_, _> = ordered
    .verification_obligations
    .iter()
    .map(|obligation| (obligation.id.clone(), obligation.criterion_id.clone()))
    .collect();

  let requirements = ordered
    .requirements
    .iter()
    .zip(proposal.requirements)
    .map(|(requirement, assessment)| {
      for obligation_id in &assessment.missing_evidence {
        let criterion_id = obligation_criteria
          .get(obligation_id)
          .with_context(|| format!("unknown missing evidence obligation {obligation_id}"))?;
        if criterion_requirements.get(criterion_id) != Some(&requirement.id) {
          bail!(
            "{} reports missing evidence owned by another requirement",
            requirement.id
          );
        }
      }
      let mut observations = assessment.observations;
      observations.sort();
      observations.dedup();
      let mut missing_implementation = assessment.missing_implementation;
      missing_implementation.sort();
      missing_implementation.dedup();
      Ok(RequirementAssessment {
        requirement_id: requirement.id.clone(),
        implementation_state: assessment.implementation_state,
        observations,
        missing_implementation,
        missing_evidence: canonical_obligations(assessment.missing_evidence),
      })
    })
    .collect::<Result<Vec<_>>>()?;

  let mut work_units = proposal
    .work_units
    .into_iter()
    .map(|proposal| {
      let obligation_ids = canonical_obligations(
        proposal
          .verification_obligation_ids
          .into_iter()
          .chain(
            proposal
              .suggested_checks
              .iter()
              .map(|check| check.obligation_id.clone()),
          )
          .collect(),
      );
      let criterion_ids: BTreeSet<_> = obligation_ids
        .iter()
        .map(|obligation_id| {
          obligation_criteria
            .get(obligation_id)
            .cloned()
            .ok_or_else(|| {
              DomainValidationError::UnknownObligation {
                work_unit_id: proposal.id.clone(),
                obligation_id: obligation_id.to_string(),
              }
              .into()
            })
        })
        .collect::<Result<_>>()?;
      let requirement_ids: BTreeSet<_> = criterion_ids
        .iter()
        .map(|criterion_id| {
          criterion_requirements
            .get(criterion_id)
            .cloned()
            .with_context(|| format!("unknown work criterion {criterion_id}"))
        })
        .collect::<Result<_>>()?;

      let mut suggested_checks = proposal.suggested_checks;
      suggested_checks.sort_by(|left, right| {
        (&left.obligation_id, &left.command).cmp(&(&right.obligation_id, &right.command))
      });
      suggested_checks.dedup();

      let mut depends_on = proposal.depends_on;
      depends_on.sort();
      depends_on.dedup();
      let mut scope = proposal.scope;
      scope.paths.sort();
      scope.paths.dedup();
      Ok(WorkUnit {
        id: proposal.id,
        title: proposal.title,
        objective: proposal.objective,
        requirement_ids: requirement_ids.into_iter().collect(),
        criterion_ids: criterion_ids.into_iter().collect(),
        verification_obligation_ids: obligation_ids,
        suggested_checks,
        depends_on,
        scope,
      })
    })
    .collect::<Result<Vec<_>>>()?;
  work_units.sort_by(|left, right| left.id.cmp(&right.id));

  Ok(ReconcileResult {
    summary: proposal.summary,
    requirements,
    work_units,
  })
}
#[derive(Debug)]
enum ReconciliationValidationError {
  PresentWithGaps(RequirementId),
  IncompleteWithoutGap {
    requirement_id: RequirementId,
    state: ImplementationState,
  },
}

impl std::fmt::Display for ReconciliationValidationError {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::PresentWithGaps(requirement_id) => write!(
        formatter,
        "present implementation {requirement_id} cannot report implementation gaps"
      ),
      Self::IncompleteWithoutGap { requirement_id, .. } => write!(
        formatter,
        "incomplete implementation {requirement_id} requires a concrete gap"
      ),
    }
  }
}

impl std::error::Error for ReconciliationValidationError {}

/// Validates semantic reconciliation invariants after controller materialization.
pub fn validate_reconciliation(result: &ReconcileResult) -> Result<()> {
  let mut implementation_gap = false;
  for assessment in &result.requirements {
    if assessment
      .observations
      .iter()
      .chain(&assessment.missing_implementation)
      .any(|value| value.trim().is_empty())
    {
      bail!(
        "{} contains a blank implementation observation",
        assessment.requirement_id
      );
    }
    match assessment.implementation_state {
      ImplementationState::Present if !assessment.missing_implementation.is_empty() => {
        return Err(
          ReconciliationValidationError::PresentWithGaps(assessment.requirement_id.clone()).into(),
        );
      }
      ImplementationState::Partial | ImplementationState::Absent | ImplementationState::Unknown => {
        implementation_gap = true;
        if assessment.missing_implementation.is_empty() {
          return Err(
            ReconciliationValidationError::IncompleteWithoutGap {
              requirement_id: assessment.requirement_id.clone(),
              state: assessment.implementation_state,
            }
            .into(),
          );
        }
      }
      ImplementationState::Present => {}
    }
  }
  if implementation_gap && result.work_units.is_empty() {
    bail!("implementation gaps require at least one proposed work unit");
  }
  Ok(())
}

pub fn reconciliation_retry_feedback(error: &anyhow::Error) -> String {
  if let Some(validation) = error.downcast_ref::<ReconciliationValidationError>() {
    return match validation {
      ReconciliationValidationError::PresentWithGaps(requirement_id) => format!(
        "{requirement_id} is internally inconsistent.\n\nimplementationState=\"present\" means all required implementation exists and missingImplementation must therefore be empty.\n\nIf the reported implementation gaps are real, choose \"partial\", \"absent\", or \"unknown\".\n\nIf no implementation gap actually remains, keep \"present\" and return an empty missingImplementation list.\n\nRegenerate the complete reconciliation result."
      ),
      ReconciliationValidationError::IncompleteWithoutGap {
        requirement_id,
        state,
      } => format!(
        "{requirement_id} is internally inconsistent.\n\nimplementationState=\"{}\" means required implementation is missing, incomplete, or could not be established. missingImplementation must contain at least one concrete gap or explanation.\n\nIf all required implementation exists, choose \"present\" and return an empty missingImplementation list.\n\nRegenerate the complete reconciliation result.",
        implementation_state_name(*state)
      ),
    };
  }
  format!(
    "{error:#}\nRegenerate the complete reconciliation proposal and correct this semantic validation failure."
  )
}

fn implementation_state_name(state: ImplementationState) -> &'static str {
  match state {
    ImplementationState::Present => "present",
    ImplementationState::Partial => "partial",
    ImplementationState::Absent => "absent",
    ImplementationState::Unknown => "unknown",
  }
}

/// Binds ordered agent judgments to the controller-selected required obligations.
pub fn materialize_semantic_assessment(
  catalog: &RequirementCatalog,
  proposal: SemanticAssessmentProposal,
) -> Result<SemanticAssessmentReport> {
  let mut obligation_ids: Vec<_> = catalog
    .verification_obligations
    .iter()
    .filter(|obligation| obligation.required)
    .map(|obligation| obligation.id.clone())
    .collect();
  obligation_ids.sort();
  if proposal.assessments.len() != obligation_ids.len() {
    bail!(
      "semantic assessment must cover every required obligation exactly once (received {} judgments for {} controller-selected obligations)",
      proposal.assessments.len(),
      obligation_ids.len()
    );
  }

  Ok(SemanticAssessmentReport {
    summary: proposal.summary,
    assessments: obligation_ids
      .into_iter()
      .zip(proposal.assessments)
      .map(|(obligation_id, assessment)| ObligationAssessmentResult {
        obligation_id,
        assessment,
      })
      .collect(),
  })
}

fn canonical_obligations(values: Vec<ObligationId>) -> Vec<ObligationId> {
  values
    .into_iter()
    .collect::<BTreeSet<_>>()
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
  use chrono::{TimeZone, Utc};
  use tenet_domain::{
    evidence::{
      AcceptanceCriterion, ImplementationState, ObligationAssessment, SemanticAssessmentProposal,
      VerificationObligation,
    },
    ids::{CriterionId, ObligationId, RequirementId},
    model::{
      AgentReconciliationProposal, AgentRequirementAssessment, AgentWorkUnit, CandidateCheck,
      DeferredCandidate, Requirement, RequirementCatalog, RunStatus, State, VerificationReport,
      WorkExecution, WorkLease, WorkScope, WorkStatus, WorkerSummary,
    },
    worker::CatalogCoverage,
  };

  use super::{
    advance_stagnation, apply_completion_decision, apply_spec_invalidation, authorize_frontier,
    classify_deferred_candidates, decide_execution, decide_reconciliation,
    materialize_reconciliation, materialize_semantic_assessment, plan_spec_invalidation,
    NextAction,
  };

  fn catalog() -> RequirementCatalog {
    RequirementCatalog {
      spec_hash: "spec-v1".into(),
      requirements: vec![Requirement {
        id: RequirementId::from("REQ-001"),
        title: "Requirement".into(),
        description: "Required behavior".into(),
        required: true,
        source_refs: Vec::new(),
      }],
      acceptance_criteria: vec![AcceptanceCriterion {
        id: CriterionId::from("REQ-001/AC-01"),
        requirement_id: RequirementId::from("REQ-001"),
        description: "Observable behavior".into(),
        mandatory: true,
      }],
      verification_obligations: vec![VerificationObligation {
        id: ObligationId::from("REQ-001/AC-01/VO-01"),
        criterion_id: CriterionId::from("REQ-001/AC-01"),
        description: "Verify behavior".into(),
        required: true,
      }],
      coverage: CatalogCoverage {
        normative_fragments: Vec::new(),
        uncovered_fragment_ids: Vec::new(),
      },
    }
  }

  fn proposal() -> AgentReconciliationProposal {
    AgentReconciliationProposal {
      summary: "Work remains".into(),
      requirements: vec![AgentRequirementAssessment {
        implementation_state: ImplementationState::Partial,
        observations: vec!["Existing implementation is incomplete".into()],
        missing_implementation: vec!["Required branch is absent".into()],
        missing_evidence: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
      }],
      work_units: vec![AgentWorkUnit {
        id: "WU-001".into(),
        title: "Implement behavior".into(),
        objective: "Add the required branch".into(),
        verification_obligation_ids: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
        suggested_checks: vec![CandidateCheck {
          obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
          command: "cargo test behavior".into(),
        }],
        depends_on: Vec::new(),
        scope: WorkScope {
          paths: vec!["src/**".into()],
        },
      }],
    }
  }

  #[test]
  fn materialization_derives_catalog_relationships() {
    let result = materialize_reconciliation(&catalog(), proposal()).expect("materialize proposal");

    assert_eq!(
      result.work_units[0].requirement_ids,
      vec![RequirementId::from("REQ-001")]
    );
    assert_eq!(
      result.work_units[0].criterion_ids,
      vec![CriterionId::from("REQ-001/AC-01")]
    );
  }

  #[test]
  fn materialization_is_canonical_for_repeated_proposals() {
    let first = materialize_reconciliation(&catalog(), proposal()).expect("first materialization");
    let second =
      materialize_reconciliation(&catalog(), proposal()).expect("second materialization");

    assert_eq!(first, second);
  }

  #[test]
  fn semantic_materialization_binds_controller_selected_obligation() {
    let report = materialize_semantic_assessment(
      &catalog(),
      SemanticAssessmentProposal {
        summary: "Satisfied".into(),
        assessments: vec![ObligationAssessment::Satisfied {
          rationale: "Verified by inspection".into(),
          evidence_refs: vec!["src/lib.rs:1".into()],
        }],
      },
    )
    .expect("materialize assessment");

    assert_eq!(
      report.assessments[0].obligation_id,
      ObligationId::from("REQ-001/AC-01/VO-01")
    );
  }

  fn deferred_candidate(catalog_hash: &str) -> DeferredCandidate {
    let work_unit = materialize_reconciliation(&catalog(), proposal())
      .expect("materialize candidate unit")
      .work_units
      .remove(0);
    DeferredCandidate {
      lease: WorkLease {
        id: "lease-1".into(),
        worker_id: "worker-1".into(),
        work_unit,
        base_revision: "rev-1".into(),
        workspace: "workspace".into(),
        issued_at: "2026-01-01T00:00:00Z".into(),
      },
      worker_summary: WorkerSummary {
        summary: "candidate".into(),
        changed_files: vec!["src/lib.rs".into()],
        tests_run: Vec::new(),
        notes: Vec::new(),
        decisions: Vec::new(),
        discoveries: Vec::new(),
        risks: Vec::new(),
        follow_ups: Vec::new(),
      },
      base_revision: "rev-1".into(),
      candidate_revision: "candidate-1".into(),
      changed_paths: vec!["src/lib.rs".into()],
      discoveries: Vec::new(),
      catalog_hash: catalog_hash.into(),
      git_ref: "refs/tenet/candidate-1".into(),
    }
  }

  fn execution(id: &str) -> WorkExecution {
    let candidate = deferred_candidate("spec-v1");
    let mut lease = candidate.lease;
    lease.work_unit.id = id.into();
    WorkExecution {
      lease,
      worker_summary: candidate.worker_summary,
      verification: VerificationReport {
        passed: true,
        started_at: Utc.timestamp_opt(0, 0).single().expect("timestamp"),
        finished_at: Utc.timestamp_opt(1, 0).single().expect("timestamp"),
        commands: Vec::new(),
        executions: Vec::new(),
        warnings: Vec::new(),
      },
      base_revision: "rev-1".into(),
      candidate_revision: format!("candidate-{id}"),
      changed_paths: vec![format!("{id}.rs")],
      discoveries: Vec::new(),
    }
  }

  #[test]
  fn execution_completion_order_does_not_change_integration_plan() {
    let left = decide_execution(
      RunStatus::Running,
      vec![execution("WU-002"), execution("WU-001")],
    );
    let right = decide_execution(
      RunStatus::Running,
      vec![execution("WU-001"), execution("WU-002")],
    );

    assert_eq!(left, right);
  }

  #[test]
  fn completion_blocker_never_transitions_run_to_done() {
    let mut state = State::fresh();
    state.status = RunStatus::Running;

    let action = apply_completion_decision(
      &mut state,
      &crate::completion::CompletionDecision::NotReady(vec![
        crate::completion::CompletionBlocker::RepositoryDirty,
      ]),
    );

    assert!(matches!(action, NextAction::Block(_)));
    assert_ne!(state.status, RunStatus::Done);
  }

  #[test]
  fn stale_deferred_candidate_is_not_current_authority() {
    let classification =
      classify_deferred_candidates(vec![deferred_candidate("spec-v0")], "spec-v1", "rev-1");

    assert!(classification.retained.is_empty());
    assert_eq!(classification.stale_refs, vec!["refs/tenet/candidate-1"]);
  }

  #[test]
  fn specification_invalidation_clears_derived_run_authority() {
    let mut state = State::fresh();
    state.deferred_candidates = vec![deferred_candidate("spec-v0")];
    state
      .work_statuses
      .insert("WU-001".into(), WorkStatus::Completed);
    state.verification_layers.completion_eligible = true;
    state.progress_fingerprint = Some("old-progress".into());
    state.stagnation_count = 3;

    let refs = plan_spec_invalidation(&state).expect("plan invalidation");
    assert_eq!(state.deferred_candidates.len(), 1);
    apply_spec_invalidation(&mut state);

    assert_eq!(refs, vec!["refs/tenet/candidate-1"]);
    assert!(state.deferred_candidates.is_empty());
    assert_eq!(state.work_statuses["WU-001"], WorkStatus::Invalidated);
    assert!(!state.verification_layers.completion_eligible);
    assert!(state.progress_fingerprint.is_none());
    assert_eq!(state.stagnation_count, 0);
  }

  #[test]
  fn terminal_runs_never_authorize_reconciliation_work() {
    for status in [
      RunStatus::Done,
      RunStatus::Blocked,
      RunStatus::Failed,
      RunStatus::Stopped,
    ] {
      let action = decide_reconciliation(status, true, Vec::new(), None);
      assert_eq!(action, NextAction::Finish);
    }
  }

  #[test]
  fn reconciliation_without_work_requires_project_verification() {
    let action = decide_reconciliation(RunStatus::Running, false, Vec::new(), None);

    assert_eq!(action, NextAction::VerifyProject);
  }

  #[test]
  fn repeated_fingerprint_blocks_before_authorizing_work() {
    let unit = materialize_reconciliation(&catalog(), proposal())
      .expect("materialize")
      .work_units
      .remove(0);
    let action = decide_reconciliation(RunStatus::Running, true, vec![unit], Some(1));

    assert!(matches!(action, NextAction::Block(_)));
  }

  #[test]
  fn reconciliation_frontier_is_authorized_in_canonical_order() {
    let mut second = materialize_reconciliation(&catalog(), proposal())
      .expect("materialize second")
      .work_units
      .remove(0);
    second.id = "WU-002".into();
    let mut first = second.clone();
    first.id = "WU-001".into();

    let action = decide_reconciliation(RunStatus::Running, true, vec![second, first], None);
    let NextAction::ExecuteFrontier(units) = action else {
      panic!("expected execution action");
    };

    assert_eq!(
      units.into_iter().map(|unit| unit.id).collect::<Vec<_>>(),
      vec!["WU-001", "WU-002"]
    );
  }

  #[test]
  fn materialization_ignores_agent_work_unit_order() {
    let mut left = proposal();
    let mut second = left.work_units[0].clone();
    second.id = "WU-002".into();
    left.work_units.push(second);
    let mut right = left.clone();
    right.work_units.reverse();

    let left = materialize_reconciliation(&catalog(), left).expect("left materialization");
    let right = materialize_reconciliation(&catalog(), right).expect("right materialization");

    assert_eq!(left, right);
  }

  #[test]
  fn repeated_progress_fingerprint_blocks_at_configured_bound() {
    let first = advance_stagnation(None, 0, "same".into(), 2);
    let second = advance_stagnation(Some(&first.fingerprint), first.count, "same".into(), 2);
    let third = advance_stagnation(Some(&second.fingerprint), second.count, "same".into(), 2);

    assert!(!first.blocked);
    assert!(!second.blocked);
    assert!(third.blocked);
  }

  #[test]
  fn no_longer_authorized_candidate_cannot_enter_execution_plan() {
    let mut candidate = deferred_candidate("spec-v1");
    candidate.changed_paths = vec!["outside-authority.txt".into()];
    let unit = candidate.lease.work_unit.clone();

    let authorization = authorize_frontier(vec![unit], &[candidate], "spec-v1", "rev-1")
      .expect("evaluate candidate authority");

    assert!(authorization.work_units.is_empty());
    assert!(authorization.deferred_candidates.is_empty());
  }

  #[test]
  fn progress_telemetry_does_not_change_reconciliation_decision() {
    let mut implementing = State::fresh();
    implementing.status = RunStatus::Running;
    implementing.phase = tenet_domain::model::Phase::Implementing;
    implementing.last_summary = "Implementing A".into();
    let mut repairing = implementing.clone();
    repairing.phase = tenet_domain::model::Phase::Repairing;
    repairing.last_summary = "Repairing B".into();
    let unit = materialize_reconciliation(&catalog(), proposal())
      .expect("materialize")
      .work_units
      .remove(0);

    let left = decide_reconciliation(implementing.status, true, vec![unit.clone()], None);
    let right = decide_reconciliation(repairing.status, true, vec![unit], None);

    assert_eq!(left, right);
  }
}
