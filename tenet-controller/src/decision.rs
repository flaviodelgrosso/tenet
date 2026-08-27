use std::{
  collections::{BTreeMap, BTreeSet},
  fmt::Write as _,
};

use anyhow::{bail, Context, Result};
use serde::Serialize;
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
  worker::normalize_scope_pattern,
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
    RunStatus::ReviewRequired
      | RunStatus::Done
      | RunStatus::Blocked
      | RunStatus::Failed
      | RunStatus::Stopped
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

/// Hashes only canonical controller-relevant semantic reconciliation facts.
pub fn progress_fingerprint(
  catalog_hash: &str,
  reconciliation: &ReconcileResult,
) -> Result<String> {
  let projection = ProgressProjection::from_reconciliation(catalog_hash, reconciliation)?;
  let bytes = serde_json::to_vec(&projection)?;
  let digest = Sha256::digest(bytes);
  let mut fingerprint = String::with_capacity(digest.len() * 2);
  for byte in digest {
    write!(fingerprint, "{byte:02x}")?;
  }
  Ok(fingerprint)
}

#[derive(Serialize)]
struct ProgressProjection<'a> {
  catalog_hash: &'a str,
  requirements: Vec<RequirementProgress>,
  work_units: Vec<WorkUnitProgress>,
}

#[derive(Serialize)]
struct RequirementProgress {
  requirement_id: RequirementId,
  implementation_state: ImplementationState,
  missing_evidence: Vec<ObligationId>,
}

#[derive(Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct SemanticWorkSignature {
  requirement_ids: Vec<RequirementId>,
  criterion_ids: Vec<tenet_domain::ids::CriterionId>,
  verification_obligation_ids: Vec<ObligationId>,
  scope_paths: Vec<String>,
}

#[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct WorkUnitProgress {
  target: SemanticWorkSignature,
  dependencies: Vec<SemanticWorkSignature>,
}

impl<'a> ProgressProjection<'a> {
  fn from_reconciliation(catalog_hash: &'a str, reconciliation: &ReconcileResult) -> Result<Self> {
    let mut requirements: Vec<_> = reconciliation
      .requirements
      .iter()
      .map(|assessment| RequirementProgress {
        requirement_id: assessment.requirement_id.clone(),
        implementation_state: assessment.implementation_state,
        missing_evidence: canonical_obligations(assessment.missing_evidence.clone()),
      })
      .collect();
    requirements.sort_by(|left, right| left.requirement_id.cmp(&right.requirement_id));

    let targets: BTreeMap<_, _> = reconciliation
      .work_units
      .iter()
      .map(|unit| (unit.id.as_str(), semantic_work_signature(unit)))
      .collect();
    let mut work_units = Vec::with_capacity(reconciliation.work_units.len());
    for unit in &reconciliation.work_units {
      let mut dependencies = unit
        .depends_on
        .iter()
        .map(|dependency| {
          targets.get(dependency.as_str()).cloned().with_context(|| {
            format!(
              "work unit {} depends on unknown work unit {dependency}",
              unit.id
            )
          })
        })
        .collect::<Result<Vec<_>>>()?;
      dependencies.sort();
      dependencies.dedup();
      work_units.push(WorkUnitProgress {
        target: semantic_work_signature(unit),
        dependencies,
      });
    }
    work_units.sort();
    work_units.dedup();

    Ok(Self {
      catalog_hash,
      requirements,
      work_units,
    })
  }
}

fn semantic_work_signature(unit: &WorkUnit) -> SemanticWorkSignature {
  SemanticWorkSignature {
    requirement_ids: canonical_requirements(unit.requirement_ids.clone()),
    criterion_ids: canonical_criteria(unit.criterion_ids.clone()),
    verification_obligation_ids: canonical_obligations(unit.verification_obligation_ids.clone()),
    scope_paths: canonical_scope_paths(&unit.scope.paths),
  }
}

fn canonical_requirements(mut values: Vec<RequirementId>) -> Vec<RequirementId> {
  values.sort();
  values.dedup();
  values
}

