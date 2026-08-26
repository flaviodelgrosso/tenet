use std::collections::BTreeMap;

use chrono::{TimeZone, Utc};
use tenet_domain::{
  evidence::{ObligationAssessmentResult, SemanticAssessmentReport},
  ids::{ArtifactId, ObligationId, VerificationRunId},
  proof::{AssessmentJudgment, EvidenceContract, EvidencePredicate, ProofState},
  verification::{CommandResult, ProjectCheckResult, ProjectVerificationRun, VerificationSpec},
};
use tenet_storage::Storage;

mod support;

fn project_run(revision: &str) -> ProjectVerificationRun {
  let now = Utc.with_ymd_and_hms(2026, 8, 26, 10, 0, 0).unwrap();
  ProjectVerificationRun {
    run_id: VerificationRunId::new(),
    revision: revision.into(),
    suite_hash: "suite".into(),
    checks: vec![ProjectCheckResult {
      name: "quality".into(),
      spec: VerificationSpec {
        program: "true".into(),
        args: Vec::new(),
        working_directory: ".".into(),
        environment: BTreeMap::new(),
      },
      timeout_secs: 10,
      result: CommandResult {
        command: "true".into(),
        exit_code: Some(0),
        timed_out: false,
        duration_ms: 1,
        stdout: String::new(),
        stderr: String::new(),
      },
    }],
    passed: true,
    started_at: now,
    finished_at: now,
  }
}

async fn prepared_storage() -> (
  tempfile::TempDir,
  Storage,
  tenet_domain::model::RequirementCatalog,
) {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let mut catalog = support::catalog();
  catalog.verification_obligations[0].evidence_contract = EvidenceContract::Artifact {
    predicate: EvidencePredicate::NamedProjectCheck {
      name: "quality".into(),
    },
  };
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("catalog");
  storage.create_run("run-1").await.expect("run");
  (project, storage, catalog)
}

#[tokio::test]
async fn artifact_and_derivation_survive_restart_with_provenance() {
  let (project_dir, storage, catalog) = prepared_storage().await;
  let project = project_run("revision-1");
  storage
    .record_project_verification("run-1", &project)
    .await
    .expect("project verification");
  let mut graph = support::empty_graph(&catalog);
  graph.record_project_verification(&project);
  let ids = graph.record_project_artifacts(&project).expect("artifacts");
  graph.derive_proofs("revision-1");
  storage
    .persist_evidence_graph("run-1", &graph)
    .await
    .expect("persist graph");

  let reopened = Storage::open(project_dir.path())
    .await
    .expect("reopen storage");
  let loaded = reopened
    .load_evidence_graph(&catalog)
    .await
    .expect("load graph");
  assert_eq!(loaded.artifacts.get(&ids[0]), graph.artifacts.get(&ids[0]));
  assert_eq!(
    loaded.proof_derivations[&ObligationId::from("REQ-001/AC-01/VO-01")].state,
    ProofState::Proven
  );
}

#[tokio::test]
async fn stale_artifact_and_blocking_proof_survive_restart() {
  let (_project, storage, catalog) = prepared_storage().await;
  let project = project_run("revision-1");
  let mut graph = support::empty_graph(&catalog);
  graph.record_project_verification(&project);
  graph.record_project_artifacts(&project).expect("artifacts");
  graph.derive_proofs("revision-1");
  graph.transition_artifacts("revision-2", None);
  storage
    .persist_evidence_graph("run-1", &graph)
    .await
    .expect("persist stale graph");

  let loaded = storage
    .load_evidence_graph(&catalog)
    .await
    .expect("load graph");
  assert_eq!(
    loaded.proof_derivations[&ObligationId::from("REQ-001/AC-01/VO-01")].state,
    ProofState::Stale
  );
}

#[tokio::test]
async fn fabricated_artifact_reference_is_rejected() {
  let (_project, _storage, catalog) = prepared_storage().await;
  let mut graph = support::empty_graph(&catalog);
  let report = SemanticAssessmentReport {
    summary: "fabricated".into(),
    assessments: vec![ObligationAssessmentResult {
      obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
      assessment: AssessmentJudgment::Supported {
        artifact_ids: vec![ArtifactId::new()],
        rationale: "invented".into(),
      },
    }],
  };
  let error = graph
    .record_semantic_assessment("revision-1", Utc::now(), "assessor", &report)
    .expect_err("fabricated artifact rejected");
  assert!(error.to_string().contains("unknown evidence artifact"));
}
#[tokio::test]
async fn forged_controller_execution_cannot_be_persisted() {
  let (_project, storage, catalog) = prepared_storage().await;
  let project = project_run("revision-1");
  let mut graph = support::empty_graph(&catalog);
  graph.record_project_verification(&project);
  graph.record_project_artifacts(&project).expect("artifacts");
  let artifact = graph.artifacts.values_mut().next().expect("artifact");
  if let tenet_domain::proof::EvidenceArtifactKind::CommandExecution { result, .. } =
    &mut artifact.kind
  {
    result.exit_code = Some(99);
  } else {
    panic!("expected command artifact");
  }

  let error = storage
    .persist_evidence_graph("run-1", &graph)
    .await
    .expect_err("forged execution rejected");
  assert!(error.to_string().contains("unauthorized combination"));
}
