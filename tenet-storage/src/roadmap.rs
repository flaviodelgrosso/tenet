use sqlx::Row;
use uuid::Uuid;

use tenet_domain::{
  evidence::ImplementationState,
  ids::{CriterionId, ObligationId, RequirementId},
  model::{CandidateCheck, ReconcileResult, RequirementAssessment, WorkScope, WorkUnit},
};

use crate::{Storage, StorageError};

impl Storage {
  /// Atomically persists one reconciliation history row and its normalized work graph.
  pub async fn persist_reconcile_round(
    &self,
    run_id: &str,
    cycle: u32,
    repository_revision: &str,
    catalog_hash: &str,
    reconciliation: &ReconcileResult,
  ) -> Result<String, StorageError> {
    let round_id = Uuid::new_v4().to_string();
    let mut transaction = self.pool.begin().await.map_err(StorageError::from_sqlx)?;
    sqlx::query("INSERT INTO reconcile_rounds(id, run_id, cycle, repository_revision, catalog_hash, summary, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
      .bind(&round_id)
      .bind(run_id)
      .bind(i64::from(cycle))
      .bind(repository_revision)
      .bind(catalog_hash)
      .bind(&reconciliation.summary)
      .bind(chrono::Utc::now().to_rfc3339())
      .execute(&mut *transaction)
      .await
      .map_err(StorageError::from_sqlx)?;

    for (ordinal, assessment) in reconciliation.requirements.iter().enumerate() {
      sqlx::query("INSERT INTO requirement_assessments(reconcile_round_id, requirement_id, implementation_state, ordinal) VALUES (?, ?, ?, ?)")
        .bind(&round_id)
        .bind(assessment.requirement_id.as_str())
        .bind(implementation_state_name(assessment.implementation_state))
        .bind(ordinal as i64)
        .execute(&mut *transaction)
        .await
        .map_err(StorageError::from_sqlx)?;
      for (item_ordinal, observation) in assessment.observations.iter().enumerate() {
        sqlx::query("INSERT INTO requirement_assessment_observations(reconcile_round_id, requirement_id, ordinal, observation) VALUES (?, ?, ?, ?)")
          .bind(&round_id)
          .bind(assessment.requirement_id.as_str())
          .bind(item_ordinal as i64)
          .bind(observation)
          .execute(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      }
      for (item_ordinal, description) in assessment.missing_implementation.iter().enumerate() {
        sqlx::query("INSERT INTO requirement_assessment_missing_implementation(reconcile_round_id, requirement_id, ordinal, description) VALUES (?, ?, ?, ?)")
          .bind(&round_id)
          .bind(assessment.requirement_id.as_str())
          .bind(item_ordinal as i64)
          .bind(description)
          .execute(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      }
      for (item_ordinal, obligation_id) in assessment.missing_evidence.iter().enumerate() {
        sqlx::query("INSERT INTO requirement_assessment_missing_evidence(reconcile_round_id, requirement_id, obligation_id, ordinal) VALUES (?, ?, ?, ?)")
          .bind(&round_id)
          .bind(assessment.requirement_id.as_str())
          .bind(obligation_id.as_str())
          .bind(item_ordinal as i64)
          .execute(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      }
    }

    for (ordinal, unit) in reconciliation.work_units.iter().enumerate() {
      for statement in [
        "DELETE FROM work_unit_requirements WHERE run_id = ? AND work_unit_id = ?",
        "DELETE FROM work_unit_criteria WHERE run_id = ? AND work_unit_id = ?",
        "DELETE FROM work_unit_obligations WHERE run_id = ? AND work_unit_id = ?",
        "DELETE FROM work_unit_dependencies WHERE run_id = ? AND work_unit_id = ?",
        "DELETE FROM work_unit_scope_paths WHERE run_id = ? AND work_unit_id = ?",
        "DELETE FROM work_unit_suggested_checks WHERE run_id = ? AND work_unit_id = ?",
      ] {
        sqlx::query(statement)
          .bind(run_id)
          .bind(&unit.id)
          .execute(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      }
      sqlx::query("INSERT INTO work_units(run_id, id, reconcile_round_id, ordinal, title, objective, status) VALUES (?, ?, ?, ?, ?, ?, 'pending') ON CONFLICT(run_id, id) DO UPDATE SET reconcile_round_id = excluded.reconcile_round_id, ordinal = excluded.ordinal, title = excluded.title, objective = excluded.objective")
        .bind(run_id)
        .bind(&unit.id)
        .bind(&round_id)
        .bind(ordinal as i64)
        .bind(&unit.title)
        .bind(&unit.objective)
        .execute(&mut *transaction)
        .await
        .map_err(StorageError::from_sqlx)?;
      for (item_ordinal, requirement_id) in unit.requirement_ids.iter().enumerate() {
        sqlx::query("INSERT INTO work_unit_requirements(run_id, work_unit_id, requirement_id, ordinal) VALUES (?, ?, ?, ?)")
          .bind(run_id)
          .bind(&unit.id)
          .bind(requirement_id.as_str())
          .bind(item_ordinal as i64)
          .execute(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      }
      for (item_ordinal, criterion_id) in unit.criterion_ids.iter().enumerate() {
        sqlx::query("INSERT INTO work_unit_criteria(run_id, work_unit_id, criterion_id, ordinal) VALUES (?, ?, ?, ?)")
          .bind(run_id)
          .bind(&unit.id)
          .bind(criterion_id.as_str())
          .bind(item_ordinal as i64)
          .execute(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      }
      for (item_ordinal, obligation_id) in unit.verification_obligation_ids.iter().enumerate() {
        sqlx::query("INSERT INTO work_unit_obligations(run_id, work_unit_id, obligation_id, ordinal) VALUES (?, ?, ?, ?)")
          .bind(run_id)
          .bind(&unit.id)
          .bind(obligation_id.as_str())
          .bind(item_ordinal as i64)
          .execute(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      }
      for (item_ordinal, path) in unit.scope.paths.iter().enumerate() {
        sqlx::query("INSERT INTO work_unit_scope_paths(run_id, work_unit_id, ordinal, path) VALUES (?, ?, ?, ?)")
          .bind(run_id)
          .bind(&unit.id)
          .bind(item_ordinal as i64)
          .bind(path)
          .execute(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      }
      for (item_ordinal, check) in unit.suggested_checks.iter().enumerate() {
        sqlx::query("INSERT INTO work_unit_suggested_checks(run_id, work_unit_id, ordinal, obligation_id, command) VALUES (?, ?, ?, ?, ?)")
          .bind(run_id)
          .bind(&unit.id)
          .bind(item_ordinal as i64)
          .bind(check.obligation_id.as_str())
          .bind(&check.command)
          .execute(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      }
    }
    for unit in &reconciliation.work_units {
      for (item_ordinal, dependency_id) in unit.depends_on.iter().enumerate() {
        sqlx::query("INSERT INTO work_unit_dependencies(run_id, work_unit_id, dependency_id, ordinal) VALUES (?, ?, ?, ?)")
          .bind(run_id)
          .bind(&unit.id)
          .bind(dependency_id)
          .bind(item_ordinal as i64)
          .execute(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      }
    }
    transaction
      .commit()
      .await
      .map_err(StorageError::from_sqlx)?;
    Ok(round_id)
  }

  /// Loads the latest persisted reconciliation projection for a run.
  pub async fn load_latest_reconcile_result(
    &self,
    run_id: &str,
  ) -> Result<Option<ReconcileResult>, StorageError> {
    let round = sqlx::query(
      "SELECT id, summary FROM reconcile_rounds WHERE run_id = ? ORDER BY cycle DESC LIMIT 1",
    )
    .bind(run_id)
    .fetch_optional(&self.pool)
    .await
    .map_err(StorageError::from_sqlx)?;
    let Some(round) = round else {
      return Ok(None);
    };
    let round_id = round.get::<String, _>("id");
    let assessment_rows = sqlx::query("SELECT requirement_id, implementation_state FROM requirement_assessments WHERE reconcile_round_id = ? ORDER BY ordinal")
      .bind(&round_id)
      .fetch_all(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    let mut requirements = Vec::with_capacity(assessment_rows.len());
    for row in assessment_rows {
      let requirement_id = row.get::<String, _>("requirement_id");
      requirements.push(RequirementAssessment {
        requirement_id: RequirementId::from(requirement_id.clone()),
        implementation_state: parse_implementation_state(&row.get::<String, _>("implementation_state"))?,
        observations: load_strings(
          &self.pool,
          "SELECT observation FROM requirement_assessment_observations WHERE reconcile_round_id = ? AND requirement_id = ? ORDER BY ordinal",
          &round_id,
          &requirement_id,
        ).await?,
        missing_implementation: load_strings(
          &self.pool,
          "SELECT description FROM requirement_assessment_missing_implementation WHERE reconcile_round_id = ? AND requirement_id = ? ORDER BY ordinal",
          &round_id,
          &requirement_id,
        ).await?,
        missing_evidence: load_strings(
          &self.pool,
          "SELECT obligation_id FROM requirement_assessment_missing_evidence WHERE reconcile_round_id = ? AND requirement_id = ? ORDER BY ordinal",
          &round_id,
          &requirement_id,
        ).await?.into_iter().map(ObligationId::from).collect(),
      });
    }

    let unit_rows = sqlx::query(
      "SELECT id, title, objective FROM work_units WHERE reconcile_round_id = ? ORDER BY ordinal",
    )
    .bind(&round_id)
    .fetch_all(&self.pool)
    .await
    .map_err(StorageError::from_sqlx)?;
    let mut work_units = Vec::with_capacity(unit_rows.len());
    for row in unit_rows {
      let id = row.get::<String, _>("id");
      let requirement_ids = load_unit_strings(&self.pool, "SELECT requirement_id FROM work_unit_requirements WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", run_id, &id).await?.into_iter().map(RequirementId::from).collect();
      let criterion_ids = load_unit_strings(&self.pool, "SELECT criterion_id FROM work_unit_criteria WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", run_id, &id).await?.into_iter().map(CriterionId::from).collect();
      let verification_obligation_ids = load_unit_strings(&self.pool, "SELECT obligation_id FROM work_unit_obligations WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", run_id, &id).await?.into_iter().map(ObligationId::from).collect();
      let depends_on = load_unit_strings(&self.pool, "SELECT dependency_id FROM work_unit_dependencies WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", run_id, &id).await?;
      let paths = load_unit_strings(&self.pool, "SELECT path FROM work_unit_scope_paths WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", run_id, &id).await?;
      let check_rows = sqlx::query("SELECT obligation_id, command FROM work_unit_suggested_checks WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal")
        .bind(run_id)
        .bind(&id)
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from_sqlx)?;
      let suggested_checks = check_rows
        .into_iter()
        .map(|check| CandidateCheck {
          obligation_id: ObligationId::from(check.get::<String, _>("obligation_id")),
          command: check.get("command"),
        })
        .collect();
      work_units.push(WorkUnit {
        id,
        title: row.get("title"),
        objective: row.get("objective"),
        requirement_ids,
        criterion_ids,
        verification_obligation_ids,
        suggested_checks,
        depends_on,
        scope: WorkScope { paths },
      });
    }
    Ok(Some(ReconcileResult {
      summary: round.get("summary"),
      requirements,
      work_units,
    }))
  }
}

async fn load_strings(
  pool: &sqlx::SqlitePool,
  query: &'static str,
  round_id: &str,
  requirement_id: &str,
) -> Result<Vec<String>, StorageError> {
  sqlx::query_scalar(query)
    .bind(round_id)
    .bind(requirement_id)
    .fetch_all(pool)
    .await
    .map_err(StorageError::from_sqlx)
}

async fn load_unit_strings(
  pool: &sqlx::SqlitePool,
  query: &'static str,
  run_id: &str,
  work_unit_id: &str,
) -> Result<Vec<String>, StorageError> {
  sqlx::query_scalar(query)
    .bind(run_id)
    .bind(work_unit_id)
    .fetch_all(pool)
    .await
    .map_err(StorageError::from_sqlx)
}

fn implementation_state_name(state: ImplementationState) -> &'static str {
  match state {
    ImplementationState::Present => "present",
    ImplementationState::Partial => "partial",
    ImplementationState::Absent => "absent",
    ImplementationState::Unknown => "unknown",
  }
}

fn parse_implementation_state(value: &str) -> Result<ImplementationState, StorageError> {
  match value {
    "present" => Ok(ImplementationState::Present),
    "partial" => Ok(ImplementationState::Partial),
    "absent" => Ok(ImplementationState::Absent),
    "unknown" => Ok(ImplementationState::Unknown),
    other => Err(StorageError::IntegrityViolation(format!(
      "unknown implementation state {other}"
    ))),
  }
}