fn canonical_criteria(
  mut values: Vec<tenet_domain::ids::CriterionId>,
) -> Vec<tenet_domain::ids::CriterionId> {
  values.sort();
  values.dedup();
  values
}

fn canonical_scope_paths(paths: &[String]) -> Vec<String> {
  let mut paths: Vec<_> = paths
    .iter()
    .map(|path| normalize_scope_pattern(path))
    .collect();
  paths.sort();
  paths.dedup();
  paths
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
  if candidates
    .windows(2)
    .any(|pair| pair[0].git_ref == pair[1].git_ref)
  {
    bail!("duplicate deferred candidate Git ref");
  }
  let mut selected_refs = BTreeSet::new();
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
      if selected_refs.contains(&candidate.git_ref) {
        continue;
      }
      if tenet_runtime::scheduler::deferred_candidate_authorized(candidate, &unit)? {
        selected_refs.insert(candidate.git_ref.clone());
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

/// Returns stable, controller-generated handles for reconciliation judgments.
pub fn reconciliation_handles(catalog: &RequirementCatalog) -> BTreeMap<RequirementId, String> {
  reconciliation_catalog(catalog)
    .requirements
    .into_iter()
    .enumerate()
    .map(|(index, requirement)| (requirement.id, format!("R{:03}", index + 1)))
    .collect()
}

/// Returns stable, controller-generated handles for required semantic judgments.
pub fn semantic_assessment_handles(catalog: &RequirementCatalog) -> BTreeMap<ObligationId, String> {
  semantic_assessment_catalog(catalog)
    .verification_obligations
    .into_iter()
    .enumerate()
    .map(|(index, obligation)| (obligation.id, format!("O{:03}", index + 1)))
    .collect()
}

/// Binds an agent proposal to controller-owned catalog identities and relationships.
pub fn materialize_reconciliation(
  catalog: &RequirementCatalog,
  proposal: AgentReconciliationProposal,
) -> Result<ReconcileResult> {
  let ordered = reconciliation_catalog(catalog);
  let handles = reconciliation_handles(&ordered);
  let expected: BTreeMap<_, _> = handles
    .iter()
    .map(|(requirement_id, handle)| (handle.clone(), requirement_id.clone()))
    .collect();
  let assessments = bind_judgments(
    proposal.requirements,
    &expected,
    |assessment| &assessment.requirement_handle,
    "reconciliation requirement",
  )?;

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
    .map(|requirement| {
      let handle = handles
        .get(&requirement.id)
        .expect("every controller-selected requirement has a handle");
      let assessment = assessments
        .get(handle)
        .expect("every controller-generated handle is bound")
        .clone();
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

/// Binds agent semantic judgments to controller-selected required obligations by handle.
pub fn materialize_semantic_assessment(
  catalog: &RequirementCatalog,
  proposal: SemanticAssessmentProposal,
) -> Result<SemanticAssessmentReport> {
  let handles = semantic_assessment_handles(catalog);
  let expected: BTreeMap<_, _> = handles
    .iter()
    .map(|(obligation_id, handle)| (handle.clone(), obligation_id.clone()))
    .collect();
  let assessments = bind_judgments(
    proposal.assessments,
    &expected,
    |assessment| &assessment.obligation_handle,
    "semantic obligation",
  )?;

  Ok(SemanticAssessmentReport {
    summary: proposal.summary,
    assessments: expected
      .into_iter()
      .map(|(handle, obligation_id)| ObligationAssessmentResult {
        obligation_id,
        assessment: assessments
          .get(&handle)
          .expect("every controller-generated handle is bound")
          .judgment
          .clone(),
      })
      .collect(),
  })
}

fn bind_judgments<T, Handle>(
  judgments: Vec<T>,
  expected: &BTreeMap<String, Handle>,
  handle: impl Fn(&T) -> &str,
  kind: &str,
) -> Result<BTreeMap<String, T>> {
  let mut bound = BTreeMap::new();
  for judgment in judgments {
    let handle = handle(&judgment).to_owned();
    if bound.insert(handle.clone(), judgment).is_some() {
      bail!("duplicate {kind} handle {handle}");
    }
    if !expected.contains_key(&handle) {
      bail!("unknown {kind} handle {handle}");
    }
  }
  for handle in expected.keys() {
    if !bound.contains_key(handle) {
      bail!("missing {kind} handle {handle}");
    }
  }
  Ok(bound)
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
      AcceptanceCriterion, AgentObligationAssessment, ImplementationState,
      SemanticAssessmentProposal, VerificationObligation,
    },
    ids::{CriterionId, ObligationId, RequirementId},
    model::{
      AgentReconciliationProposal, AgentRequirementAssessment, AgentWorkUnit, CandidateCheck,
      DeferredCandidate, Requirement, RequirementCatalog, RunStatus, State, VerificationReport,
      WorkExecution, WorkLease, WorkScope, WorkStatus, WorkerSummary,
    },
    proof::{AssessmentJudgment, GapKind},
    worker::CatalogCoverage,
  };

  use super::{
    advance_stagnation, apply_completion_decision, apply_spec_invalidation, authorize_frontier,
    classify_deferred_candidates, decide_execution, decide_reconciliation,
    materialize_reconciliation, materialize_semantic_assessment, plan_spec_invalidation,
    progress_fingerprint, NextAction,
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
        evidence_contract: Default::default(),
      }],
      coverage: CatalogCoverage {
        normative_fragments: Vec::new(),
        uncovered_fragment_ids: Vec::new(),
      },
    }
  }

  fn two_requirement_catalog() -> RequirementCatalog {
    let mut catalog = catalog();
    catalog.requirements.push(Requirement {
      id: RequirementId::from("REQ-002"),
      title: "Second requirement".into(),
      description: "Second required behavior".into(),
      required: true,
      source_refs: Vec::new(),
    });
    catalog.acceptance_criteria.push(AcceptanceCriterion {
      id: CriterionId::from("REQ-002/AC-01"),
      requirement_id: RequirementId::from("REQ-002"),
      description: "Second observable behavior".into(),
      mandatory: true,
    });
    catalog
      .verification_obligations
      .push(VerificationObligation {
        id: ObligationId::from("REQ-002/AC-01/VO-01"),
        criterion_id: CriterionId::from("REQ-002/AC-01"),
        description: "Verify second behavior".into(),
        required: true,
        evidence_contract: Default::default(),
      });
    catalog
  }

  fn proposal() -> AgentReconciliationProposal {
    AgentReconciliationProposal {
      summary: "Work remains".into(),
      requirements: vec![AgentRequirementAssessment {
        requirement_handle: "R001".into(),
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
        assessments: vec![AgentObligationAssessment {
          obligation_handle: "O001".into(),
          judgment: AssessmentJudgment::Supported {
            artifact_ids: Vec::new(),
            rationale: "Verified by inspection".into(),
          },
        }],
      },
    )
    .expect("materialize assessment");

    assert_eq!(
      report.assessments[0].obligation_id,
      ObligationId::from("REQ-001/AC-01/VO-01")
    );
  }

  #[test]
  fn reordered_judgments_bind_to_their_controller_handles() {
    let catalog = two_requirement_catalog();
    let mut reconciliation = proposal();
    reconciliation
      .requirements
      .push(AgentRequirementAssessment {
        requirement_handle: "R002".into(),
        implementation_state: ImplementationState::Present,
        observations: vec!["Second implementation is present".into()],
        missing_implementation: Vec::new(),
        missing_evidence: vec![ObligationId::from("REQ-002/AC-01/VO-01")],
      });
    reconciliation.requirements.reverse();

    let result = materialize_reconciliation(&catalog, reconciliation).expect("rebind by handle");

    assert_eq!(
      result.requirements[0].requirement_id,
      RequirementId::from("REQ-001")
    );
    assert_eq!(
      result.requirements[0].implementation_state,
      ImplementationState::Partial
    );
    assert_eq!(
      result.requirements[1].requirement_id,
      RequirementId::from("REQ-002")
    );
    assert_eq!(
      result.requirements[1].implementation_state,
      ImplementationState::Present
    );

    let report = materialize_semantic_assessment(
      &catalog,
      SemanticAssessmentProposal {
        summary: "Judgments are deliberately reversed".into(),
        assessments: vec![
          AgentObligationAssessment {
            obligation_handle: "O002".into(),
            judgment: AssessmentJudgment::Insufficient {
              reason: "Second obligation has a gap".into(),
              proposals: Vec::new(),
              gap_kind: GapKind::Implementation,
            },
          },
          AgentObligationAssessment {
            obligation_handle: "O001".into(),
            judgment: AssessmentJudgment::Supported {
              artifact_ids: Vec::new(),
              rationale: "First obligation supported".into(),
            },
          },
        ],
      },
    )
    .expect("rebind semantic judgments by handle");

    assert_eq!(
      report.assessments[0].obligation_id,
      ObligationId::from("REQ-001/AC-01/VO-01")
    );
    assert_eq!(
      report.assessments[1].obligation_id,
      ObligationId::from("REQ-002/AC-01/VO-01")
    );
    assert!(matches!(
      report.assessments[0].assessment,
      AssessmentJudgment::Supported { .. }
    ));
    assert!(matches!(
      report.assessments[1].assessment,
      AssessmentJudgment::Insufficient { .. }
    ));
  }

  #[test]
  fn judgment_handles_reject_missing_duplicate_and_unknown_values() {
    let mut missing = proposal();
    missing.requirements.clear();
    assert!(materialize_reconciliation(&catalog(), missing)
      .expect_err("missing handle rejected")
      .to_string()
      .contains("missing reconciliation requirement handle R001"));

    let mut duplicate = proposal();
    duplicate
      .requirements
      .push(duplicate.requirements[0].clone());
    assert!(materialize_reconciliation(&catalog(), duplicate)
      .expect_err("duplicate handle rejected")
      .to_string()
      .contains("duplicate reconciliation requirement handle R001"));

    let mut unknown = proposal();
    unknown.requirements[0].requirement_handle = "R999".into();
    assert!(materialize_reconciliation(&catalog(), unknown)
      .expect_err("unknown handle rejected")
      .to_string()
      .contains("unknown reconciliation requirement handle R999"));

    let unknown = materialize_semantic_assessment(
      &catalog(),
      SemanticAssessmentProposal {
        summary: "Unknown handle".into(),
        assessments: vec![AgentObligationAssessment {
          obligation_handle: "O999".into(),
          judgment: AssessmentJudgment::Insufficient {
            reason: "Cannot establish obligation".into(),
            proposals: Vec::new(),
            gap_kind: GapKind::Evidence,
          },
        }],
      },
    )
    .expect_err("unknown semantic handle rejected");
    assert!(unknown
      .to_string()
      .contains("unknown semantic obligation handle O999"));

    let missing = materialize_semantic_assessment(
      &catalog(),
      SemanticAssessmentProposal {
        summary: "Missing handle".into(),
        assessments: Vec::new(),
      },
    )
    .expect_err("missing semantic handle rejected");
    assert!(missing
      .to_string()
      .contains("missing semantic obligation handle O001"));

    let duplicate = AgentObligationAssessment {
      obligation_handle: "O001".into(),
      judgment: AssessmentJudgment::Contradicted {
        artifact_ids: Vec::new(),
        rationale: "Cannot establish obligation".into(),
        proposals: Vec::new(),
      },
    };
    let duplicate = materialize_semantic_assessment(
      &catalog(),
      SemanticAssessmentProposal {
        summary: "Duplicate handle".into(),
        assessments: vec![duplicate.clone(), duplicate],
      },
    )
    .expect_err("duplicate semantic handle rejected");
    assert!(duplicate
      .to_string()
      .contains("duplicate semantic obligation handle O001"));
  }

  #[test]
  fn progress_fingerprint_ignores_agent_prose_order_and_duplicate_semantic_work() {
    let left = materialize_reconciliation(&catalog(), proposal()).expect("materialize left");
    let mut right = left.clone();
    right.summary = "Different model summary".into();
    right.requirements[0].observations = vec!["Different observation prose".into()];
    right.requirements[0].missing_implementation = vec!["Different gap prose".into()];
    right.work_units[0].title = "Different work title".into();
    right.work_units[0].objective = "Different work objective".into();
    right.work_units[0].suggested_checks[0].command = "different advisory command".into();
    let mut duplicate = right.work_units[0].clone();
    duplicate.id = "WU-duplicate".into();
    right.work_units.push(duplicate);
    right.work_units.reverse();

    assert_eq!(
      progress_fingerprint("catalog", &left).expect("left fingerprint"),
      progress_fingerprint("catalog", &right).expect("right fingerprint")
    );
  }

  #[test]
  fn progress_fingerprint_is_deterministic_for_canonical_scope_patterns() {
    let mut left = materialize_reconciliation(&catalog(), proposal()).expect("materialize left");
    left.work_units[0].scope.paths = vec!["tests/**".into(), "src/auth/**".into()];
    let mut right = left.clone();
    right.work_units[0].scope.paths = vec![
      "src/auth/**".into(),
      "tests/**".into(),
      "src/auth/**".into(),
    ];

    assert_eq!(
      progress_fingerprint("catalog", &left).expect("left fingerprint"),
      progress_fingerprint("catalog", &right).expect("right fingerprint")
    );
  }

  #[test]
  fn progress_fingerprint_ignores_generated_work_ids_and_dependency_ids() {
    let mut left = materialize_reconciliation(&catalog(), proposal()).expect("materialize left");
    let mut dependent = left.work_units[0].clone();
    dependent.id = "WU-002".into();
    dependent.scope.paths = vec!["tests/**".into()];
    dependent.depends_on = vec!["WU-001".into()];
    left.work_units.push(dependent);
    let mut right = left.clone();
    right.work_units[0].id = "generated-root".into();
    right.work_units[1].id = "generated-dependent".into();
    right.work_units[1].depends_on = vec!["generated-root".into()];

    assert_eq!(
      progress_fingerprint("catalog", &left).expect("left fingerprint"),
      progress_fingerprint("catalog", &right).expect("right fingerprint")
    );
  }

  #[test]
  fn progress_fingerprint_changes_with_semantic_dependency_topology() {
    let mut dependent =
      materialize_reconciliation(&catalog(), proposal()).expect("materialize dependent graph");
    let mut second = dependent.work_units[0].clone();
    second.id = "WU-002".into();
    second.scope.paths = vec!["tests/**".into()];
    second.depends_on = vec!["WU-001".into()];
    dependent.work_units.push(second);
    let mut independent = dependent.clone();
    independent.work_units[1].depends_on.clear();

    assert_ne!(
      progress_fingerprint("catalog", &dependent).expect("dependent fingerprint"),
      progress_fingerprint("catalog", &independent).expect("independent fingerprint")
    );
  }

  #[test]
  fn progress_fingerprint_changes_with_implementation_state() {
    let left = materialize_reconciliation(&catalog(), proposal()).expect("materialize left");
    let mut right = left.clone();
    right.requirements[0].implementation_state = ImplementationState::Absent;

    assert_ne!(
      progress_fingerprint("catalog", &left).expect("left fingerprint"),
      progress_fingerprint("catalog", &right).expect("right fingerprint")
    );
  }

  #[test]
  fn progress_fingerprint_changes_with_missing_evidence() {
    let left = materialize_reconciliation(&catalog(), proposal()).expect("materialize left");
    let mut right = left.clone();
    right.requirements[0].missing_evidence.clear();

    assert_ne!(
      progress_fingerprint("catalog", &left).expect("left fingerprint"),
      progress_fingerprint("catalog", &right).expect("right fingerprint")
    );
  }

  #[test]
  fn progress_fingerprint_changes_with_authoritative_catalog() {
    let reconciliation =
      materialize_reconciliation(&catalog(), proposal()).expect("materialize reconciliation");

    assert_ne!(
      progress_fingerprint("catalog-a", &reconciliation).expect("first fingerprint"),
      progress_fingerprint("catalog-b", &reconciliation).expect("second fingerprint")
    );
  }

  #[test]
  fn progress_fingerprint_changes_with_obligation_targets() {
    let left = materialize_reconciliation(&catalog(), proposal()).expect("materialize left");
    let mut right = left.clone();
    right.work_units[0].verification_obligation_ids.clear();

    assert_ne!(
      progress_fingerprint("catalog", &left).expect("left fingerprint"),
      progress_fingerprint("catalog", &right).expect("right fingerprint")
    );
  }

  #[test]
  fn progress_fingerprint_changes_with_material_scope() {
    let left = materialize_reconciliation(&catalog(), proposal()).expect("materialize left");
    let mut right = left.clone();
    right.work_units[0].scope.paths = vec!["tests/**".into()];

    assert_ne!(
      progress_fingerprint("catalog", &left).expect("left fingerprint"),
      progress_fingerprint("catalog", &right).expect("right fingerprint")
    );
  }

  #[test]
  fn progress_fingerprint_normalizes_equivalent_scope_paths() {
    let left = materialize_reconciliation(&catalog(), proposal()).expect("materialize left");
    let mut right = left.clone();
    right.work_units[0].scope.paths = vec!["./src//**".into()];

    assert_eq!(
      progress_fingerprint("catalog", &left).expect("left fingerprint"),
      progress_fingerprint("catalog", &right).expect("right fingerprint")
    );
  }

  #[test]
  fn progress_fingerprint_preserves_glob_escape_semantics() {
    let mut left = materialize_reconciliation(&catalog(), proposal()).expect("materialize left");
    left.work_units[0].scope.paths = vec!["src/foo/**".into()];
    let mut right = left.clone();
    right.work_units[0].scope.paths = vec!["src\\foo/**".into()];

    assert_ne!(
      progress_fingerprint("catalog", &left).expect("left fingerprint"),
      progress_fingerprint("catalog", &right).expect("right fingerprint")
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
      repair_attempts: 0,
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
      RunStatus::ReviewRequired,
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
  fn deferred_candidate_authorizes_at_most_one_lease_per_round() {
    let candidate = deferred_candidate("spec-v1");
    let mut first = candidate.lease.work_unit.clone();
    first.id = "A".into();
    let mut second = first.clone();
    second.id = "B".into();

    let authorization = authorize_frontier(vec![second, first], &[candidate], "spec-v1", "rev-1")
      .expect("authorize one lease");
    let selected_refs: Vec<_> = authorization
      .deferred_candidates
      .iter()
      .filter_map(|candidate| {
        candidate
          .as_ref()
          .map(|candidate| candidate.git_ref.as_str())
      })
      .collect();

    assert_eq!(
      authorization
        .work_units
        .iter()
        .map(|unit| unit.id.as_str())
        .collect::<Vec<_>>(),
      ["A"]
    );
    assert_eq!(selected_refs, ["refs/tenet/candidate-1"]);
  }

  #[test]
  fn deferred_candidate_assignment_is_independent_of_input_order() {
    let first_candidate = deferred_candidate("spec-v1");
    let mut second_candidate = first_candidate.clone();
    second_candidate.git_ref = "refs/tenet/candidate-2".into();
    let mut first = first_candidate.lease.work_unit.clone();
    first.id = "A".into();
    let mut second = first.clone();
    second.id = "B".into();

    let forward = authorize_frontier(
      vec![first.clone(), second.clone()],
      &[first_candidate.clone(), second_candidate.clone()],
      "spec-v1",
      "rev-1",
    )
    .expect("forward authorization");
    let reversed = authorize_frontier(
      vec![second, first],
      &[second_candidate, first_candidate],
      "spec-v1",
      "rev-1",
    )
    .expect("reversed authorization");

    assert_eq!(forward, reversed);
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
