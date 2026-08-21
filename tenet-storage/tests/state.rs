use chrono::Utc;
use tenet_domain::model::{
  DeferredCandidate, Discovery, DiscoveryRecord, DiscoveryStatus, Phase, RepairProgress, RunStatus,
  State, WorkLease, WorkStatus, WorkerRole, WorkerSummary,
};
use tenet_storage::Storage;

mod support;

async fn prepared_storage() -> (tempfile::TempDir, Storage, tenet_domain::model::WorkUnit) {
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
    .persist_reconcile_round(
      "run-1",
      1,
      "revision-1",
      &catalog.spec_hash,
      &reconciliation,
    )
    .await
    .expect("reconciliation");
  (project, storage, reconciliation.work_units[0].clone())
}

#[tokio::test]
async fn state_round_trip_preserves_authority_relevant_run_data() {
  let (_project, storage, unit) = prepared_storage().await;
  let lease = WorkLease {
    id: "lease-1".into(),
    worker_id: "worker-1".into(),
    work_unit: unit.clone(),
    base_revision: "revision-1".into(),
    workspace: "/tmp/workspace".into(),
    issued_at: "2026-08-20T10:00:00+00:00".into(),
  };
  let mut state = State::fresh();
  state.run_id = Some("run-1".into());
  state.status = RunStatus::Running;
  state.phase = Phase::Repairing;
  state.cycle = 1;
  state.active_leases.insert(lease.id.clone(), lease);
  state
    .work_statuses
    .insert(unit.id.clone(), WorkStatus::Running);
  state.current_repair = Some(RepairProgress {
    work_unit_id: unit.id.clone(),
    attempt: 2,
  });
  state.discoveries.push(DiscoveryRecord {
    fingerprint: "discovery-1".into(),
    discovery: Discovery::Blocker {
      description: "External dependency".into(),
    },
    catalog_hash: "spec-hash".into(),
    repository_revision: "revision-1".into(),
    work_unit_id: unit.id.clone(),
    role: WorkerRole::Repair,
    cycle: 1,
    status: DiscoveryStatus::Active,
  });
  state.last_summary = "Repairing WU-001".into();
  state.updated_at = "2026-08-20T10:00:00+00:00".into();

  storage.persist_state(&state).await.expect("persist state");

  assert_eq!(
    storage.load_current_state().await.expect("load state"),
    state
  );
}

#[tokio::test]
async fn deferred_candidate_survives_new_run_publication_before_reconciliation() {
  let (_project, storage, unit) = prepared_storage().await;
  let lease = WorkLease {
    id: "lease-deferred".into(),
    worker_id: "worker-1".into(),
    work_unit: unit,
    base_revision: "revision-1".into(),
    workspace: "/tmp/deferred".into(),
    issued_at: "2026-08-20T10:00:00+00:00".into(),
  };
  let deferred = DeferredCandidate {
    lease,
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
    base_revision: "revision-1".into(),
    candidate_revision: "candidate-revision".into(),
    changed_paths: vec!["src/lib.rs".into()],
    discoveries: Vec::new(),
    catalog_hash: "spec-hash".into(),
    git_ref: "refs/tenet/candidates/candidate-revision".into(),
  };
  let mut first = State::fresh();
  first.run_id = Some("run-1".into());
  first.status = RunStatus::Blocked;
  first.phase = Phase::Scheduling;
  first.deferred_candidates.push(deferred.clone());
  storage
    .persist_state(&first)
    .await
    .expect("historical deferred state");

  storage.create_run("run-2").await.expect("new run");
  let mut second = first;
  second.run_id = Some("run-2".into());
  second.status = RunStatus::Running;
  second.phase = Phase::Architecting;
  storage
    .persist_state(&second)
    .await
    .expect("new run publication");

  let reloaded = storage.load_current_state().await.expect("reloaded state");
  assert_eq!(reloaded.run_id.as_deref(), Some("run-2"));
  assert_eq!(reloaded.deferred_candidates, vec![deferred.clone()]);
  assert_eq!(
    sqlx::query_scalar::<_, String>("SELECT id FROM candidates")
      .fetch_one(storage.pool())
      .await
      .expect("candidate identity"),
    "candidate-revision"
  );

  let catalog = support::catalog();
  let mut changed = support::reconciliation();
  changed.work_units[0].objective = "Different authority under a reused ID".into();
  storage
    .persist_reconcile_round("run-2", 1, "revision-2", &catalog.spec_hash, &changed)
    .await
    .expect("current reconciliation");
  storage
    .persist_state(&reloaded)
    .await
    .expect("reassociate candidate projection");
  let after_reconciliation = storage
    .load_current_state()
    .await
    .expect("authority snapshot");
  assert_eq!(after_reconciliation.deferred_candidates, vec![deferred]);
  assert_eq!(
    sqlx::query_scalar::<_, String>(
      "SELECT run_id FROM candidates WHERE id = 'candidate-revision'"
    )
    .fetch_one(storage.pool())
    .await
    .expect("candidate owner"),
    "run-1"
  );
  assert_eq!(
    sqlx::query_scalar::<_, String>("SELECT run_id FROM leases WHERE id = 'lease-deferred'")
      .fetch_one(storage.pool())
      .await
      .expect("lease owner"),
    "run-1"
  );
}

#[tokio::test]
async fn state_transition_rolls_back_when_work_status_references_unknown_unit() {
  let (_project, storage, unit) = prepared_storage().await;
  let mut valid = State::fresh();
  valid.run_id = Some("run-1".into());
  valid.status = RunStatus::Running;
  valid.phase = Phase::Scheduling;
  valid.cycle = 1;
  valid
    .work_statuses
    .insert(unit.id.clone(), WorkStatus::Ready);
  storage.persist_state(&valid).await.expect("valid state");
  let mut invalid = valid.clone();
  invalid
    .work_statuses
    .insert("WU-404".into(), WorkStatus::Ready);
  invalid.last_summary = "must roll back".into();

  storage
    .persist_state(&invalid)
    .await
    .expect_err("unknown work unit rejected");

  assert_eq!(storage.load_current_state().await.expect("state"), valid);
}

#[tokio::test]
async fn empty_database_loads_fresh_state_without_creating_json() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");

  let state = storage.load_current_state().await.expect("fresh state");
  assert_eq!(state.status, RunStatus::Idle);
  assert_eq!(state.phase, Phase::Initialized);
  assert!(state.run_id.is_none());
  assert!(!project.path().join(".tenet/state.json").exists());
}
