use std::{
  collections::{BTreeMap, BTreeSet},
  path::{Component, Path},
};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};

use tenet_domain::{
  config::{CONFIG_FILE, TENET_DIR},
  events::{EventSink, RunEvent},
  evidence::{
    EvidenceGraphState, EvidencePolicy, EvidenceProjection, EvidenceValidity,
    SemanticAssessmentReport, VerificationState,
  },
  ids::{ArtifactId, CriterionId, EvidenceId, RequirementId},
  model::RequirementCatalog,
  proof::{
    ArtifactAuthority, ArtifactObservation, ArtifactProvenance, ArtifactValidity,
    DependencySurface, EvidenceArtifact, EvidenceArtifactKind, EvidenceContract, EvidencePredicate,
    EvidenceRequestProposal,
  },
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
  report: &SemanticAssessmentReport,
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
        EvidenceRequestProposal::Reproduce { .. } => {
          // A reproduction request is assessor advice, not an execution grant. The
          // persisted assessment retains it for a future controller-owned issuer.
        }
      }
    }
  }
  if acquired.is_empty() {
    return Ok(acquired);
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

  fn run_git(cwd: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
      .args(args)
      .current_dir(cwd)
      .output()
      .expect("run git");
    assert!(
      output.status.success(),
      "git {} failed: {}",
      args.join(" "),
      String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
  }

  #[tokio::test]
  async fn controller_source_inspection_issues_only_revision_bound_supporting_artifacts() {
    let project = tempfile::tempdir().expect("temporary project");
    run_git(project.path(), &["init"]);
    run_git(project.path(), &["config", "user.name", "Tenet Test"]);
    run_git(
      project.path(),
      &["config", "user.email", "tenet-test@localhost"],
    );
    std::fs::create_dir_all(project.path().join("src")).expect("create source directory");
    std::fs::write(
      project.path().join("src/lib.rs"),
      "pub fn value() -> u8 { 1 }\n",
    )
    .expect("write source");
    run_git(project.path(), &["add", "src/lib.rs"]);
    run_git(project.path(), &["commit", "-m", "source"]);
    let revision = run_git(project.path(), &["rev-parse", "HEAD"]);

    let artifact = inspect_source(
      project.path(),
      &revision,
      "src/lib.rs",
      1,
      1,
      tenet_domain::ids::ObligationId::from("VO-1"),
    )
    .await
    .expect("inspect immutable source");

    assert_eq!(artifact.revision, revision);
    assert_eq!(artifact.authority, ArtifactAuthority::Supporting);
    assert_eq!(
      artifact.provenance,
      ArtifactProvenance::ControllerSourceInspection
    );
    assert!(artifact.validate().is_ok());
    assert!(matches!(
      (&artifact.kind, &artifact.dependencies),
      (
        EvidenceArtifactKind::SourceSpan {
          path,
          blob_sha256,
          content_sha256,
          ..
        },
        DependencySurface::Paths { blob_hashes }
      ) if path == "src/lib.rs"
        && !content_sha256.is_empty()
        && blob_hashes.get(path) == Some(blob_sha256)
    ));
  }

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

  #[tokio::test]
  async fn reproduce_proposal_is_deferred_without_execution_or_artifact() {
    use tenet_domain::{
      evidence::{AcceptanceCriterion, ObligationAssessmentResult, VerificationObligation},
      ids::{CriterionId, ObligationId, RequirementId},
      proof::{AssessmentJudgment, EvidenceContract, EvidenceRequestProposal, GapKind, ProofState},
    };

    let project = tempfile::tempdir().expect("temporary project");
    let marker = project.path().join("assessor-command-ran");
    let obligation_id = ObligationId::from("REQ-001/AC-01/VO-01");
    let mut graph = EvidenceGraphState::new("spec");
    graph.register_requirement(RequirementId::from("REQ-001"), true);
    graph
      .add_criterion(AcceptanceCriterion {
        id: CriterionId::from("REQ-001/AC-01"),
        requirement_id: RequirementId::from("REQ-001"),
        description: "Observable behavior".into(),
        mandatory: true,
      })
      .expect("criterion");
    graph
      .add_obligation(VerificationObligation {
        id: obligation_id.clone(),
        criterion_id: CriterionId::from("REQ-001/AC-01"),
        description: "Unsupported proof".into(),
        required: true,
        evidence_contract: EvidenceContract::HumanAttestation {
          statement: "unavailable".into(),
        },
      })
      .expect("obligation");
    graph.derive_proofs("revision");
    let report = SemanticAssessmentReport {
      summary: "request deferred".into(),
      assessments: vec![ObligationAssessmentResult {
        obligation_id: obligation_id.clone(),
        assessment: AssessmentJudgment::Insufficient {
          reason: "evidence absent".into(),
          proposals: vec![EvidenceRequestProposal::Reproduce {
            program: "touch".into(),
            args: vec![marker.display().to_string()],
          }],
          gap_kind: GapKind::Evidence,
        },
      }],
    };

    let acquired = acquire_assessment_proposals(project.path(), &mut graph, "revision", &report)
      .await
      .expect("defer proposal");

    assert!(acquired.is_empty());
    assert!(!marker.exists());
    assert!(graph.artifacts.is_empty());
    assert_eq!(
      graph.proof_derivations[&obligation_id].state,
      ProofState::Insufficient
    );
  }
}
