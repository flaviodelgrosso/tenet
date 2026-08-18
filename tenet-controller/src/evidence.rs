use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::Utc;

use tenet_domain::{
  events::{EventSink, RunEvent},
  evidence::{
    EvidenceGraphState, EvidencePolicy, EvidenceProjection, EvidenceResult, EvidenceValidity,
    SemanticAssessmentReport, VerificationState,
  },
  ids::{CriterionId, EvidenceId, RequirementId},
  model::RequirementCatalog,
  verification::ProjectVerificationRun,
};

use tenet_runtime::store;

pub fn graph_from_catalog(catalog: &RequirementCatalog) -> Result<EvidenceGraphState> {
  let mut graph = EvidenceGraphState::new(&catalog.spec_hash);
  for requirement in &catalog.requirements {
    graph.register_requirement(requirement.id.clone(), requirement.required);
  }
  for criterion in &catalog.acceptance_criteria {
    graph
      .add_criterion(criterion.clone())
      .context("register acceptance criterion")?;
  }
  for obligation in &catalog.verification_obligations {
    graph
      .add_obligation(obligation.clone())
      .context("register verification obligation")?;
  }
  Ok(graph)
}

pub async fn load(
  cwd: &std::path::Path,
  catalog: &RequirementCatalog,
) -> Result<EvidenceGraphState> {
  let expected = graph_from_catalog(catalog)?;
  store::read_evidence_graph(cwd, &expected).await
}

pub fn verification_states(
  graph: &EvidenceGraphState,
  policy: EvidencePolicy<'_>,
) -> Result<BTreeMap<RequirementId, VerificationState>> {
  graph
    .requirements
    .iter()
    .map(|requirement_id| {
      graph
        .requirement_verification_state(requirement_id, policy)
        .map(|state| (requirement_id.clone(), state))
        .map_err(anyhow::Error::new)
    })
    .collect()
}

pub fn criterion_states(
  graph: &EvidenceGraphState,
  policy: EvidencePolicy<'_>,
) -> Result<BTreeMap<CriterionId, VerificationState>> {
  graph
    .criteria
    .keys()
    .map(|criterion_id| {
      graph
        .criterion_verification_state(criterion_id, policy)
        .map(|state| (criterion_id.clone(), state))
        .map_err(anyhow::Error::new)
    })
    .collect()
}

pub fn projections(
  graph: &EvidenceGraphState,
  policy: EvidencePolicy<'_>,
) -> Result<Vec<EvidenceProjection>> {
  graph
    .requirements
    .iter()
    .map(|requirement_id| {
      graph
        .projection(requirement_id, policy)
        .map_err(anyhow::Error::new)
    })
    .collect()
}

pub async fn record_project_verification(
  cwd: &std::path::Path,
  graph: &mut EvidenceGraphState,
  report: &ProjectVerificationRun,
) -> Result<()> {
  graph.record_project_verification(report);
  store::write_evidence_graph(cwd, graph).await
}

pub async fn establish_semantic_assessment(
  cwd: &std::path::Path,
  events: &EventSink,
  graph: &mut EvidenceGraphState,
  revision: &str,
  suite_hash: &str,
  worker_id: &str,
  report: &SemanticAssessmentReport,
) -> Result<()> {
  let policy = EvidencePolicy::new(revision, suite_hash);
  let requirement_before = verification_states(graph, policy)?;
  let criterion_before = criterion_states(graph, policy)?;
  let ids = graph
    .record_semantic_assessment(revision, Utc::now(), worker_id, report)
    .context("record independent semantic assessment")?;
  store::write_evidence_graph(cwd, graph).await?;

  for id in ids {
    let evidence = graph.evidence[&id].clone();
    let event = match evidence.result {
      EvidenceResult::Passed => RunEvent::EvidenceEstablished(evidence),
      EvidenceResult::Failed | EvidenceResult::Inconclusive => RunEvent::EvidenceFailed(evidence),
    };
    events.emit(event).await?;
  }
  emit_transitions(
    events,
    graph,
    EvidencePolicy::new(revision, suite_hash),
    requirement_before,
    criterion_before,
  )
  .await
}

pub async fn invalidate(
  cwd: &std::path::Path,
  events: &EventSink,
  graph: &mut EvidenceGraphState,
  revision: &str,
  suite_hash: &str,
) -> Result<()> {
  let policy = EvidencePolicy::new(revision, suite_hash);
  let requirement_before = verification_states(graph, policy)?;
  let criterion_before = criterion_states(graph, policy)?;
  let invalidated = graph.invalidate_where(revision, Utc::now(), |_| true);
  if invalidated.is_empty() {
    return Ok(());
  }
  store::write_evidence_graph(cwd, graph).await?;
  for evidence_id in invalidated {
    events
      .emit(RunEvent::EvidenceInvalidated {
        evidence_id,
        revision: revision.to_owned(),
      })
      .await?;
  }
  emit_transitions(
    events,
    graph,
    EvidencePolicy::new(revision, suite_hash),
    requirement_before,
    criterion_before,
  )
  .await
}

async fn emit_transitions(
  events: &EventSink,
  graph: &EvidenceGraphState,
  policy: EvidencePolicy<'_>,
  requirement_before: BTreeMap<RequirementId, VerificationState>,
  criterion_before: BTreeMap<CriterionId, VerificationState>,
) -> Result<()> {
  for (criterion_id, current) in criterion_states(graph, policy)? {
    let previous = criterion_before
      .get(&criterion_id)
      .copied()
      .unwrap_or(VerificationState::Unverified);
    if previous != current {
      events
        .emit(RunEvent::CriterionVerificationChanged {
          criterion_id,
          previous,
          current,
        })
        .await?;
    }
  }
  for (requirement_id, current) in verification_states(graph, policy)? {
    let previous = requirement_before
      .get(&requirement_id)
      .copied()
      .unwrap_or(VerificationState::Unverified);
    if previous != current {
      events
        .emit(RunEvent::RequirementVerificationChanged {
          requirement_id,
          previous,
          current,
        })
        .await?;
    }
  }
  Ok(())
}

pub fn stale_evidence(graph: &EvidenceGraphState) -> impl Iterator<Item = EvidenceId> + '_ {
  graph.evidence.values().filter_map(|evidence| {
    matches!(evidence.validity, EvidenceValidity::Stale { .. }).then_some(evidence.id)
  })
}
