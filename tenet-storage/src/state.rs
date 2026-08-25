use std::collections::{BTreeMap, BTreeSet};

use sqlx::{Row, Sqlite, Transaction};

use tenet_domain::{
  ids::VerificationRunId,
  model::{
    CandidateCheck, CompletedWorkUnit, DeferredCandidate, Discovery, DiscoveryRecord,
    DiscoveryStatus, IntegrationPhase, Phase, RepairProgress, RequirementCounts, RunStatus, State,
    VerificationLayers, WorkExecution, WorkLease, WorkScope, WorkStatus, WorkUnit, WorkerDiscovery,
    WorkerRole,
  },
};

use crate::{Storage, StorageError};

impl Storage {
  /// Atomically persists one complete authoritative run-state transition.
  pub async fn persist_state(&self, state: &State) -> Result<(), StorageError> {
    validate_state(state)?;
    let Some(run_id) = state.run_id.as_deref() else {
      if *state == State::fresh() {
        return Ok(());
      }
      return Err(StorageError::IntegrityViolation(
        "non-fresh state requires a run id".into(),
      ));
    };
    let mut transaction = self.pool.begin().await.map_err(StorageError::from_sqlx)?;
    sqlx::query("INSERT INTO runs(id, status, phase, cycle, stagnation_count, progress_fingerprint, last_summary, blocked_reason, last_error, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET status = excluded.status, phase = excluded.phase, cycle = excluded.cycle, stagnation_count = excluded.stagnation_count, progress_fingerprint = excluded.progress_fingerprint, last_summary = excluded.last_summary, blocked_reason = excluded.blocked_reason, last_error = excluded.last_error, updated_at = excluded.updated_at")
      .bind(run_id)
      .bind(run_status_name(&state.status))
      .bind(phase_name(&state.phase))
      .bind(i64::from(state.cycle))
      .bind(i64::from(state.stagnation_count))
      .bind(&state.progress_fingerprint)
      .bind(&state.last_summary)
      .bind(&state.blocked_reason)
      .bind(&state.last_error)
      .bind(&state.updated_at)
      .execute(&mut *transaction)
      .await
      .map_err(StorageError::from_sqlx)?;
    sqlx::query("INSERT INTO current_run(singleton, run_id) VALUES (1, ?) ON CONFLICT(singleton) DO UPDATE SET run_id = excluded.run_id")
      .bind(run_id)
      .execute(&mut *transaction)
      .await
      .map_err(StorageError::from_sqlx)?;
    sqlx::query("INSERT INTO run_projection_cache(run_id, requirement_counts_json, verification_layers_json) VALUES (?, ?, ?) ON CONFLICT(run_id) DO UPDATE SET requirement_counts_json = excluded.requirement_counts_json, verification_layers_json = excluded.verification_layers_json")
      .bind(run_id)
      .bind(serde_json::to_string(&state.requirement_counts).map_err(serialization_error)?)
      .bind(serde_json::to_string(&state.verification_layers).map_err(serialization_error)?)
      .execute(&mut *transaction)
      .await
      .map_err(StorageError::from_sqlx)?;

    sqlx::query("UPDATE work_units SET status = 'pending' WHERE run_id = ?")
      .bind(run_id)
      .execute(&mut *transaction)
      .await
      .map_err(StorageError::from_sqlx)?;
    for (work_unit_id, status) in &state.work_statuses {
      let result = sqlx::query("UPDATE work_units SET status = ? WHERE run_id = ? AND id = ?")
        .bind(work_status_name(status))
        .bind(run_id)
        .bind(work_unit_id)
        .execute(&mut *transaction)
        .await
        .map_err(StorageError::from_sqlx)?;
      if result.rows_affected() != 1 {
        return Err(StorageError::IntegrityViolation(format!(
          "work status references unknown unit {work_unit_id}"
        )));
      }
    }
    let candidate_owners: BTreeMap<String, String> =
      sqlx::query("SELECT candidate_revision, run_id FROM candidates")
        .fetch_all(&mut *transaction)
        .await
        .map_err(StorageError::from_sqlx)?
        .into_iter()
        .map(|row| (row.get("candidate_revision"), row.get("run_id")))
        .collect();

    for statement in [
      "DELETE FROM discoveries WHERE run_id = ?",
      "DELETE FROM repair_progress WHERE run_id = ?",
    ] {
      sqlx::query(statement)
        .bind(run_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
          StorageError::IntegrityViolation(format!(
            "clear current-run mutable state with `{statement}`: {error}"
          ))
        })?;
    }
    sqlx::query("DELETE FROM candidates")
      .execute(&mut *transaction)
      .await
      .map_err(|error| {
        StorageError::IntegrityViolation(format!("clear candidate projection: {error}"))
      })?;
    sqlx::query("DELETE FROM leases")
      .execute(&mut *transaction)
      .await
      .map_err(|error| {
        StorageError::IntegrityViolation(format!("clear lease projection: {error}"))
      })?;
    sqlx::query("DELETE FROM completed_work_units")
      .execute(&mut *transaction)
      .await
      .map_err(|error| {
        StorageError::IntegrityViolation(format!("clear completion projection: {error}"))
      })?;
    for lease in state.active_leases.values() {
      insert_lease(&mut transaction, run_id, lease, true).await?;
    }
    let deferred_revisions: BTreeSet<_> = state
      .deferred_candidates
      .iter()
      .map(|candidate| candidate.candidate_revision.as_str())
      .collect();
    for candidate in &state.candidate_integrations {
      if !deferred_revisions.contains(candidate.candidate_revision.as_str()) {
        let owner_run_id = match candidate_owners.get(&candidate.candidate_revision) {
          Some(owner) => owner.clone(),
          None => {
            candidate_owner_run(&mut transaction, run_id, &candidate.lease.work_unit.id).await?
          }
        };
        insert_lease(&mut transaction, &owner_run_id, &candidate.lease, false).await?;
        insert_execution_candidate(&mut transaction, &owner_run_id, candidate).await?;
      }
    }
    for candidate in &state.deferred_candidates {
      let owner_run_id = match candidate_owners.get(&candidate.candidate_revision) {
        Some(owner) => owner.clone(),
        None => {
          candidate_owner_run(&mut transaction, run_id, &candidate.lease.work_unit.id).await?
        }
      };
      insert_lease(&mut transaction, &owner_run_id, &candidate.lease, false).await?;
      insert_deferred_candidate(&mut transaction, &owner_run_id, candidate).await?;
    }
    if let Some(repair) = &state.current_repair {
      sqlx::query("INSERT INTO repair_progress(run_id, work_unit_id, attempt) VALUES (?, ?, ?)")
        .bind(run_id)
        .bind(&repair.work_unit_id)
        .bind(i64::from(repair.attempt))
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
          StorageError::IntegrityViolation(format!(
            "persist repair progress for {}: {error}",
            repair.work_unit_id
          ))
        })?;
    }
    for discovery in &state.discoveries {
      let owner_run_id = match sqlx::query_scalar::<_, String>(
        "SELECT run_id FROM discoveries WHERE fingerprint = ?",
      )
      .bind(&discovery.fingerprint)
      .fetch_optional(&mut *transaction)
      .await
      .map_err(StorageError::from_sqlx)?
      {
        Some(owner_run_id) => owner_run_id,
        None => run_id.to_owned(),
      };
      sqlx::query("DELETE FROM discoveries WHERE fingerprint = ?")
        .bind(&discovery.fingerprint)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
          StorageError::IntegrityViolation(format!(
            "replace discovery {}: {error}",
            discovery.fingerprint
          ))
        })?;
      let (kind, reason) = discovery_kind_reason(&discovery.discovery);
      sqlx::query("INSERT INTO discoveries(fingerprint, run_id, catalog_hash, repository_revision, work_unit_id, role, cycle, kind, status, reason, payload_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(&discovery.fingerprint)
        .bind(owner_run_id)
        .bind(&discovery.catalog_hash)
        .bind(&discovery.repository_revision)
        .bind(&discovery.work_unit_id)
        .bind(discovery.role.as_str())
        .bind(i64::from(discovery.cycle))
        .bind(kind)
        .bind(discovery_status_name(&discovery.status))
        .bind(reason)
        .bind(serde_json::to_string(&discovery.discovery).map_err(serialization_error)?)
        .execute(&mut *transaction)
        .await
        .map_err(|error| StorageError::IntegrityViolation(format!("persist discovery {}: {error}", discovery.fingerprint)))?;
    }
    for completed in &state.completed_work_units {
      let owner_run_id = sqlx::query_scalar::<_, String>(
        "SELECT run_id FROM project_verification_runs WHERE id = ?",
      )
      .bind(completed.verification_run_id.to_string())
      .fetch_optional(&mut *transaction)
      .await
      .map_err(StorageError::from_sqlx)?
      .ok_or_else(|| {
        StorageError::IntegrityViolation(format!(
          "completion references unavailable verification run {}",
          completed.verification_run_id
        ))
      })?;
      sqlx::query("INSERT INTO completed_work_units(run_id, work_unit_id, work_unit_json, completed_at, verification_run_id) VALUES (?, ?, ?, ?, ?)")
        .bind(owner_run_id)
        .bind(&completed.work_unit.id)
        .bind(serde_json::to_string(&completed.work_unit).map_err(serialization_error)?)
        .bind(&completed.completed_at)
        .bind(completed.verification_run_id.to_string())
        .execute(&mut *transaction)
        .await
        .map_err(|error| StorageError::IntegrityViolation(format!("persist completion for {}: {error}", completed.work_unit.id)))?;
    }
    transaction.commit().await.map_err(StorageError::from_sqlx)
  }

  /// Reconstructs the explicitly selected current run projection.
  pub async fn load_current_state(&self) -> Result<State, StorageError> {
    let row = sqlx::query(
      "SELECT r.* FROM current_run AS c JOIN runs AS r ON r.id = c.run_id WHERE c.singleton = 1",
    )
    .fetch_optional(&self.pool)
    .await
    .map_err(StorageError::from_sqlx)?;
    let Some(row) = row else {
      return Ok(State::fresh());
    };
    let run_id = row.get::<String, _>("id");
    let projection = sqlx::query("SELECT requirement_counts_json, verification_layers_json FROM run_projection_cache WHERE run_id = ?")
      .bind(&run_id)
      .fetch_optional(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    let (requirement_counts, verification_layers) = match projection {
      Some(projection) => (
        serde_json::from_str::<RequirementCounts>(
          &projection.get::<String, _>("requirement_counts_json"),
        )
        .map_err(serialization_error)?,
        serde_json::from_str::<VerificationLayers>(
          &projection.get::<String, _>("verification_layers_json"),
        )
        .map_err(serialization_error)?,
      ),
      None => (RequirementCounts::default(), VerificationLayers::default()),
    };
    let work_units = self.load_work_units_by_id(&run_id).await?;
    let status_rows = sqlx::query("SELECT id, status FROM work_units WHERE run_id = ? ORDER BY id")
      .bind(&run_id)
      .fetch_all(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    let work_statuses = status_rows
      .into_iter()
      .filter_map(|status| {
        let value = status.get::<String, _>("status");
        (value != "pending")
          .then(|| parse_work_status(&value).map(|parsed| (status.get::<String, _>("id"), parsed)))
      })
      .collect::<Result<BTreeMap<_, _>, _>>()?;
    let leases = self.load_leases(&run_id, &work_units).await?;
    let active_leases = leases
      .iter()
      .filter(|(_, (_, active))| *active)
      .map(|(id, (lease, _))| (id.clone(), lease.clone()))
      .collect();
    let (candidate_integrations, deferred_candidates) =
      self.load_candidates(&run_id, &leases).await?;
    let discoveries = self.load_discoveries(&run_id).await?;
    let completed_work_units = self.load_completed_work(&run_id, &work_units).await?;
    let repair_row =
      sqlx::query("SELECT work_unit_id, attempt FROM repair_progress WHERE run_id = ?")
        .bind(&run_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StorageError::from_sqlx)?;
    let current_repair = repair_row.map(|repair| RepairProgress {
      work_unit_id: repair.get("work_unit_id"),
      attempt: repair.get::<i64, _>("attempt") as u32,
    });
    Ok(State {
      version: State::VERSION,
      status: parse_run_status(&row.get::<String, _>("status"))?,
      phase: parse_phase(&row.get::<String, _>("phase"))?,
      run_id: Some(run_id),
      cycle: row.get::<i64, _>("cycle") as u32,
      active_leases,
      candidate_integrations,
      deferred_candidates,
      work_statuses,
      requirement_counts,
      verification_layers,
      completed_work_units,
      discoveries,
      stagnation_count: row.get::<i64, _>("stagnation_count") as u32,
      progress_fingerprint: row.get("progress_fingerprint"),
      last_summary: row.get("last_summary"),
      current_repair,
      blocked_reason: row.get("blocked_reason"),
      last_error: row.get("last_error"),
      updated_at: row.get("updated_at"),
    })
  }

  async fn load_work_units_by_id(
    &self,
    run_id: &str,
  ) -> Result<BTreeMap<String, WorkUnit>, StorageError> {
    let rows =
      sqlx::query("SELECT id, title, objective FROM work_units WHERE run_id = ? ORDER BY id")
        .bind(run_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from_sqlx)?;
    let mut units = BTreeMap::new();
    for row in rows {
      let id = row.get::<String, _>("id");
      let requirement_ids = load_unit_values(&self.pool, "SELECT requirement_id FROM work_unit_requirements WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", run_id, &id).await?.into_iter().map(Into::into).collect();
      let criterion_ids = load_unit_values(&self.pool, "SELECT criterion_id FROM work_unit_criteria WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", run_id, &id).await?.into_iter().map(Into::into).collect();
      let verification_obligation_ids = load_unit_values(&self.pool, "SELECT obligation_id FROM work_unit_obligations WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", run_id, &id).await?.into_iter().map(Into::into).collect();
      let depends_on = load_unit_values(&self.pool, "SELECT dependency_id FROM work_unit_dependencies WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", run_id, &id).await?;
      let paths = load_unit_values(&self.pool, "SELECT path FROM work_unit_scope_paths WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", run_id, &id).await?;
      let suggested_checks = sqlx::query("SELECT obligation_id, command FROM work_unit_suggested_checks WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal")
        .bind(run_id)
        .bind(&id)
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from_sqlx)?
        .into_iter()
        .map(|check| CandidateCheck {
          obligation_id: check.get::<String, _>("obligation_id").into(),
          command: check.get("command"),
        })
        .collect();
      units.insert(
        id.clone(),
        WorkUnit {
          id,
          title: row.get("title"),
          objective: row.get("objective"),
          requirement_ids,
          criterion_ids,
          verification_obligation_ids,
          suggested_checks,
          depends_on,
          scope: WorkScope { paths },
        },
      );
    }
    Ok(units)
  }

  async fn load_leases(
    &self,
    _run_id: &str,
    _units: &BTreeMap<String, WorkUnit>,
  ) -> Result<BTreeMap<String, (WorkLease, bool)>, StorageError> {
    let rows = sqlx::query("SELECT id, work_unit_json, worker_id, base_revision, workspace, issued_at, active FROM leases ORDER BY id")
      .fetch_all(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    rows
      .into_iter()
      .map(|row| {
        let id = row.get::<String, _>("id");
        Ok((
          id.clone(),
          (
            WorkLease {
              id,
              worker_id: row.get("worker_id"),
              work_unit: serde_json::from_str(&row.get::<String, _>("work_unit_json"))
                .map_err(serialization_error)?,
              base_revision: row.get("base_revision"),
              workspace: row.get::<String, _>("workspace").into(),
              issued_at: row.get("issued_at"),
            },
            row.get("active"),
          ),
        ))
      })
      .collect()
  }

  async fn load_candidates(
    &self,
    _run_id: &str,
    leases: &BTreeMap<String, (WorkLease, bool)>,
  ) -> Result<(Vec<WorkExecution>, Vec<DeferredCandidate>), StorageError> {
    let rows = sqlx::query("SELECT id, lease_id, base_revision, candidate_revision, catalog_hash, git_ref, state, worker_summary_json, verification_report_json FROM candidates ORDER BY created_at, id")
      .fetch_all(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    let mut executions = Vec::new();
    let mut deferred = Vec::new();
    for row in rows {
      let candidate_id = row.get::<String, _>("id");
      let lease_id = row.get::<String, _>("lease_id");
      let lease = leases
        .get(&lease_id)
        .map(|(lease, _)| lease.clone())
        .ok_or_else(|| {
          StorageError::IntegrityViolation(format!(
            "candidate references unavailable lease {lease_id}"
          ))
        })?;
      let changed_paths = sqlx::query_scalar::<_, String>(
        "SELECT path FROM candidate_changed_paths WHERE candidate_id = ? ORDER BY ordinal",
      )
      .bind(&candidate_id)
      .fetch_all(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
      let discovery_json = sqlx::query_scalar::<_, String>(
        "SELECT discovery_json FROM candidate_discoveries WHERE candidate_id = ? ORDER BY ordinal",
      )
      .bind(&candidate_id)
      .fetch_all(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
      let discoveries = discovery_json
        .into_iter()
        .map(|value| serde_json::from_str::<WorkerDiscovery>(&value).map_err(serialization_error))
        .collect::<Result<Vec<_>, _>>()?;
      let worker_summary = serde_json::from_str(&row.get::<String, _>("worker_summary_json"))
        .map_err(serialization_error)?;
      let base_revision = row.get("base_revision");
      let candidate_revision = row.get("candidate_revision");
      match row.get::<String, _>("state").as_str() {
        "candidate" | "integrating" => {
          let verification = row
            .get::<Option<String>, _>("verification_report_json")
            .ok_or_else(|| {
              StorageError::IntegrityViolation("active candidate has no verification report".into())
            })?;
          executions.push(WorkExecution {
            lease,
            worker_summary,
            verification: serde_json::from_str(&verification).map_err(serialization_error)?,
            base_revision,
            candidate_revision,
            changed_paths,
            discoveries,
          });
        }
        "deferred" => deferred.push(DeferredCandidate {
          lease,
          worker_summary,
          base_revision,
          candidate_revision,
          changed_paths,
          discoveries,
          catalog_hash: row.get("catalog_hash"),
          git_ref: row.get::<Option<String>, _>("git_ref").ok_or_else(|| {
            StorageError::IntegrityViolation("deferred candidate has no git ref".into())
          })?,
        }),
        state => {
          return Err(StorageError::IntegrityViolation(format!(
            "unexpected current candidate state {state}"
          )))
        }
      }
    }
    Ok((executions, deferred))
  }

  async fn load_discoveries(&self, _run_id: &str) -> Result<Vec<DiscoveryRecord>, StorageError> {
    let rows = sqlx::query("SELECT fingerprint, catalog_hash, repository_revision, work_unit_id, role, cycle, status, payload_json FROM discoveries WHERE catalog_hash = (SELECT value FROM storage_metadata WHERE key = 'active_spec_hash') ORDER BY fingerprint")
      .fetch_all(&self.pool).await.map_err(StorageError::from_sqlx)?;
    rows
      .into_iter()
      .map(|row| {
        Ok(DiscoveryRecord {
          fingerprint: row.get("fingerprint"),
          discovery: serde_json::from_str(&row.get::<String, _>("payload_json"))
            .map_err(serialization_error)?,
          catalog_hash: row.get("catalog_hash"),
          repository_revision: row.get("repository_revision"),
          work_unit_id: row.get("work_unit_id"),
          role: parse_worker_role(&row.get::<String, _>("role"))?,
          cycle: row.get::<i64, _>("cycle") as u32,
          status: parse_discovery_status(&row.get::<String, _>("status"))?,
        })
      })
      .collect()
  }

  async fn load_completed_work(
    &self,
    _run_id: &str,
    _units: &BTreeMap<String, WorkUnit>,
  ) -> Result<Vec<CompletedWorkUnit>, StorageError> {
    let rows = sqlx::query("SELECT work_unit_json, completed_at, verification_run_id FROM completed_work_units ORDER BY completed_at, run_id, work_unit_id, verification_run_id")
      .fetch_all(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    rows
      .into_iter()
      .map(|row| {
        Ok(CompletedWorkUnit {
          work_unit: serde_json::from_str(&row.get::<String, _>("work_unit_json"))
            .map_err(serialization_error)?,
          completed_at: row.get("completed_at"),
          verification_run_id: parse_verification_run_id(
            &row.get::<String, _>("verification_run_id"),
          )?,
        })
      })
      .collect()
  }
}

async fn candidate_owner_run(
  transaction: &mut Transaction<'_, Sqlite>,
  current_run_id: &str,
  work_unit_id: &str,
) -> Result<String, StorageError> {
  sqlx::query_scalar(
    "SELECT run_id FROM work_units WHERE id = ? ORDER BY (run_id = ?) DESC, rowid DESC LIMIT 1",
  )
  .bind(work_unit_id)
  .bind(current_run_id)
  .fetch_optional(&mut **transaction)
  .await
  .map_err(StorageError::from_sqlx)?
  .ok_or_else(|| {
    StorageError::IntegrityViolation(format!(
      "candidate references unavailable unit {work_unit_id}"
    ))
  })
}

async fn load_unit_values(
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

async fn insert_lease(
  transaction: &mut Transaction<'_, Sqlite>,
  run_id: &str,
  lease: &WorkLease,
  active: bool,
) -> Result<(), StorageError> {
  sqlx::query("INSERT INTO leases(id, run_id, work_unit_id, work_unit_json, worker_id, base_revision, workspace, issued_at, active) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET run_id = excluded.run_id, work_unit_id = excluded.work_unit_id, work_unit_json = excluded.work_unit_json, active = excluded.active, workspace = excluded.workspace")
    .bind(&lease.id).bind(run_id).bind(&lease.work_unit.id)
    .bind(serde_json::to_string(&lease.work_unit).map_err(serialization_error)?).bind(&lease.worker_id)
    .bind(&lease.base_revision).bind(lease.workspace.to_string_lossy().as_ref()).bind(&lease.issued_at).bind(active)
    .execute(&mut **transaction).await.map_err(|error| StorageError::IntegrityViolation(format!("persist lease {} for run {run_id}: {error}", lease.id)))?;
  Ok(())
}

async fn insert_execution_candidate(
  transaction: &mut Transaction<'_, Sqlite>,
  run_id: &str,
  candidate: &WorkExecution,
) -> Result<(), StorageError> {
  let catalog_hash = sqlx::query_scalar::<_, String>(
    "SELECT value FROM storage_metadata WHERE key = 'active_spec_hash'",
  )
  .fetch_one(&mut **transaction)
  .await
  .map_err(StorageError::from_sqlx)?;
  let id = candidate.candidate_revision.clone();
  sqlx::query("INSERT INTO candidates(id, run_id, lease_id, base_revision, candidate_revision, catalog_hash, state, worker_summary_json, verification_report_json, created_at) VALUES (?, ?, ?, ?, ?, ?, 'candidate', ?, ?, ?)")
    .bind(&id).bind(run_id).bind(&candidate.lease.id).bind(&candidate.base_revision).bind(&candidate.candidate_revision)
    .bind(catalog_hash).bind(serde_json::to_string(&candidate.worker_summary).map_err(serialization_error)?)
    .bind(serde_json::to_string(&candidate.verification).map_err(serialization_error)?).bind(&candidate.lease.issued_at)
    .execute(&mut **transaction).await.map_err(StorageError::from_sqlx)?;
  insert_candidate_children(
    transaction,
    &id,
    &candidate.changed_paths,
    &candidate.discoveries,
  )
  .await
}

async fn insert_deferred_candidate(
  transaction: &mut Transaction<'_, Sqlite>,
  run_id: &str,
  candidate: &DeferredCandidate,
) -> Result<(), StorageError> {
  let id = candidate.candidate_revision.clone();
  sqlx::query("INSERT INTO candidates(id, run_id, lease_id, base_revision, candidate_revision, catalog_hash, git_ref, state, worker_summary_json, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, 'deferred', ?, ?)")
    .bind(&id).bind(run_id).bind(&candidate.lease.id).bind(&candidate.base_revision).bind(&candidate.candidate_revision)
    .bind(&candidate.catalog_hash).bind(&candidate.git_ref).bind(serde_json::to_string(&candidate.worker_summary).map_err(serialization_error)?)
    .bind(&candidate.lease.issued_at).execute(&mut **transaction).await.map_err(|error| StorageError::IntegrityViolation(format!("persist deferred candidate {id}: {error}")))?;
  insert_candidate_children(
    transaction,
    &id,
    &candidate.changed_paths,
    &candidate.discoveries,
  )
  .await
}

async fn insert_candidate_children(
  transaction: &mut Transaction<'_, Sqlite>,
  candidate_id: &str,
  paths: &[String],
  discoveries: &[WorkerDiscovery],
) -> Result<(), StorageError> {
  for (ordinal, path) in paths.iter().enumerate() {
    sqlx::query(
      "INSERT INTO candidate_changed_paths(candidate_id, ordinal, path) VALUES (?, ?, ?)",
    )
    .bind(candidate_id)
    .bind(ordinal as i64)
    .bind(path)
    .execute(&mut **transaction)
    .await
    .map_err(StorageError::from_sqlx)?;
  }
  for (ordinal, discovery) in discoveries.iter().enumerate() {
    sqlx::query(
      "INSERT INTO candidate_discoveries(candidate_id, ordinal, discovery_json) VALUES (?, ?, ?)",
    )
    .bind(candidate_id)
    .bind(ordinal as i64)
    .bind(serde_json::to_string(discovery).map_err(serialization_error)?)
    .execute(&mut **transaction)
    .await
    .map_err(StorageError::from_sqlx)?;
  }
  Ok(())
}

fn validate_state(state: &State) -> Result<(), StorageError> {
  if state.status == RunStatus::Done && state.phase != Phase::Complete {
    return Err(StorageError::IntegrityViolation(
      "done status requires complete phase".into(),
    ));
  }
  if (state.status == RunStatus::ReviewRequired) != (state.phase == Phase::ReviewingRequirements) {
    return Err(StorageError::IntegrityViolation(
      "review-required status and reviewing-requirements phase must occur together".into(),
    ));
  }
  if state.status == RunStatus::ReviewRequired
    && (!state.active_leases.is_empty() || !state.candidate_integrations.is_empty())
  {
    return Err(StorageError::IntegrityViolation(
      "review-required state cannot contain active work".into(),
    ));
  }
  if state.status == RunStatus::Idle
    && (!state.active_leases.is_empty() || !state.candidate_integrations.is_empty())
  {
    return Err(StorageError::IntegrityViolation(
      "idle state cannot contain active work".into(),
    ));
  }
  if state.current_repair.is_some()
    && (state.status != RunStatus::Running || state.phase != Phase::Repairing)
  {
    return Err(StorageError::IntegrityViolation(
      "current repair requires running/repairing state".into(),
    ));
  }
  Ok(())
}

fn serialization_error(error: impl std::fmt::Display) -> StorageError {
  StorageError::IntegrityViolation(error.to_string())
}

fn parse_verification_run_id(value: &str) -> Result<VerificationRunId, StorageError> {
  serde_json::from_value(serde_json::Value::String(value.to_owned())).map_err(serialization_error)
}

fn run_status_name(value: &RunStatus) -> &'static str {
  match value {
    RunStatus::Idle => "idle",
    RunStatus::Running => "running",
    RunStatus::ReviewRequired => "review_required",
    RunStatus::Done => "done",
    RunStatus::Blocked => "blocked",
    RunStatus::Failed => "failed",
    RunStatus::Stopped => "stopped",
  }
}
fn parse_run_status(value: &str) -> Result<RunStatus, StorageError> {
  match value {
    "idle" => Ok(RunStatus::Idle),
    "running" => Ok(RunStatus::Running),
    "review_required" => Ok(RunStatus::ReviewRequired),
    "done" => Ok(RunStatus::Done),
    "blocked" => Ok(RunStatus::Blocked),
    "failed" => Ok(RunStatus::Failed),
    "stopped" => Ok(RunStatus::Stopped),
    other => Err(StorageError::IntegrityViolation(format!(
      "unknown run status {other}"
    ))),
  }
}
fn phase_name(value: &Phase) -> &'static str {
  match value {
    Phase::Initialized => "initialized",
    Phase::Architecting => "architecting",
    Phase::ReviewingRequirements => "reviewing_requirements",
    Phase::Reconciling => "reconciling",
    Phase::Scheduling => "scheduling",
    Phase::Implementing => "implementing",
    Phase::Verifying => "verifying",
    Phase::Repairing => "repairing",
    Phase::Integrating => "integrating",
    Phase::Assessing => "assessing",
    Phase::Complete => "complete",
  }
}
fn parse_phase(value: &str) -> Result<Phase, StorageError> {
  match value {
    "initialized" => Ok(Phase::Initialized),
    "architecting" => Ok(Phase::Architecting),
    "reviewing_requirements" => Ok(Phase::ReviewingRequirements),
    "reconciling" => Ok(Phase::Reconciling),
    "scheduling" => Ok(Phase::Scheduling),
    "implementing" => Ok(Phase::Implementing),
    "verifying" => Ok(Phase::Verifying),
    "repairing" => Ok(Phase::Repairing),
    "integrating" => Ok(Phase::Integrating),
    "assessing" => Ok(Phase::Assessing),
    "complete" => Ok(Phase::Complete),
    other => Err(StorageError::IntegrityViolation(format!(
      "unknown phase {other}"
    ))),
  }
}
fn work_status_name(value: &WorkStatus) -> &'static str {
  match value {
    WorkStatus::Pending => "pending",
    WorkStatus::Ready => "ready",
    WorkStatus::Running => "running",
    WorkStatus::Candidate => "candidate",
    WorkStatus::Integrating => "integrating",
    WorkStatus::Completed => "completed",
    WorkStatus::Failed => "failed",
    WorkStatus::Blocked => "blocked",
    WorkStatus::Invalidated => "invalidated",
  }
}
fn parse_work_status(value: &str) -> Result<WorkStatus, StorageError> {
  match value {
    "pending" => Ok(WorkStatus::Pending),
    "ready" => Ok(WorkStatus::Ready),
    "running" => Ok(WorkStatus::Running),
    "candidate" => Ok(WorkStatus::Candidate),
    "integrating" => Ok(WorkStatus::Integrating),
    "completed" => Ok(WorkStatus::Completed),
    "failed" => Ok(WorkStatus::Failed),
    "blocked" => Ok(WorkStatus::Blocked),
    "invalidated" => Ok(WorkStatus::Invalidated),
    other => Err(StorageError::IntegrityViolation(format!(
      "unknown work status {other}"
    ))),
  }
}
fn discovery_status_name(value: &DiscoveryStatus) -> &'static str {
  match value {
    DiscoveryStatus::Active => "active",
    DiscoveryStatus::Consumed => "consumed",
    DiscoveryStatus::Invalidated => "invalidated",
  }
}
pub(crate) fn parse_discovery_status(value: &str) -> Result<DiscoveryStatus, StorageError> {
  match value {
    "active" => Ok(DiscoveryStatus::Active),
    "consumed" => Ok(DiscoveryStatus::Consumed),
    "invalidated" => Ok(DiscoveryStatus::Invalidated),
    other => Err(StorageError::IntegrityViolation(format!(
      "unknown discovery status {other}"
    ))),
  }
}
pub(crate) fn parse_worker_role(value: &str) -> Result<WorkerRole, StorageError> {
  match value {
    "architect" => Ok(WorkerRole::Architect),
    "reconcile" => Ok(WorkerRole::Reconcile),
    "implement" => Ok(WorkerRole::Implement),
    "repair" => Ok(WorkerRole::Repair),
    "assess" => Ok(WorkerRole::Assess),
    other => Err(StorageError::IntegrityViolation(format!(
      "unknown worker role {other}"
    ))),
  }
}
fn discovery_kind_reason(discovery: &Discovery) -> (&'static str, Option<&str>) {
  match discovery {
    Discovery::Dependency { reason, .. } => ("dependency", Some(reason)),
    Discovery::Blocker { description } => ("blocker", Some(description)),
    Discovery::VerificationBlocker { description } => ("verification_blocker", Some(description)),
    Discovery::ScopeExpansion { reason, .. } => ("scope_expansion", Some(reason)),
  }
}

#[allow(dead_code)]
fn integration_phase_name(value: &IntegrationPhase) -> &'static str {
  match value {
    IntegrationPhase::Prepared => "prepared",
    IntegrationPhase::GitCommitted => "git_committed",
    IntegrationPhase::StateCommitted => "state_committed",
  }
}
