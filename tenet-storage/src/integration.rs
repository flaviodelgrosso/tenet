use sqlx::Row;

use tenet_domain::{
  ids::VerificationRunId,
  model::{IntegrationPhase, IntegrationTransaction},
};

use crate::{Storage, StorageError};

impl Storage {
  /// Durably prepares the Git/SQLite recovery record before canonical Git mutation.
  pub async fn prepare_integration(
    &self,
    transaction: &IntegrationTransaction,
  ) -> Result<(), StorageError> {
    if transaction.phase != IntegrationPhase::Prepared {
      return Err(StorageError::IntegrityViolation(
        "new integration transaction must be prepared".into(),
      ));
    }
    sqlx::query("INSERT INTO integration_transactions(id, run_id, work_unit_id, candidate_revision, old_head, new_head, phase, verification_run_id, verification_hash, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, 'prepared', ?, ?, ?, ?)")
      .bind(&transaction.id)
      .bind(&transaction.run_id)
      .bind(&transaction.work_unit.id)
      .bind(&transaction.candidate_revision)
      .bind(&transaction.old_head)
      .bind(&transaction.new_head)
      .bind(transaction.verification_run_id.to_string())
      .bind(&transaction.verification_hash)
      .bind(&transaction.created_at)
      .bind(&transaction.updated_at)
      .execute(&self.pool)
      .await
      .map(|_| ())
      .map_err(StorageError::from_sqlx)
  }

