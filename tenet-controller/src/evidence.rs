use std::{
  collections::{BTreeMap, BTreeSet},
  path::{Component, Path},
};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use tenet_domain::{
  config::{Config, CONFIG_FILE, TENET_DIR},
  events::{EventSink, RunEvent},
  evidence::{
    EvidenceGraphState, EvidencePolicy, EvidenceProjection, EvidenceValidity,
    SemanticAssessmentReport, VerificationState,
  },
  ids::{ArtifactId, CriterionId, EvidenceId, RequirementId, VerificationRunId},
  model::RequirementCatalog,
  proof::{
    ArtifactAuthority, ArtifactObservation, ArtifactProvenance, ArtifactValidity,
    DependencySurface, EvidenceArtifact, EvidenceArtifactKind, EvidenceContract, EvidencePredicate,
    EvidenceRequestProposal, ExecutionDomain, ExecutionObservation,
  },
  verification::{
    ProjectVerificationRun, VerificationAuthority, VerificationExecutionRequest, VerificationSpec,
  },
};

use tenet_runtime::{store, verifier, workspace::WorkspaceManager};

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
  graph
    .record_project_artifacts(report)
    .context("record controller-issued project evidence artifacts")?;
  graph.derive_proofs(&report.revision);
  store::write_evidence_graph(cwd, graph).await
}

pub async fn establish_semantic_assessment(
  cwd: &std::path::Path,
  _events: &EventSink,
  graph: &mut EvidenceGraphState,
  revision: &str,
  _suite_hash: &str,
  worker_id: &str,
  report: &SemanticAssessmentReport,
) -> Result<()> {
  graph
    .record_semantic_assessment(revision, Utc::now(), worker_id, report)
    .context("record advisory semantic assessment")?;
  graph.derive_proofs(revision);
  store::write_evidence_graph(cwd, graph).await
}
pub async fn acquire_assessment_proposals(
  cwd: &Path,
  graph: &mut EvidenceGraphState,
  revision: &str,
  worker_id: &str,
  report: &SemanticAssessmentReport,
  config: &Config,
  cancel: &CancellationToken,
) -> Result<Vec<ArtifactId>> {
  let mut acquired = Vec::new();
  for item in &report.assessments {
    let obligation = graph
      .obligations
      .get(&item.obligation_id)
      .context("assessment targets unknown obligation")?
      .clone();
    for proposal in item.assessment.proposals() {
      match proposal {
        EvidenceRequestProposal::RunProjectCheck { name } => {
          let admitted = contract_contains_named_check(&obligation.evidence_contract, name);
          let exists = graph.artifacts.values().any(|artifact| {
            artifact.revision == revision
              && artifact.obligation_ids.contains(&item.obligation_id)
              && matches!(&artifact.kind, EvidenceArtifactKind::CommandExecution { check_name: Some(actual), .. } if actual == name)
          });
          if !admitted || !exists {
            continue;
          }
        }
        EvidenceRequestProposal::InspectSource {
          path,
          start_line,
          end_line,
        } => {
          let Some(artifact) = acquire_source_proposal(
            cwd,
            revision,
            path,
            *start_line,
            *end_line,
            item.obligation_id.clone(),
          )
          .await
          else {
            continue;
          };
          acquired.push(artifact.id);
          graph
            .establish_artifact(artifact)
            .context("record controller source inspection")?;
        }
        EvidenceRequestProposal::Reproduce { program, args } => {
          if program.trim().is_empty() {
            continue;
          }
          let manager = WorkspaceManager::new(cwd.to_path_buf(), format!("evidence-{worker_id}"));
          let workspace = match manager
            .create_disposable("agent-reproduction", revision)
            .await
          {
            Ok(workspace) => workspace,
            Err(_) => continue,
          };
          let request = VerificationExecutionRequest {
            run_id: VerificationRunId::new(),
            obligation_id: item.obligation_id.clone(),
            spec: VerificationSpec {
              program: program.clone(),
              args: args.clone(),
              working_directory: ".".into(),
              environment: BTreeMap::new(),
            },
            authority: VerificationAuthority::AgentProposed,
          };
          let execution =
            verifier::run_execution_requests_cancelled(&workspace, config, &[request], cancel)
              .await;
          let cleanup = manager.remove(&workspace).await;
          let execution = match (execution, cleanup) {
            (_, Err(error)) => return Err(error).context("remove evidence workspace"),
            (Err(error), Ok(())) => {
              if cancel.is_cancelled() {
                return Err(error);
              }
              continue;
            }
            (Ok(execution), Ok(())) => execution,
          };
          for result in execution.executions {
            let artifact = EvidenceArtifact {
              id: ArtifactId::new(),
              revision: revision.to_owned(),
              observed_at: execution.finished_at,
              authority: ArtifactAuthority::Advisory,
              provenance: ArtifactProvenance::AgentProposedExecution {
                worker_role: "assess".into(),
              },
              observation: if result.result.exit_code == Some(0) && !result.result.timed_out {
                ArtifactObservation::Supports
              } else {
                ArtifactObservation::Contradicts
              },
              kind: EvidenceArtifactKind::CommandExecution {
                check_name: None,
                run_id: result.run_id,
                spec: result.spec,
                result: ExecutionObservation {
                  command: result.result.command.clone(),
                  exit_code: result.result.exit_code,
                  timed_out: result.result.timed_out,
                  duration_ms: u64::try_from(result.result.duration_ms).unwrap_or(u64::MAX),
                  stdout: result.result.stdout,
                  stderr: result.result.stderr,
                },
                domain: ExecutionDomain::Worker,
                execution_authority: VerificationAuthority::AgentProposed,
              },
              obligation_ids: BTreeSet::from([item.obligation_id.clone()]),
              validity: ArtifactValidity::Valid,
              dependencies: DependencySurface::Unknown,
              compatible_revisions: BTreeSet::new(),
            };
            acquired.push(artifact.id);
            graph
              .establish_artifact(artifact)
              .context("record advisory reproduction")?;
          }
        }
      }
    }
  }
  graph.derive_proofs(revision);
  store::write_evidence_graph(cwd, graph).await?;

  Ok(acquired)
}
async fn acquire_source_proposal(
  cwd: &Path,
  revision: &str,
  path: &str,
  start_line: u32,
  end_line: u32,
  obligation_id: tenet_domain::ids::ObligationId,
) -> Option<EvidenceArtifact> {
  inspect_source(cwd, revision, path, start_line, end_line, obligation_id)
    .await
    .ok()
}

