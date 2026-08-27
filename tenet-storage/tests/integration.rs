use chrono::{TimeZone, Utc};
use tenet_domain::{
  ids::VerificationRunId,
  model::{IntegrationPhase, IntegrationTransaction},
  verification::ProjectVerificationRun,
};
use tenet_storage::{install_controller_authority_key, Storage};

mod support;

async fn prepared() -> (tempfile::TempDir, Storage, IntegrationTransaction) {
  install_controller_authority_key("tenet-integration-tests", b"tenet-integration-authority")
    .expect("install project authority key");
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let catalog = support::catalog();
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("catalog");
  storage.create_run("run-1").await.expect("run");
  let reconciliation = support::reconciliation();
  storage
    .persist_reconcile_round("run-1", 1, "old", &catalog.spec_hash, &reconciliation)
    .await
    .expect("round");
  let verification_id = VerificationRunId::new();
  let now = Utc.with_ymd_and_hms(2026, 8, 20, 10, 0, 0).unwrap();
  storage
    .record_project_verification(
      "run-1",
      &ProjectVerificationRun {
        run_id: verification_id,
        revision: "new".into(),
        suite_hash: "suite".into(),
        checks: Vec::new(),
        passed: true,
        started_at: now,
        finished_at: now,
      },
    )
    .await
    .expect("verification");
  let transaction = IntegrationTransaction {
    version: IntegrationTransaction::VERSION,
    id: "integration-1".into(),
    run_id: "run-1".into(),
    work_unit: reconciliation.work_units[0].clone(),
    candidate_revision: "candidate".into(),
    old_head: "old".into(),
    new_head: "new".into(),
    phase: IntegrationPhase::Prepared,
    verification_run_id: verification_id,
    verification_hash: "verification-hash".into(),
    created_at: now.to_rfc3339(),
    updated_at: now.to_rfc3339(),
  };
  (project, storage, transaction)
}

#[tokio::test]
async fn integration_transaction_round_trip_preserves_recovery_identity() {
  let (_project, storage, expected) = prepared().await;

  storage
    .prepare_integration(&expected)
    .await
    .expect("prepare");

  assert_eq!(
    storage
      .load_active_integration("run-1")
      .await
      .expect("load"),
    Some(expected)
  );
}

#[tokio::test]
async fn only_one_active_integration_is_permitted_per_run() {
  let (_project, storage, first) = prepared().await;
  storage.prepare_integration(&first).await.expect("first");
  let mut second = first.clone();
  second.id = "integration-2".into();

  storage
    .prepare_integration(&second)
    .await
    .expect_err("duplicate active integration rejected");

  assert_eq!(
    storage
      .load_active_integration("run-1")
      .await
      .expect("active"),
    Some(first)
  );
}

#[tokio::test]
async fn integration_completion_atomically_records_completed_work() {
  let (_project, storage, mut transaction) = prepared().await;
  storage
    .prepare_integration(&transaction)
    .await
    .expect("prepare");
  transaction.phase = IntegrationPhase::GitCommitted;
  transaction.updated_at = "2026-08-20T10:00:01+00:00".into();
  storage
    .mark_integration_git_committed(&transaction)
    .await
    .expect("git committed");

  storage
    .complete_integration(&transaction, "2026-08-20T10:00:02+00:00")
    .await
    .expect("complete");

  assert!(storage
    .load_active_integration("run-1")
    .await
    .expect("active")
    .is_none());
  let state = storage.load_current_state().await.expect("state");
  assert_eq!(state.completed_work_units.len(), 1);
  assert_eq!(
    state.completed_work_units[0].verification_run_id,
    transaction.verification_run_id
  );
}
