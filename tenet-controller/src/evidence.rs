use std::{
  collections::{BTreeMap, BTreeSet},
  path::{Component, Path},
};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};

use tenet_domain::{
  config::{CONFIG_FILE, TENET_DIR},
  events::{EventSink, EvidenceAcquisitionStage, EvidenceIssuer, RunEvent},
  evidence::{
    EvidenceGraphState, EvidencePolicy, EvidenceProjection, EvidenceValidity,
    SemanticAssessmentReport, VerificationState,
  },
  falsifier::{FalsificationExecutionRecord, FalsifierSpec},
  human_attestation::{HumanAttestationRecord, HumanAttestorSpec},
  ids::{ArtifactId, CriterionId, EvidenceId, RequirementId},
  model::RequirementCatalog,
  proof::{
    derive_proof_state, ArtifactAuthority, ArtifactObservation, ArtifactProvenance,
    ArtifactValidity, DependencyPolicy, DependencySurface, EvidenceAcquisitionIdentity,
    EvidenceAcquisitionKind, EvidenceAcquisitionRequest, EvidenceArtifact, EvidenceArtifactKind,
    EvidenceContract, EvidencePredicate, EvidenceRequestProposal, ProofState,
  },
  trusted_verifier::{TrustedExecutionRecord, TrustedVerificationSpec},
  verification::ProjectVerificationRun,
};

use tenet_runtime::{git, store};

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
pub fn plan_missing_evidence(
  graph: &EvidenceGraphState,
  revision: &str,
  trusted_specs: &[TrustedVerificationSpec],
  falsifier_specs: &[FalsifierSpec],
  attempted: &BTreeSet<EvidenceAcquisitionIdentity>,
) -> Result<Vec<EvidenceAcquisitionRequest>> {
  let mut requests: Vec<EvidenceAcquisitionRequest> = Vec::new();
  for obligation in graph.obligations.values().filter(|item| item.required) {
    let Some(kind) = next_acquirable_kind(
      &obligation.evidence_contract,
      &obligation.id,
      graph,
      revision,
      trusted_specs,
      falsifier_specs,
      attempted,
    )?
    else {
      continue;
    };
    let identity = EvidenceAcquisitionIdentity {
      revision: revision.to_owned(),
      kind: kind.clone(),
    };
    if let Some(existing) = requests
      .iter_mut()
      .find(|request| request.identity() == identity)
    {
      existing.obligation_ids.insert(obligation.id.clone());
    } else {
      requests.push(EvidenceAcquisitionRequest {
        revision: revision.to_owned(),
        kind,
        obligation_ids: BTreeSet::from([obligation.id.clone()]),
      });
    }
  }
  Ok(requests)
}

