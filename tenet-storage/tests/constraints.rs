use chrono::Utc;
use sqlx::Executor;
use tenet_storage::Storage;

mod support;

async fn prepared() -> (tempfile::TempDir, Storage) {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("storage");
  let catalog = support::catalog();
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("catalog");
  storage.create_run("run-1").await.expect("run");
  storage
    .persist_reconcile_round(
      "run-1",
      1,
      "revision-1",
      &catalog.spec_hash,
      &support::reconciliation(),
    )
    .await
    .expect("reconciliation");
  (project, storage)
}

#[tokio::test]
async fn direct_relational_constraint_violations_are_rejected() {
  let (_project, storage) = prepared().await;
  let pool = storage.pool();

  sqlx::query("INSERT INTO acceptance_criteria(id, requirement_id, ordinal, description, mandatory) VALUES ('AC-404', 'REQ-404', 10, 'invalid', 1)")
    .execute(pool)
    .await
    .expect_err("criterion cannot reference an unknown requirement");
  sqlx::query("INSERT INTO verification_obligations(id, criterion_id, ordinal, description, required) VALUES ('VO-404', 'AC-404', 10, 'invalid', 1)")
    .execute(pool)
    .await
    .expect_err("obligation cannot reference an unknown criterion");
  sqlx::query("INSERT INTO semantic_evidence(id, run_id, requirement_id, criterion_id, obligation_id, source, result, revision, observed_at, provenance_kind, worker_id, rationale, validity) VALUES ('EV-404', 'run-1', 'REQ-001', 'REQ-001/AC-01', 'VO-404', 'semantic_assessment', 'passed', 'revision-1', '2026-08-20T10:00:00Z', 'independent_assessment', 'assessor', 'invalid', 'valid')")
    .execute(pool)
    .await
    .expect_err("evidence cannot reference an unknown obligation");
  sqlx::query("INSERT INTO requirement_source_fragments(requirement_id, fragment_id, ordinal) VALUES ('REQ-001', 'SPEC-0001-abcdef', 20)")
    .execute(pool)
    .await
    .expect_err("duplicate graph edges are rejected");
  sqlx::query("INSERT INTO work_unit_dependencies(run_id, work_unit_id, dependency_id, ordinal) VALUES ('run-1', 'WU-001', 'WU-001', 0)")
    .execute(pool)
    .await
    .expect_err("self dependencies are rejected");

  pool.execute("INSERT INTO project_verification_runs(id, run_id, revision, suite_hash, passed, started_at, finished_at) VALUES ('VR-001', 'run-1', 'revision-1', 'suite', 1, '2026-08-20T10:00:00Z', '2026-08-20T10:00:01Z')")
    .await
    .expect("verification fixture");
  pool.execute("INSERT INTO integration_transactions(id, run_id, work_unit_id, candidate_revision, old_head, new_head, phase, verification_run_id, verification_hash, created_at, updated_at) VALUES ('IT-001', 'run-1', 'WU-001', 'candidate-1', 'old', 'new', 'prepared', 'VR-001', 'hash', '2026-08-20T10:00:00Z', '2026-08-20T10:00:00Z')")
    .await
    .expect("first active integration");
  pool.execute("INSERT INTO integration_transactions(id, run_id, work_unit_id, candidate_revision, old_head, new_head, phase, verification_run_id, verification_hash, created_at, updated_at) VALUES ('IT-002', 'run-1', 'WU-001', 'candidate-2', 'old', 'newer', 'prepared', 'VR-001', 'hash', '2026-08-20T10:00:00Z', '2026-08-20T10:00:00Z')")
    .await
    .expect_err("a run cannot have two active integrations");
}
