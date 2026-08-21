use chrono::{TimeZone, Utc};
use tenet_domain::{
  evidence::{
    EvidencePolicy, ObligationAssessment, ObligationAssessmentResult, SemanticAssessmentReport,
    VerificationState,
  },
  ids::{ObligationId, RequirementId, VerificationRunId},
  verification::{CommandResult, ProjectCheckResult, ProjectVerificationRun, VerificationSpec},
};
use tenet_storage::Storage;

mod support;

#[tokio::test]
async fn evidence_graph_round_trip_preserves_policy_semantics() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let catalog = support::catalog();
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("catalog");
  storage.create_run("run-1").await.expect("run");
  let now = Utc.with_ymd_and_hms(2026, 8, 20, 10, 0, 0).unwrap();
  let verification = ProjectVerificationRun {
    run_id: VerificationRunId::new(),
    revision: "revision-1".into(),
    suite_hash: "suite-1".into(),
    checks: vec![ProjectCheckResult {
      name: "quality".into(),
      spec: VerificationSpec {
        program: "cargo".into(),
        args: vec!["test".into()],
        working_directory: ".".into(),
        environment: [("CI".into(), "true".into())].into_iter().collect(),
      },
      timeout_secs: 60,
      result: CommandResult {
        command: "cargo test".into(),
        exit_code: Some(0),
        timed_out: false,
        duration_ms: 12,
        stdout: "ok".into(),
        stderr: String::new(),
      },
    }],
    passed: true,
    started_at: now,
    finished_at: now,
  };
  storage
    .record_project_verification("run-1", &verification)
    .await
    .expect("verification");
  storage
    .record_semantic_assessment(
      "run-1",
      "revision-1",
      now,
      "assessor-1",
      &SemanticAssessmentReport {
        summary: "satisfied".into(),
        assessments: vec![ObligationAssessmentResult {
          obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
          assessment: ObligationAssessment::Satisfied {
            rationale: "Observed durable state".into(),
            evidence_refs: vec!["verification:quality".into()],
          },
        }],
      },
    )
    .await
    .expect("semantic evidence");

  let graph = storage.load_evidence_graph(&catalog).await.expect("graph");
  assert_eq!(
    graph.requirement_verification_state(
      &RequirementId::from("REQ-001"),
      EvidencePolicy::new("revision-1", "suite-1")
    ),
    Ok(VerificationState::Verified)
  );
  assert_eq!(graph.project_evidence.len(), 1);
  assert_eq!(graph.evidence.len(), 1);
}

#[tokio::test]
async fn stale_evidence_is_excluded_from_current_obligation_projection() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let catalog = support::catalog();
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("catalog");
  storage.create_run("run-1").await.expect("run");
  let now = Utc::now();
  storage
    .record_semantic_assessment(
      "run-1",
      "revision-1",
      now,
      "assessor-1",
      &SemanticAssessmentReport {
        summary: "satisfied".into(),
        assessments: vec![ObligationAssessmentResult {
          obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
          assessment: ObligationAssessment::Satisfied {
            rationale: "Observed".into(),
            evidence_refs: Vec::new(),
          },
        }],
      },
    )
    .await
    .expect("evidence");
  storage
    .invalidate_evidence_for_revision("run-1", "revision-2", now)
    .await
    .expect("invalidate");

  let current = storage
    .load_obligation_evidence(&ObligationId::from("REQ-001/AC-01/VO-01"), "revision-2")
    .await
    .expect("projection");

  assert!(current.is_empty());
  let full = storage
    .load_evidence_graph(&catalog)
    .await
    .expect("full graph");
  assert_eq!(full.evidence.len(), 1);
}

#[tokio::test]
async fn semantic_assessment_rolls_back_when_any_obligation_is_unknown() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let catalog = support::catalog();
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("catalog");
  storage.create_run("run-1").await.expect("run");
  let report = SemanticAssessmentReport {
    summary: "invalid".into(),
    assessments: vec![ObligationAssessmentResult {
      obligation_id: ObligationId::from("REQ-404/AC-01/VO-01"),
      assessment: ObligationAssessment::Gap {
        description: "missing".into(),
      },
    }],
  };

  storage
    .record_semantic_assessment("run-1", "revision-1", Utc::now(), "assessor", &report)
    .await
    .expect_err("unknown obligation rejected");

  assert_eq!(
    storage.load_evidence_graph(&catalog).await.expect("graph"),
    support::empty_graph(&catalog)
  );
}