  /// Advances exactly one prepared transaction after Git reaches `new_head`.
  pub async fn mark_integration_git_committed(
    &self,
    transaction: &IntegrationTransaction,
  ) -> Result<(), StorageError> {
    if transaction.phase != IntegrationPhase::GitCommitted {
      return Err(StorageError::IntegrityViolation(
        "Git commit transition requires git_committed phase".into(),
      ));
    }
    let result = sqlx::query("UPDATE integration_transactions SET phase = 'git_committed', updated_at = ? WHERE id = ? AND run_id = ? AND phase = 'prepared' AND old_head = ? AND new_head = ? AND verification_run_id = ? AND verification_hash = ?")
      .bind(&transaction.updated_at)
      .bind(&transaction.id)
      .bind(&transaction.run_id)
      .bind(&transaction.old_head)
      .bind(&transaction.new_head)
      .bind(transaction.verification_run_id.to_string())
      .bind(&transaction.verification_hash)
      .execute(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    require_one(result.rows_affected(), "prepared integration transaction")
  }

  /// Atomically records completed work and closes a Git-committed integration transaction.
  pub async fn complete_integration(
    &self,
    transaction: &IntegrationTransaction,
    completed_at: &str,
  ) -> Result<(), StorageError> {
    let mut database = self.pool.begin().await.map_err(StorageError::from_sqlx)?;
    sqlx::query("INSERT INTO completed_work_units(run_id, work_unit_id, work_unit_json, completed_at, verification_run_id) VALUES (?, ?, ?, ?, ?) ON CONFLICT(run_id, work_unit_id, verification_run_id) DO UPDATE SET work_unit_json = excluded.work_unit_json, completed_at = excluded.completed_at")
      .bind(&transaction.run_id)
      .bind(&transaction.work_unit.id)
      .bind(serde_json::to_string(&transaction.work_unit).map_err(|error| StorageError::IntegrityViolation(error.to_string()))?)
      .bind(completed_at)
      .bind(transaction.verification_run_id.to_string())
      .execute(&mut *database)
      .await
      .map_err(StorageError::from_sqlx)?;
    let updated = sqlx::query("UPDATE integration_transactions SET phase = 'state_committed', updated_at = ? WHERE id = ? AND run_id = ? AND phase = 'git_committed' AND verification_run_id = ? AND verification_hash = ?")
      .bind(completed_at)
      .bind(&transaction.id)
      .bind(&transaction.run_id)
      .bind(transaction.verification_run_id.to_string())
      .bind(&transaction.verification_hash)
      .execute(&mut *database)
      .await
      .map_err(StorageError::from_sqlx)?;
    require_one(
      updated.rows_affected(),
      "Git-committed integration transaction",
    )?;
    database.commit().await.map_err(StorageError::from_sqlx)
  }

  /// Counts unfinished Git/SQLite integration transactions across every run.
  pub async fn unfinished_integration_count(&self) -> Result<i64, StorageError> {
    sqlx::query_scalar(
      "SELECT COUNT(*) FROM integration_transactions WHERE phase <> 'state_committed'",
    )
    .fetch_one(&self.pool)
    .await
    .map_err(StorageError::from_sqlx)
  }

  /// Reports whether any run has an unfinished Git/SQLite integration transaction.
  pub async fn has_unfinished_integration(&self) -> Result<bool, StorageError> {
    Ok(self.unfinished_integration_count().await? != 0)
  }

  /// Loads the one unfinished recovery record for a run, if present.
  pub async fn load_active_integration(
    &self,
    run_id: &str,
  ) -> Result<Option<IntegrationTransaction>, StorageError> {
    let row = sqlx::query("SELECT id, work_unit_id, candidate_revision, old_head, new_head, phase, verification_run_id, verification_hash, created_at, updated_at FROM integration_transactions WHERE run_id = ? AND phase <> 'state_committed'")
      .bind(run_id)
      .fetch_optional(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    let Some(row) = row else {
      return Ok(None);
    };
    let work_unit_id = row.get::<String, _>("work_unit_id");
    let work_unit = self
      .load_latest_reconcile_result(run_id)
      .await?
      .and_then(|round| {
        round
          .work_units
          .into_iter()
          .find(|unit| unit.id == work_unit_id)
      })
      .ok_or_else(|| {
        StorageError::IntegrityViolation(format!(
          "integration references unavailable work unit {work_unit_id}"
        ))
      })?;
    Ok(Some(IntegrationTransaction {
      version: IntegrationTransaction::VERSION,
      id: row.get("id"),
      run_id: run_id.to_owned(),
      work_unit,
      candidate_revision: row.get("candidate_revision"),
      old_head: row.get("old_head"),
      new_head: row.get("new_head"),
      phase: parse_phase(&row.get::<String, _>("phase"))?,
      verification_run_id: parse_verification_run_id(&row.get::<String, _>("verification_run_id"))?,
      verification_hash: row.get("verification_hash"),
      created_at: row.get("created_at"),
      updated_at: row.get("updated_at"),
    }))
  }

  /// Abandons a prepared transaction only while canonical Git remains at `old_head`.
  pub async fn abandon_prepared_integration(
    &self,
    transaction_id: &str,
  ) -> Result<(), StorageError> {
    let result =
      sqlx::query("DELETE FROM integration_transactions WHERE id = ? AND phase = 'prepared'")
        .bind(transaction_id)
        .execute(&self.pool)
        .await
        .map_err(StorageError::from_sqlx)?;
    require_one(result.rows_affected(), "prepared integration transaction")
  }
}

fn require_one(rows: u64, subject: &str) -> Result<(), StorageError> {
  if rows == 1 {
    Ok(())
  } else {
    Err(StorageError::UnexpectedCardinality(format!(
      "expected one {subject}, observed {rows}"
    )))
  }
}

fn parse_phase(value: &str) -> Result<IntegrationPhase, StorageError> {
  match value {
    "prepared" => Ok(IntegrationPhase::Prepared),
    "git_committed" => Ok(IntegrationPhase::GitCommitted),
    "state_committed" => Ok(IntegrationPhase::StateCommitted),
    other => Err(StorageError::IntegrityViolation(format!(
      "unknown integration phase {other}"
    ))),
  }
}

fn parse_verification_run_id(value: &str) -> Result<VerificationRunId, StorageError> {
  serde_json::from_value(serde_json::Value::String(value.to_owned()))
    .map_err(|error| StorageError::IntegrityViolation(error.to_string()))
}