async fn inspect_source(
  cwd: &Path,
  revision: &str,
  path: &str,
  start_line: u32,
  end_line: u32,
  obligation_id: tenet_domain::ids::ObligationId,
) -> Result<EvidenceArtifact> {
  let relative = Path::new(path);
  if relative.is_absolute()
    || relative.components().any(|component| {
      matches!(
        component,
        Component::ParentDir | Component::RootDir | Component::Prefix(_)
      )
    })
    || relative.starts_with(TENET_DIR)
    || path == CONFIG_FILE
    || start_line == 0
    || end_line < start_line
  {
    bail!("inadmissible source inspection proposal");
  }
  let output = tokio::process::Command::new("git")
    .args(["show", &format!("{revision}:{path}")])
    .current_dir(cwd)
    .output()
    .await
    .context("inspect immutable source blob")?;
  if !output.status.success() {
    bail!("source inspection path does not exist at requested revision");
  }
  let content = output.stdout;
  let mut offsets = vec![0usize];
  offsets.extend(
    content
      .iter()
      .enumerate()
      .filter_map(|(index, byte)| (*byte == b'\n').then_some(index + 1)),
  );
  let start = *offsets
    .get(start_line.saturating_sub(1) as usize)
    .context("source inspection starts beyond file")?;
  let end = offsets
    .get(end_line as usize)
    .copied()
    .unwrap_or(content.len());
  let blob_hash = sha256_hex(&content);
  let span_hash = sha256_hex(&content[start..end]);
  Ok(EvidenceArtifact {
    id: ArtifactId::new(),
    revision: revision.to_owned(),
    observed_at: Utc::now(),
    authority: ArtifactAuthority::Supporting,
    provenance: ArtifactProvenance::ControllerSourceInspection,
    observation: ArtifactObservation::Supports,
    kind: EvidenceArtifactKind::SourceSpan {
      path: path.to_owned(),
      blob_sha256: blob_hash.clone(),
      content_sha256: span_hash,
      start_byte: start as u64,
      end_byte: end as u64,
    },
    obligation_ids: BTreeSet::from([obligation_id]),
    validity: ArtifactValidity::Valid,
    dependencies: DependencySurface::Paths {
      blob_hashes: BTreeMap::from([(path.to_owned(), blob_hash)]),
    },
    compatible_revisions: BTreeSet::new(),
  })
}

fn sha256_hex(bytes: &[u8]) -> String {
  const HEX: &[u8; 16] = b"0123456789abcdef";
  let digest = Sha256::digest(bytes);
  let mut encoded = String::with_capacity(digest.len() * 2);
  for byte in digest {
    encoded.push(HEX[(byte >> 4) as usize] as char);
    encoded.push(HEX[(byte & 0x0f) as usize] as char);
  }
  encoded
}

fn contract_contains_named_check(contract: &EvidenceContract, name: &str) -> bool {
  match contract {
    EvidenceContract::Artifact {
      predicate: EvidencePredicate::NamedProjectCheck { name: expected },
    } => expected == name,
    EvidenceContract::All { requirements } | EvidenceContract::Any { requirements } => requirements
      .iter()
      .any(|item| contract_contains_named_check(item, name)),
    _ => false,
  }
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
  let invalidated_artifacts = graph.transition_artifacts(revision, None);
  if invalidated.is_empty() && invalidated_artifacts.is_empty() {
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
#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn inadmissible_source_proposal_is_rejected_without_error() {
    let artifact = acquire_source_proposal(
      Path::new("."),
      "revision",
      "../outside",
      0,
      1,
      tenet_domain::ids::ObligationId::from("VO-1"),
    )
    .await;
    assert!(artifact.is_none());
  }
}
