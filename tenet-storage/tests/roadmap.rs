use chrono::Utc;
use tenet_domain::{
  evidence::ImplementationState,
  ids::{CriterionId, ObligationId, RequirementId},
  model::{CandidateCheck, ReconcileResult, RequirementAssessment, WorkScope, WorkUnit},
};
use tenet_storage::Storage;

mod support;

fn reconciliation() -> ReconcileResult {
  ReconcileResult {
    summary: "Two ordered units".into(),
    requirements: vec![RequirementAssessment {
      requirement_id: RequirementId::from("REQ-001"),
      implementation_state: ImplementationState::Partial,
      observations: vec!["Storage exists".into()],
      missing_implementation: vec!["Controller cutover".into()],
      missing_evidence: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
    }],
    work_units: vec![
      WorkUnit {
        id: "WU-001".into(),
        title: "Persist".into(),
        objective: "Persist state".into(),
        requirement_ids: vec![RequirementId::from("REQ-001")],
        criterion_ids: vec![CriterionId::from("REQ-001/AC-01")],
        verification_obligation_ids: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
        suggested_checks: vec![CandidateCheck {
          obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
          command: "cargo test".into(),
        }],
        depends_on: Vec::new(),
        scope: WorkScope {
          paths: vec!["tenet-storage/**".into()],
        },
      },
      WorkUnit {
        id: "WU-002".into(),
        title: "Cut over".into(),
        objective: "Use SQLite".into(),
        requirement_ids: vec![RequirementId::from("REQ-001")],
        criterion_ids: vec![CriterionId::from("REQ-001/AC-01")],
        verification_obligation_ids: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
        suggested_checks: Vec::new(),
        depends_on: vec!["WU-001".into()],
        scope: WorkScope {
          paths: vec!["tenet-controller/**".into()],
        },
      },
    ],
  }
}

#[tokio::test]
async fn reconcile_round_trip_preserves_ordered_graph_facts() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let catalog = support::catalog();
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("catalog");
  storage.create_run("run-1").await.expect("run");
  let expected = reconciliation();

  storage
    .persist_reconcile_round("run-1", 1, "revision-1", &catalog.spec_hash, &expected)
    .await
    .expect("persist reconciliation");

  assert_eq!(
    storage
      .load_latest_reconcile_result("run-1")
      .await
      .expect("load reconciliation"),
    Some(expected)
  );
}

#[tokio::test]
async fn self_dependency_is_rejected_without_partial_round() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let catalog = support::catalog();
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("catalog");
  storage.create_run("run-1").await.expect("run");
  let mut invalid = reconciliation();
  invalid.work_units[0].depends_on = vec!["WU-001".into()];

  storage
    .persist_reconcile_round("run-1", 1, "revision-1", &catalog.spec_hash, &invalid)
    .await
    .expect_err("self dependency rejected");

  assert_eq!(
    storage
      .load_latest_reconcile_result("run-1")
      .await
      .expect("round"),
    None
  );
}

#[tokio::test]
async fn unknown_requirement_link_is_rejected_without_partial_work_unit() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let catalog = support::catalog();
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("catalog");
  storage.create_run("run-1").await.expect("run");
  let mut invalid = reconciliation();
  invalid.work_units[0].requirement_ids = vec![RequirementId::from("REQ-404")];

  storage
    .persist_reconcile_round("run-1", 1, "revision-1", &catalog.spec_hash, &invalid)
    .await
    .expect_err("unknown requirement rejected");

  assert_eq!(
    storage
      .load_latest_reconcile_result("run-1")
      .await
      .expect("round"),
    None
  );
}