fn next_acquirable_kind(
  contract: &EvidenceContract,
  obligation_id: &tenet_domain::ids::ObligationId,
  graph: &EvidenceGraphState,
  revision: &str,
  trusted_specs: &[TrustedVerificationSpec],
  falsifier_specs: &[FalsifierSpec],
  attempted: &BTreeSet<EvidenceAcquisitionIdentity>,
) -> Result<Option<EvidenceAcquisitionKind>> {
  let proof = derive_proof_state(obligation_id, contract, graph.artifacts.values(), revision);
  if matches!(proof.state, ProofState::Proven | ProofState::Contradicted) {
    return Ok(None);
  }
  match contract {
    EvidenceContract::Artifact {
      predicate: EvidencePredicate::TrustedVerifierCheck { name },
    } => {
      let Some(spec) = trusted_specs.iter().find(|spec| spec.name == *name) else {
        return Ok(None);
      };
      let kind = EvidenceAcquisitionKind::TrustedVerifierCheck {
        name: name.clone(),
        spec_hash: spec.fingerprint()?,
      };
      let identity = EvidenceAcquisitionIdentity {
        revision: revision.to_owned(),
        kind: kind.clone(),
      };
      Ok((!attempted.contains(&identity)).then_some(kind))
    }
    EvidenceContract::Artifact {
      predicate: EvidencePredicate::FalsifierCheck { name },
    } => {
      let Some(spec) = falsifier_specs
        .iter()
        .find(|spec| spec.name() == name && spec.input.is_none())
      else {
        return Ok(None);
      };
      let kind = EvidenceAcquisitionKind::FalsifierCheck {
        name: name.clone(),
        spec_hash: spec.fingerprint()?,
        canonical_input: None,
      };
      let identity = EvidenceAcquisitionIdentity {
        revision: revision.to_owned(),
        kind: kind.clone(),
      };
      Ok((!attempted.contains(&identity)).then_some(kind))
    }
    EvidenceContract::HumanAttestation { statement } => {
      let kind = EvidenceAcquisitionKind::HumanAttestation {
        statement_hash: tenet_domain::proof::statement_hash(statement),
      };
      let identity = EvidenceAcquisitionIdentity {
        revision: revision.to_owned(),
        kind: kind.clone(),
      };
      Ok((!attempted.contains(&identity)).then_some(kind))
    }
    EvidenceContract::All { requirements } | EvidenceContract::Any { requirements } => {
      let mut pending_human = None;
      for requirement in requirements {
        if let Some(kind) = next_acquirable_kind(
          requirement,
          obligation_id,
          graph,
          revision,
          trusted_specs,
          falsifier_specs,
          attempted,
        )? {
          if matches!(kind, EvidenceAcquisitionKind::HumanAttestation { .. }) {
            pending_human.get_or_insert(kind);
          } else {
            return Ok(Some(kind));
          }
        }
      }
      Ok(pending_human)
    }
    EvidenceContract::Artifact { .. } => Ok(None),
  }
}

pub fn admit_assessment_proposals(
  graph: &EvidenceGraphState,
  revision: &str,
  report: &SemanticAssessmentReport,
  trusted_specs: &[TrustedVerificationSpec],
  falsifier_specs: &[FalsifierSpec],
  attempted: &BTreeSet<EvidenceAcquisitionIdentity>,
) -> Result<Vec<EvidenceAcquisitionRequest>> {
  let mut admitted: Vec<EvidenceAcquisitionRequest> = Vec::new();
  for item in &report.assessments {
    let obligation = graph
      .obligations
      .get(&item.obligation_id)
      .context("assessment targets unknown obligation")?;
    let proof = derive_proof_state(
      &obligation.id,
      &obligation.evidence_contract,
      graph.artifacts.values(),
      revision,
    );
    if matches!(proof.state, ProofState::Proven | ProofState::Contradicted) {
      continue;
    }
    for proposal in item.assessment.proposals() {
      let kind = match proposal {
        EvidenceRequestProposal::RunTrustedVerifierCheck { name } => {
          let predicate = EvidencePredicate::TrustedVerifierCheck { name: name.clone() };
          if !contract_contains_predicate(&obligation.evidence_contract, &predicate) {
            continue;
          }
          let Some(spec) = trusted_specs.iter().find(|spec| spec.name == *name) else {
            continue;
          };
          let leaf = EvidenceContract::Artifact { predicate };
          if derive_proof_state(&obligation.id, &leaf, graph.artifacts.values(), revision).state
            == ProofState::Proven
          {
            continue;
          }
          EvidenceAcquisitionKind::TrustedVerifierCheck {
            name: name.clone(),
            spec_hash: spec.fingerprint()?,
          }
        }
        EvidenceRequestProposal::RunFalsifierCheck { name, input } => {
          let predicate = EvidencePredicate::FalsifierCheck { name: name.clone() };
          if !contract_contains_predicate(&obligation.evidence_contract, &predicate) {
            continue;
          }
          let Some(spec) = falsifier_specs.iter().find(|spec| spec.name() == name) else {
            continue;
          };
          let Some(canonical_input) = admit_falsifier_input(spec, input.as_ref()) else {
            continue;
          };
          EvidenceAcquisitionKind::FalsifierCheck {
            name: name.clone(),
            spec_hash: spec.fingerprint()?,
            canonical_input,
          }
        }
        EvidenceRequestProposal::InspectSource {
          path,
          start_line,
          end_line,
        } => EvidenceAcquisitionKind::InspectSource {
          path: path.clone(),
          start_line: *start_line,
          end_line: *end_line,
        },
        EvidenceRequestProposal::RunProjectCheck { .. } => continue,
      };
      let identity = EvidenceAcquisitionIdentity {
        revision: revision.to_owned(),
        kind: kind.clone(),
      };
      if attempted.contains(&identity) {
        continue;
      }
      if let Some(request) = admitted
        .iter_mut()
        .find(|request| request.identity() == identity)
      {
        request.obligation_ids.insert(obligation.id.clone());
      } else {
        admitted.push(EvidenceAcquisitionRequest {
          revision: revision.to_owned(),
          kind,
          obligation_ids: BTreeSet::from([obligation.id.clone()]),
        });
      }
    }
  }
  Ok(admitted)
}

