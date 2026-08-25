use std::{str::FromStr, time::Duration};

use sqlx::{
  sqlite::{SqliteConnectOptions, SqlitePoolOptions},
  Row,
};
use tenet_domain::model::{Phase, RunStatus, State};
use tenet_storage::{DatabaseHealth, Storage};

#[tokio::test]
async fn open_creates_database_with_durable_pragmas_and_migrations() {
  let project = tempfile::tempdir().expect("temporary project");

  let storage = Storage::open(project.path()).await.expect("open storage");

  assert!(project.path().join(".tenet/tenet.db").exists());
  assert_eq!(
    storage.quick_check().await.expect("quick check"),
    DatabaseHealth::Ok
  );
  let row = sqlx::query("SELECT * FROM pragma_foreign_keys(), pragma_journal_mode(), pragma_synchronous(), pragma_busy_timeout()")
    .fetch_one(storage.pool())
    .await
    .expect("read pragmas");
  assert_eq!(row.get::<i64, _>(0), 1);
  assert_eq!(row.get::<String, _>(1), "wal");
  assert_eq!(row.get::<i64, _>(2), 2);
  assert!(row.get::<i64, _>(3) >= Duration::from_secs(5).as_millis() as i64);
  assert!(
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM _sqlx_migrations")
      .fetch_one(storage.pool())
      .await
      .expect("migration count")
      > 0
  );
  drop(storage);
  let inspection = Storage::open_existing(project.path())
    .await
    .expect("read-only inspection");
  assert_eq!(
    inspection.quick_check().await.expect("inspection check"),
    DatabaseHealth::Ok
  );
}

#[tokio::test]
async fn existing_database_migrates_to_review_required_lifecycle() {
  let project = tempfile::tempdir().expect("temporary project");
  let state_directory = project.path().join(".tenet");
  tokio::fs::create_dir(&state_directory)
    .await
    .expect("state directory");
  let database_path = state_directory.join("tenet.db");
  let pool = SqlitePoolOptions::new()
    .connect_with(
      SqliteConnectOptions::from_str("sqlite::memory:")
        .expect("sqlite options")
        .filename(&database_path)
        .in_memory(false)
        .create_if_missing(true)
        .foreign_keys(true),
    )
    .await
    .expect("open legacy database");
  let legacy_migrations = tempfile::tempdir().expect("migration directory");
  tokio::fs::write(
    legacy_migrations.path().join("20260820000000_initial.sql"),
    include_str!("../migrations/20260820000000_initial.sql"),
  )
  .await
  .expect("write legacy migration");
  sqlx::migrate::Migrator::new(legacy_migrations.path())
    .await
    .expect("load legacy migration")
    .run(&pool)
    .await
    .expect("apply legacy migration");
  sqlx::query("INSERT INTO runs(id, status, phase, cycle, stagnation_count, last_summary, updated_at) VALUES ('legacy-run', 'running', 'architecting', 0, 0, 'legacy', '2026-08-20T10:00:00Z')")
    .execute(&pool)
    .await
    .expect("insert legacy run");
  sqlx::query("INSERT INTO current_run(singleton, run_id) VALUES (1, 'legacy-run')")
    .execute(&pool)
    .await
    .expect("select legacy run");
  drop(pool);

  let storage = Storage::open(project.path())
    .await
    .expect("migrate existing database");
  let mut state = State::fresh();
  state.run_id = Some("legacy-run".into());
  state.status = RunStatus::ReviewRequired;
  state.phase = Phase::ReviewingRequirements;
  state.last_summary = "review".into();
  storage
    .persist_state(&state)
    .await
    .expect("persist review state");

  assert_eq!(
    storage
      .load_current_state()
      .await
      .expect("reload state")
      .status,
    RunStatus::ReviewRequired
  );
  assert_eq!(
    storage.foreign_key_check().await.expect("foreign keys"),
    DatabaseHealth::Ok
  );
}

#[tokio::test]
async fn existing_only_open_does_not_replace_a_missing_database() {
  let project = tempfile::tempdir().expect("temporary project");

  assert!(Storage::open_existing(project.path()).await.is_err());

  assert!(!project.path().join(".tenet/tenet.db").exists());
}

#[tokio::test]
async fn existing_only_open_does_not_migrate_an_empty_file() {
  let project = tempfile::tempdir().expect("temporary project");
  let directory = project.path().join(".tenet");
  tokio::fs::create_dir(&directory)
    .await
    .expect("state directory");
  let path = directory.join("tenet.db");
  tokio::fs::write(&path, [])
    .await
    .expect("empty database file");

  assert!(Storage::open_existing(project.path()).await.is_err());
  assert_eq!(tokio::fs::metadata(path).await.expect("metadata").len(), 0);
}

#[tokio::test]
async fn existing_only_open_rejects_wrong_migration_checksum() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  sqlx::query("UPDATE _sqlx_migrations SET checksum = X'00'")
    .execute(storage.pool())
    .await
    .expect("tamper migration checksum");
  drop(storage);

  assert!(Storage::open_existing(project.path()).await.is_err());
}

#[tokio::test]
async fn integrity_check_reports_healthy_database() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");

  assert_eq!(
    storage.integrity_check().await.expect("integrity check"),
    DatabaseHealth::Ok
  );
}

#[tokio::test]
async fn concurrent_readers_observe_committed_rows_without_lock_errors() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  sqlx::query("INSERT INTO storage_metadata(key, value) VALUES ('probe', 'committed')")
    .execute(storage.pool())
    .await
    .expect("insert probe");

  let reads = (0..16).map(|_| {
    let storage = storage.clone();
    tokio::spawn(async move {
      sqlx::query_scalar::<_, String>("SELECT value FROM storage_metadata WHERE key = 'probe'")
        .fetch_one(storage.pool())
        .await
    })
  });

  for read in reads {
    assert_eq!(
      read.await.expect("reader task").expect("read probe"),
      "committed"
    );
  }
}