fn admit_falsifier_input(
  spec: &FalsifierSpec,
  input: Option<&serde_json::Value>,
) -> Option<Option<String>> {
  match (&spec.input, input) {
    (None, None) => Some(None),
    (Some(input_spec), Some(value)) => {
      let validator = jsonschema::validator_for(&input_spec.schema).ok()?;
      if !validator.is_valid(value) || spec.execution_spec(Some(value)).is_err() {
        return None;
      }
      serde_json::to_string(value).ok().map(Some)
    }
    _ => None,
  }
}

fn contract_contains_predicate(contract: &EvidenceContract, predicate: &EvidencePredicate) -> bool {
  match contract {
    EvidenceContract::Artifact { predicate: actual } => actual == predicate,
    EvidenceContract::All { requirements } | EvidenceContract::Any { requirements } => requirements
      .iter()
      .any(|requirement| contract_contains_predicate(requirement, predicate)),
    EvidenceContract::HumanAttestation { .. } => false,
  }
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
pub async fn record_trusted_execution(
  cwd: &Path,
  graph: &mut EvidenceGraphState,
  record: &TrustedExecutionRecord,
  spec: &TrustedVerificationSpec,
) -> Result<Option<ArtifactId>> {
  store::record_trusted_execution(cwd, record, spec).await?;
  let dependencies = materialize_dependencies(cwd, &record.revision, &spec.dependencies).await?;
  let artifact_id = graph
    .record_trusted_execution(record, spec, dependencies)
    .context("issue controller trusted-verifier artifact")?;
  graph.derive_proofs(&record.revision);
  store::write_evidence_graph(cwd, graph).await?;
  Ok(artifact_id)
}
pub async fn record_falsification(
  cwd: &Path,
  graph: &mut EvidenceGraphState,
  record: &FalsificationExecutionRecord,
  spec: &FalsifierSpec,
) -> Result<Option<ArtifactId>> {
  store::record_falsification(cwd, record, spec).await?;
  let dependencies =
    materialize_dependencies(cwd, &record.revision, &spec.execution.dependencies).await?;
  let artifact_id = graph
    .record_falsification(record, spec, dependencies)
    .context("issue controller falsifier artifact")?;
  graph.derive_proofs(&record.revision);
  store::write_evidence_graph(cwd, graph).await?;
  Ok(artifact_id)
}

async fn materialize_dependencies(
  cwd: &Path,
  revision: &str,
  policy: &DependencyPolicy,
) -> Result<DependencySurface> {
  if matches!(policy, DependencyPolicy::RepositoryWide) {
    return Ok(DependencySurface::RepositoryWide);
  }
  let blobs = git::repository_blob_hashes(cwd, revision).await?;
  policy
    .materialize(&blobs)
    .map_err(|error| anyhow::anyhow!(error))
}

pub async fn record_human_attestation(
  cwd: &Path,
  graph: &mut EvidenceGraphState,
  record: &HumanAttestationRecord,
  attestor: &HumanAttestorSpec,
  catalog_hash: &str,
) -> Result<ArtifactId> {
  let dependencies =
    materialize_dependencies(cwd, &record.revision, &attestor.dependencies).await?;
  if dependencies != record.dependencies {
    bail!("human attestation dependency snapshot was not controller-materialized");
  }
  store::record_human_attestation(cwd, record, attestor).await?;
  let artifact_id = graph
    .record_human_attestation(record, attestor, catalog_hash)
    .context("issue authenticated human-attestation artifact")?;
  graph.derive_proofs(&record.revision);
  store::write_evidence_graph(cwd, graph).await?;
  Ok(artifact_id)
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
  events: &EventSink,
  requests: &[EvidenceAcquisitionRequest],
) -> Result<Vec<ArtifactId>> {
  let mut acquired = Vec::new();
  for request in requests {
    let EvidenceAcquisitionKind::InspectSource {
      path,
      start_line,
      end_line,
    } = &request.kind
    else {
      continue;
    };
    let obligation_ids: Vec<_> = request.obligation_ids.iter().cloned().collect();
    for stage in [
      EvidenceAcquisitionStage::Admitted,
      EvidenceAcquisitionStage::IssuerSelected,
      EvidenceAcquisitionStage::Started,
    ] {
      events
        .emit(RunEvent::EvidenceAcquisition {
          stage,
          revision: request.revision.clone(),
          issuer: EvidenceIssuer::SourceInspection,
          obligation_ids: obligation_ids.clone(),
        })
        .await?;
    }
    let Some(artifact) = inspect_source(
      cwd,
      &request.revision,
      path,
      *start_line,
      *end_line,
      request.obligation_ids.clone(),
    )
    .await
    .ok() else {
      events
        .emit(RunEvent::EvidenceAcquisition {
          stage: EvidenceAcquisitionStage::Failed,
          revision: request.revision.clone(),
          issuer: EvidenceIssuer::SourceInspection,
          obligation_ids,
        })
        .await?;
      continue;
    };
    let artifact_id = artifact.id;
    acquired.push(artifact_id);
    graph
      .establish_artifact(artifact)
      .context("record controller source inspection")?;
    events
      .emit(RunEvent::ArtifactIssued {
        artifact_id,
        revision: request.revision.clone(),
        obligation_ids: request.obligation_ids.iter().cloned().collect(),
      })
      .await?;
    events
      .emit(RunEvent::EvidenceAcquisition {
        stage: EvidenceAcquisitionStage::Completed,
        revision: request.revision.clone(),
        issuer: EvidenceIssuer::SourceInspection,
        obligation_ids: request.obligation_ids.iter().cloned().collect(),
      })
      .await?;
  }
  if acquired.is_empty() {
    return Ok(acquired);
  }
  if let Some(revision) = requests.first().map(|request| request.revision.as_str()) {
    graph.derive_proofs(revision);
  }
  store::write_evidence_graph(cwd, graph).await?;
  Ok(acquired)
}

async fn inspect_source(
  cwd: &Path,
  revision: &str,
  path: &str,
  start_line: u32,
  end_line: u32,
  obligation_ids: BTreeSet<tenet_domain::ids::ObligationId>,
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
    obligation_ids,
    validity: ArtifactValidity::Valid,
    dependencies: DependencySurface::Paths {
      patterns: vec![path.to_owned()],
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
  let transitioning_artifacts: Vec<_> = graph
    .artifacts
    .values()
    .filter(|artifact| artifact.validity.is_valid() && !artifact.is_compatible_with(revision))
    .map(|artifact| (artifact.id, artifact.revision.clone()))
    .collect();
  let artifact_transition_needed = !transitioning_artifacts.is_empty();
  let needs_blob_hashes = graph.artifacts.values().any(|artifact| {
    artifact.validity.is_valid()
      && !artifact.is_compatible_with(revision)
      && matches!(&artifact.dependencies, DependencySurface::Paths { .. })
  });
  let repository_blobs = if needs_blob_hashes {
    Some(git::repository_blob_hashes(cwd, revision).await?)
  } else {
    None
  };
  graph.transition_artifacts(revision, repository_blobs.as_ref());
  if invalidated.is_empty() && !artifact_transition_needed {
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
  for (artifact_id, from_revision) in transitioning_artifacts {
    let artifact = graph
      .artifacts
      .get(&artifact_id)
      .context("transitioned artifact disappeared from evidence graph")?;
    let event = if artifact.is_compatible_with(revision) && artifact.validity.is_valid() {
      RunEvent::ArtifactReused {
        artifact_id,
        from_revision,
        to_revision: revision.to_owned(),
      }
    } else {
      RunEvent::ArtifactBecameStale {
        artifact_id,
        revision: revision.to_owned(),
      }
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
      BTreeSet::from([tenet_domain::ids::ObligationId::from("VO-1")]),
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
        DependencySurface::Paths { blob_hashes, .. }
      ) if path == "src/lib.rs"
        && !content_sha256.is_empty()
        && blob_hashes.get(path) == Some(blob_sha256)
    ));
  }

  #[tokio::test]
  async fn inadmissible_source_proposal_is_rejected_without_error() {
    let artifact = inspect_source(
      Path::new("."),
      "revision",
      "../outside",
      0,
      1,
      BTreeSet::from([tenet_domain::ids::ObligationId::from("VO-1")]),
    )
    .await;
    assert!(artifact.is_err());
  }

  #[tokio::test]
  async fn unknown_falsifier_proposal_cannot_execute_or_issue_artifact() {
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
          proposals: vec![EvidenceRequestProposal::RunFalsifierCheck {
            name: "touch".into(),
            input: Some(serde_json::json!({"path": marker})),
          }],
          gap_kind: GapKind::Evidence,
        },
      }],
    };

    let admitted =
      admit_assessment_proposals(&graph, "revision", &report, &[], &[], &BTreeSet::new())
        .expect("admit proposals");
    let acquired =
      acquire_assessment_proposals(project.path(), &mut graph, &EventSink::new(None), &admitted)
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

  fn trusted_spec(name: &str) -> TrustedVerificationSpec {
    use tenet_domain::trusted_verifier::TrustedExecutionBackend;

    TrustedVerificationSpec {
      name: name.into(),
      backend: TrustedExecutionBackend::Microsandbox,
      image: format!("example/{name}@sha256:{}", "a".repeat(64)),
      program: "/bin/check".into(),
      args: Vec::new(),
      working_directory: ".".into(),
      environment: BTreeMap::new(),
      timeout_secs: 300,
      isolation: Default::default(),
      resources: Default::default(),
      protocol: Default::default(),
      dependencies: Default::default(),
    }
  }

  fn graph_with_contracts(contracts: Vec<EvidenceContract>) -> EvidenceGraphState {
    use tenet_domain::{
      evidence::{AcceptanceCriterion, VerificationObligation},
      ids::{CriterionId, ObligationId, RequirementId},
    };

    let mut graph = EvidenceGraphState::new("spec");
    graph.register_requirement(RequirementId::from("REQ-001"), true);
    for (index, contract) in contracts.into_iter().enumerate() {
      let criterion_id = CriterionId::from(format!("REQ-001/AC-{index:02}"));
      graph
        .add_criterion(AcceptanceCriterion {
          id: criterion_id.clone(),
          requirement_id: RequirementId::from("REQ-001"),
          description: "Observable behavior".into(),
          mandatory: true,
        })
        .expect("criterion");
      graph
        .add_obligation(VerificationObligation {
          id: ObligationId::from(format!("REQ-001/AC-{index:02}/VO-01")),
          criterion_id,
          description: "Mechanical proof".into(),
          required: true,
          evidence_contract: contract,
        })
        .expect("obligation");
    }
    graph.derive_proofs("revision");
    graph
  }

  fn trusted_contract(name: &str) -> EvidenceContract {
    EvidenceContract::Artifact {
      predicate: EvidencePredicate::TrustedVerifierCheck { name: name.into() },
    }
  }

  #[test]
  fn planner_derives_trusted_request_from_contract_without_assessment() {
    let graph = graph_with_contracts(vec![trusted_contract("boundary")]);
    let requests = plan_missing_evidence(
      &graph,
      "revision",
      &[trusted_spec("boundary")],
      &[],
      &BTreeSet::new(),
    )
    .expect("plan evidence");

    assert_eq!(requests.len(), 1);
    assert!(matches!(
      &requests[0].kind,
      EvidenceAcquisitionKind::TrustedVerifierCheck { name, .. } if name == "boundary"
    ));
  }

  #[test]
  fn planner_uses_contract_order_and_skips_attempted_any_branches() {
    let graph = graph_with_contracts(vec![EvidenceContract::Any {
      requirements: vec![trusted_contract("first"), trusted_contract("second")],
    }]);
    let specs = [trusted_spec("first"), trusted_spec("second")];
    let first = plan_missing_evidence(&graph, "revision", &specs, &[], &BTreeSet::new())
      .expect("plan first branch")
      .remove(0);
    let attempted = BTreeSet::from([first.identity()]);

    let second = plan_missing_evidence(&graph, "revision", &specs, &[], &attempted)
      .expect("plan second branch")
      .remove(0);

    assert!(matches!(
      second.kind,
      EvidenceAcquisitionKind::TrustedVerifierCheck { name, .. } if name == "second"
    ));
  }

  #[test]
  fn planner_deduplicates_execution_and_derives_all_bindings() {
    let graph = graph_with_contracts(vec![trusted_contract("shared"), trusted_contract("shared")]);
    let requests = plan_missing_evidence(
      &graph,
      "revision",
      &[trusted_spec("shared")],
      &[],
      &BTreeSet::new(),
    )
    .expect("plan shared evidence");

    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].obligation_ids.len(), 2);
  }
  fn trusted_proposal(name: &str) -> SemanticAssessmentReport {
    use tenet_domain::{
      evidence::ObligationAssessmentResult,
      ids::ObligationId,
      proof::{AssessmentJudgment, GapKind},
    };

    SemanticAssessmentReport {
      summary: "request observation".into(),
      assessments: vec![ObligationAssessmentResult {
        obligation_id: ObligationId::from("REQ-001/AC-00/VO-01"),
        assessment: AssessmentJudgment::Insufficient {
          reason: "missing observation".into(),
          proposals: vec![EvidenceRequestProposal::RunTrustedVerifierCheck { name: name.into() }],
          gap_kind: GapKind::Evidence,
        },
      }],
    }
  }

  #[test]
  fn proposal_admission_rejects_unknown_or_unrelated_verifier() {
    let graph = graph_with_contracts(vec![trusted_contract("allowed")]);
    let admitted = admit_assessment_proposals(
      &graph,
      "revision",
      &trusted_proposal("invented"),
      &[trusted_spec("invented")],
      &[],
      &BTreeSet::new(),
    )
    .expect("evaluate proposal");

    assert!(admitted.is_empty());
  }

  #[test]
  fn proposal_admission_returns_only_configured_contract_capability() {
    let graph = graph_with_contracts(vec![trusted_contract("allowed")]);
    let admitted = admit_assessment_proposals(
      &graph,
      "revision",
      &trusted_proposal("allowed"),
      &[trusted_spec("allowed")],
      &[],
      &BTreeSet::new(),
    )
    .expect("evaluate proposal");

    assert_eq!(admitted.len(), 1);
    assert!(matches!(
      &admitted[0].kind,
      EvidenceAcquisitionKind::TrustedVerifierCheck { name, .. } if name == "allowed"
    ));
  }
  fn structured_falsifier(name: &str) -> FalsifierSpec {
    use tenet_domain::falsifier::{FalsifierProtocol, StructuredFalsifierInputSpec};

    FalsifierSpec {
      execution: trusted_spec(name),
      protocol: FalsifierProtocol::ExitCode,
      input: Some(StructuredFalsifierInputSpec {
        schema: serde_json::json!({
          "type": "object",
          "required": ["seed"],
          "properties": {"seed": {"type": "integer"}},
          "additionalProperties": false
        }),
        argument: "--input-json".into(),
        max_bytes: 128,
      }),
    }
  }

  fn falsifier_proposal(name: &str, input: serde_json::Value) -> SemanticAssessmentReport {
    use tenet_domain::{
      evidence::ObligationAssessmentResult,
      ids::ObligationId,
      proof::{AssessmentJudgment, GapKind},
    };

    SemanticAssessmentReport {
      summary: "try bounded counterexample".into(),
      assessments: vec![ObligationAssessmentResult {
        obligation_id: ObligationId::from("REQ-001/AC-00/VO-01"),
        assessment: AssessmentJudgment::Insufficient {
          reason: "missing falsifier observation".into(),
          proposals: vec![EvidenceRequestProposal::RunFalsifierCheck {
            name: name.into(),
            input: Some(input),
          }],
          gap_kind: GapKind::Evidence,
        },
      }],
    }
  }

  #[test]
  fn malformed_falsifier_input_is_rejected() {
    let contract = EvidenceContract::Artifact {
      predicate: EvidencePredicate::FalsifierCheck {
        name: "search".into(),
      },
    };
    let graph = graph_with_contracts(vec![contract]);
    let admitted = admit_assessment_proposals(
      &graph,
      "revision",
      &falsifier_proposal("search", serde_json::json!({"seed": "not-an-integer"})),
      &[],
      &[structured_falsifier("search")],
      &BTreeSet::new(),
    )
    .expect("evaluate malformed input");

    assert!(admitted.is_empty());
  }

  #[test]
  fn valid_falsifier_input_is_canonicalized_without_executable_fields() {
    let contract = EvidenceContract::Artifact {
      predicate: EvidencePredicate::FalsifierCheck {
        name: "search".into(),
      },
    };
    let graph = graph_with_contracts(vec![contract]);
    let admitted = admit_assessment_proposals(
      &graph,
      "revision",
      &falsifier_proposal("search", serde_json::json!({"seed": 7})),
      &[],
      &[structured_falsifier("search")],
      &BTreeSet::new(),
    )
    .expect("evaluate valid input");

    assert!(matches!(
      &admitted[0].kind,
      EvidenceAcquisitionKind::FalsifierCheck {
        name,
        canonical_input: Some(input),
        ..
      } if name == "search" && input == "{\"seed\":7}"
    ));
  }
  #[test]
  fn falsifier_proposal_for_unrelated_contract_is_rejected() {
    let contract = EvidenceContract::Artifact {
      predicate: EvidencePredicate::FalsifierCheck {
        name: "allowed".into(),
      },
    };
    let graph = graph_with_contracts(vec![contract]);
    let admitted = admit_assessment_proposals(
      &graph,
      "revision",
      &falsifier_proposal("unrelated", serde_json::json!({"seed": 7})),
      &[],
      &[structured_falsifier("unrelated")],
      &BTreeSet::new(),
    )
    .expect("evaluate unrelated proposal");

    assert!(admitted.is_empty());
  }
}
