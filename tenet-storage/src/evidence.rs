use chrono::{DateTime, Utc};
use sqlx::Row;

use tenet_domain::{
  evidence::{
    Evidence, EvidenceGraphState, EvidenceProvenance, EvidenceResult, EvidenceSource,
    EvidenceValidity, ProjectEvidenceProvenance, ProjectVerificationEvidence,
    SemanticAssessmentReport,
  },
  ids::{EvidenceId, ObligationId, VerificationRunId},
  model::RequirementCatalog,
  verification::{CommandResult, ProjectCheckResult, ProjectVerificationRun, VerificationSpec},
};

use crate::{Storage, StorageError};

impl Storage {
  /// Creates and selects a run identity for controller-owned state.
  pub async fn create_run(&self, run_id: &str) -> Result<(), StorageError> {
    let mut transaction = self.pool.begin().await.map_err(StorageError::from_sqlx)?;
    sqlx::query("INSERT INTO runs(id, status, phase, cycle, stagnation_count, last_summary, updated_at) VALUES (?, 'running', 'initialized', 0, 0, '', ?) ON CONFLICT(id) DO NOTHING")
      .bind(run_id)
      .bind(Utc::now().to_rfc3339())
      .execute(&mut *transaction)
      .await
      .map_err(StorageError::from_sqlx)?;
    sqlx::query("INSERT INTO current_run(singleton, run_id) VALUES (1, ?) ON CONFLICT(singleton) DO UPDATE SET run_id = excluded.run_id")
      .bind(run_id)
      .execute(&mut *transaction)
      .await
      .map_err(StorageError::from_sqlx)?;
    transaction.commit().await.map_err(StorageError::from_sqlx)
  }

  /// Creates a historical run without changing the selected controller run.
  pub async fn create_detached_run(&self, run_id: &str) -> Result<(), StorageError> {
    sqlx::query("INSERT INTO runs(id, status, phase, cycle, stagnation_count, last_summary, updated_at) VALUES (?, 'idle', 'initialized', 0, 0, 'Detached verification', ?) ON CONFLICT(id) DO NOTHING")
      .bind(run_id)
      .bind(Utc::now().to_rfc3339())
      .execute(&self.pool)
      .await
      .map(|_| ())
      .map_err(StorageError::from_sqlx)
  }

  /// Atomically records a controller-executed project-verification run and its checks.
  pub async fn record_project_verification(
    &self,
    run_id: &str,
    verification: &ProjectVerificationRun,
  ) -> Result<(), StorageError> {
    let mut transaction = self.pool.begin().await.map_err(StorageError::from_sqlx)?;
    let verification_id = verification.run_id.to_string();
    sqlx::query("INSERT INTO project_verification_runs(id, run_id, revision, suite_hash, passed, started_at, finished_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
      .bind(&verification_id)
      .bind(run_id)
      .bind(&verification.revision)
      .bind(&verification.suite_hash)
      .bind(verification.passed)
      .bind(verification.started_at.to_rfc3339())
      .bind(verification.finished_at.to_rfc3339())
      .execute(&mut *transaction)
      .await
      .map_err(StorageError::from_sqlx)?;
    for (ordinal, check) in verification.checks.iter().enumerate() {
      let duration_ms = i64::try_from(check.result.duration_ms).map_err(|_| {
        StorageError::IntegrityViolation("verification duration exceeds SQLite INTEGER".into())
      })?;
      let timeout_secs = i64::try_from(check.timeout_secs).map_err(|_| {
        StorageError::IntegrityViolation("verification timeout exceeds SQLite INTEGER".into())
      })?;
      sqlx::query("INSERT INTO project_verification_checks(verification_run_id, ordinal, name, program, working_directory, timeout_secs, command_display, exit_code, timed_out, duration_ms, stdout, stderr) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(&verification_id)
        .bind(ordinal as i64)
        .bind(&check.name)
        .bind(&check.spec.program)
        .bind(&check.spec.working_directory)
        .bind(timeout_secs)
        .bind(&check.result.command)
        .bind(check.result.exit_code)
        .bind(check.result.timed_out)
        .bind(duration_ms)
        .bind(&check.result.stdout)
        .bind(&check.result.stderr)
        .execute(&mut *transaction)
        .await
        .map_err(StorageError::from_sqlx)?;
      for (argument_ordinal, argument) in check.spec.args.iter().enumerate() {
        sqlx::query("INSERT INTO project_verification_check_args(verification_run_id, check_ordinal, ordinal, argument) VALUES (?, ?, ?, ?)")
          .bind(&verification_id)
          .bind(ordinal as i64)
          .bind(argument_ordinal as i64)
          .bind(argument)
          .execute(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      }
      for (name, value) in &check.spec.environment {
        sqlx::query("INSERT INTO project_verification_check_environment(verification_run_id, check_ordinal, name, value) VALUES (?, ?, ?, ?)")
          .bind(&verification_id)
          .bind(ordinal as i64)
          .bind(name)
          .bind(value)
          .execute(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      }
    }
    transaction.commit().await.map_err(StorageError::from_sqlx)
  }

  /// Validates and atomically establishes obligation-bound semantic evidence.
  pub async fn record_semantic_assessment(
    &self,
    run_id: &str,
    revision: &str,
    observed_at: DateTime<Utc>,
    worker_id: &str,
    report: &SemanticAssessmentReport,
  ) -> Result<Vec<EvidenceId>, StorageError> {
    let catalog = self.load_active_catalog().await?.ok_or_else(|| {
      StorageError::UnexpectedCardinality("semantic assessment requires an active catalog".into())
    })?;
    let mut graph = empty_graph(&catalog)?;
    let ids = graph
      .record_semantic_assessment(revision, observed_at, worker_id, report)
      .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
    let mut transaction = self.pool.begin().await.map_err(StorageError::from_sqlx)?;
    for id in &ids {
      let evidence = &graph.evidence[id];
      insert_evidence(&mut transaction, run_id, evidence).await?;
    }
    transaction
      .commit()
      .await
      .map_err(StorageError::from_sqlx)?;
    Ok(ids)
  }

  /// Conservatively marks valid semantic evidence from older revisions stale.
  pub async fn invalidate_evidence_for_revision(
    &self,
    run_id: &str,
    revision: &str,
    invalidated_at: DateTime<Utc>,
  ) -> Result<u64, StorageError> {
    sqlx::query("UPDATE semantic_evidence SET validity = 'stale', invalidated_at = ?, superseded_by_revision = ? WHERE run_id = ? AND validity = 'valid' AND revision <> ?")
      .bind(invalidated_at.to_rfc3339())
      .bind(revision)
      .bind(run_id)
      .bind(revision)
      .execute(&self.pool)
      .await
      .map(|result| result.rows_affected())
      .map_err(StorageError::from_sqlx)
  }

  /// Loads only valid semantic evidence for one obligation at one revision.
  pub async fn load_obligation_evidence(
    &self,
    obligation_id: &ObligationId,
    revision: &str,
  ) -> Result<Vec<Evidence>, StorageError> {
    self
      .load_evidence_rows(
        "SELECT id, requirement_id, criterion_id, obligation_id, source, result, revision, observed_at, provenance_kind, worker_id, worker_role, rationale, validity, invalidated_at, superseded_by_revision FROM semantic_evidence WHERE obligation_id = ? AND revision = ? AND validity = 'valid' ORDER BY observed_at, id",
        Some((obligation_id.as_str(), revision)),
      )
      .await
  }

  /// Reconstructs the domain evidence graph while preserving EvidencePolicy behavior.
  pub async fn load_evidence_graph(
    &self,
    catalog: &RequirementCatalog,
  ) -> Result<EvidenceGraphState, StorageError> {
    let mut graph = empty_graph(catalog)?;
    for project in self.load_project_verifications().await? {
      graph.project_evidence.insert(project.run_id, project);
    }
    for evidence in self
      .load_evidence_rows(
        "SELECT id, requirement_id, criterion_id, obligation_id, source, result, revision, observed_at, provenance_kind, worker_id, worker_role, rationale, validity, invalidated_at, superseded_by_revision FROM semantic_evidence ORDER BY observed_at, id",
        None,
      )
      .await?
    {
      graph
        .establish_evidence(evidence)
        .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
    }
    Ok(graph)
  }

  /// Synchronizes an in-memory evidence graph without rewriting unchanged history.
  pub async fn persist_evidence_graph(
    &self,
    run_id: &str,
    graph: &EvidenceGraphState,
  ) -> Result<(), StorageError> {
    for project in graph.project_evidence.values() {
      if self
        .load_project_verification(project.run_id)
        .await?
        .is_none()
      {
        self
          .record_project_verification(
            run_id,
            &ProjectVerificationRun {
              run_id: project.run_id,
              revision: project.revision.clone(),
              suite_hash: project.suite_hash.clone(),
              checks: project.check_results.clone(),
              passed: project.result == EvidenceResult::Passed,
              started_at: project.observed_at,
              finished_at: project.observed_at,
            },
          )
          .await?;
      }
    }
    let mut transaction = self.pool.begin().await.map_err(StorageError::from_sqlx)?;
    for evidence in graph.evidence.values() {
      let exists =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM semantic_evidence WHERE id = ?")
          .bind(evidence.id.to_string())
          .fetch_one(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      if exists == 0 {
        insert_evidence(&mut transaction, run_id, evidence).await?;
      } else {
        let (validity, invalidated_at, superseded_by_revision) = match &evidence.validity {
          EvidenceValidity::Valid => ("valid", None, None),
          EvidenceValidity::Stale {
            invalidated_at,
            superseded_by_revision,
          } => (
            "stale",
            Some(invalidated_at.to_rfc3339()),
            Some(superseded_by_revision.clone()),
          ),
        };
        sqlx::query("UPDATE semantic_evidence SET validity = ?, invalidated_at = ?, superseded_by_revision = ? WHERE id = ?")
          .bind(validity)
          .bind(invalidated_at)
          .bind(superseded_by_revision)
          .bind(evidence.id.to_string())
          .execute(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      }
    }
    transaction.commit().await.map_err(StorageError::from_sqlx)
  }

  /// Loads one controller verification run by stable identity.
  pub async fn load_project_verification(
    &self,
    verification_run_id: VerificationRunId,
  ) -> Result<Option<ProjectVerificationRun>, StorageError> {
    let id = verification_run_id.to_string();
    let row = sqlx::query("SELECT revision, suite_hash, passed, started_at, finished_at FROM project_verification_runs WHERE id = ?")
      .bind(&id)
      .fetch_optional(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    let Some(row) = row else {
      return Ok(None);
    };
    Ok(Some(ProjectVerificationRun {
      run_id: verification_run_id,
      revision: row.get("revision"),
      suite_hash: row.get("suite_hash"),
      checks: self.load_project_checks(&id).await?,
      passed: row.get("passed"),
      started_at: parse_timestamp(&row.get::<String, _>("started_at"))?,
      finished_at: parse_timestamp(&row.get::<String, _>("finished_at"))?,
    }))
  }

  async fn load_project_verifications(
    &self,
  ) -> Result<Vec<ProjectVerificationEvidence>, StorageError> {
    let rows = sqlx::query("SELECT id, revision, suite_hash, passed, finished_at FROM project_verification_runs ORDER BY finished_at, id")
      .fetch_all(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    let mut evidence = Vec::with_capacity(rows.len());
    for row in rows {
      let id = row.get::<String, _>("id");
      let checks = self.load_project_checks(&id).await?;
      evidence.push(ProjectVerificationEvidence {
        run_id: parse_uuid_id(&id)?,
        revision: row.get("revision"),
        suite_hash: row.get("suite_hash"),
        result: if row.get::<bool, _>("passed") {
          EvidenceResult::Passed
        } else {
          EvidenceResult::Failed
        },
        check_results: checks,
        observed_at: parse_timestamp(&row.get::<String, _>("finished_at"))?,
        source: EvidenceSource::ProjectVerification,
        provenance: ProjectEvidenceProvenance::ControllerExecution,
      });
    }
    Ok(evidence)
  }

  async fn load_project_checks(
    &self,
    verification_id: &str,
  ) -> Result<Vec<ProjectCheckResult>, StorageError> {
    let rows = sqlx::query("SELECT ordinal, name, program, working_directory, timeout_secs, command_display, exit_code, timed_out, duration_ms, stdout, stderr FROM project_verification_checks WHERE verification_run_id = ? ORDER BY ordinal")
      .bind(verification_id)
      .fetch_all(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    let mut checks = Vec::with_capacity(rows.len());
    for row in rows {
      let ordinal = row.get::<i64, _>("ordinal");
      let args = sqlx::query_scalar::<_, String>("SELECT argument FROM project_verification_check_args WHERE verification_run_id = ? AND check_ordinal = ? ORDER BY ordinal")
        .bind(verification_id)
        .bind(ordinal)
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from_sqlx)?;
      let environment = sqlx::query("SELECT name, value FROM project_verification_check_environment WHERE verification_run_id = ? AND check_ordinal = ? ORDER BY name")
        .bind(verification_id)
        .bind(ordinal)
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from_sqlx)?
        .into_iter()
        .map(|entry| (entry.get("name"), entry.get("value")))
        .collect();
      checks.push(ProjectCheckResult {
        name: row.get("name"),
        spec: VerificationSpec {
          program: row.get("program"),
          args,
          working_directory: row.get("working_directory"),
          environment,
        },
        timeout_secs: row.get::<i64, _>("timeout_secs") as u64,
        result: CommandResult {
          command: row.get("command_display"),
          exit_code: row.get("exit_code"),
          timed_out: row.get("timed_out"),
          duration_ms: row.get::<i64, _>("duration_ms") as u128,
          stdout: row.get("stdout"),
          stderr: row.get("stderr"),
        },
      });
    }
    Ok(checks)
  }

  async fn load_evidence_rows(
    &self,
    query: &'static str,
    bindings: Option<(&str, &str)>,
  ) -> Result<Vec<Evidence>, StorageError> {
    let rows = match bindings {
      Some((first, second)) => {
        sqlx::query(query)
          .bind(first)
          .bind(second)
          .fetch_all(&self.pool)
          .await
      }
      None => sqlx::query(query).fetch_all(&self.pool).await,
    }
    .map_err(StorageError::from_sqlx)?;
    let mut evidence = Vec::with_capacity(rows.len());
    for row in rows {
      let id_text = row.get::<String, _>("id");
      let references = sqlx::query_scalar::<_, String>(
        "SELECT reference FROM evidence_refs WHERE evidence_id = ? ORDER BY ordinal",
      )
      .bind(&id_text)
      .fetch_all(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
      evidence.push(Evidence {
        id: parse_uuid_id(&id_text)?,
        requirement_id: row.get::<String, _>("requirement_id").into(),
        criterion_id: row.get::<String, _>("criterion_id").into(),
        obligation_id: row.get::<String, _>("obligation_id").into(),
        source: parse_source(&row.get::<String, _>("source"))?,
        result: parse_result(&row.get::<String, _>("result"))?,
        revision: row.get("revision"),
        observed_at: parse_timestamp(&row.get::<String, _>("observed_at"))?,
        provenance: parse_provenance(
          &row.get::<String, _>("provenance_kind"),
          row.get("worker_id"),
          row.get("worker_role"),
        )?,
        rationale: row.get("rationale"),
        evidence_refs: references,
        validity: parse_validity(
          &row.get::<String, _>("validity"),
          row.get("invalidated_at"),
          row.get("superseded_by_revision"),
        )?,
      });
    }
    Ok(evidence)
  }
}

pub(crate) async fn insert_evidence(
  transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
  run_id: &str,
  evidence: &Evidence,
) -> Result<(), StorageError> {
  let (provenance_kind, worker_id, worker_role) = match &evidence.provenance {
    EvidenceProvenance::IndependentAssessment { worker_id } => {
      ("independent_assessment", Some(worker_id.as_str()), None)
    }
    EvidenceProvenance::AgentProposal { worker_role } => {
      ("agent_proposal", None, Some(worker_role.as_str()))
    }
  };
  let (validity, invalidated_at, superseded_by_revision) = match &evidence.validity {
    EvidenceValidity::Valid => ("valid", None, None),
    EvidenceValidity::Stale {
      invalidated_at,
      superseded_by_revision,
    } => (
      "stale",
      Some(invalidated_at.to_rfc3339()),
      Some(superseded_by_revision.clone()),
    ),
  };
  sqlx::query("INSERT INTO semantic_evidence(id, run_id, requirement_id, criterion_id, obligation_id, source, result, revision, observed_at, provenance_kind, worker_id, worker_role, rationale, validity, invalidated_at, superseded_by_revision) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
    .bind(evidence.id.to_string())
    .bind(run_id)
    .bind(evidence.requirement_id.as_str())
    .bind(evidence.criterion_id.as_str())
    .bind(evidence.obligation_id.as_str())
    .bind(source_name(evidence.source))
    .bind(result_name(evidence.result))
    .bind(&evidence.revision)
    .bind(evidence.observed_at.to_rfc3339())
    .bind(provenance_kind)
    .bind(worker_id)
    .bind(worker_role)
    .bind(&evidence.rationale)
    .bind(validity)
    .bind(invalidated_at)
    .bind(superseded_by_revision)
    .execute(&mut **transaction)
    .await
    .map_err(StorageError::from_sqlx)?;
  for (ordinal, reference) in evidence.evidence_refs.iter().enumerate() {
    sqlx::query("INSERT INTO evidence_refs(evidence_id, ordinal, reference) VALUES (?, ?, ?)")
      .bind(evidence.id.to_string())
      .bind(ordinal as i64)
      .bind(reference)
      .execute(&mut **transaction)
      .await
      .map_err(StorageError::from_sqlx)?;
  }
  Ok(())
}

fn empty_graph(catalog: &RequirementCatalog) -> Result<EvidenceGraphState, StorageError> {
  let mut graph = EvidenceGraphState::new(&catalog.spec_hash);
  for requirement in &catalog.requirements {
    graph.register_requirement(requirement.id.clone(), requirement.required);
  }
  for criterion in &catalog.acceptance_criteria {
    graph
      .add_criterion(criterion.clone())
      .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
  }
  for obligation in &catalog.verification_obligations {
    graph
      .add_obligation(obligation.clone())
      .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
  }
  Ok(graph)
}

fn parse_uuid_id<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, StorageError> {
  serde_json::from_value(serde_json::Value::String(value.to_owned()))
    .map_err(|error| StorageError::IntegrityViolation(error.to_string()))
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, StorageError> {
  DateTime::parse_from_rfc3339(value)
    .map(|value| value.with_timezone(&Utc))
    .map_err(|error| StorageError::IntegrityViolation(error.to_string()))
}

fn source_name(value: EvidenceSource) -> &'static str {
  match value {
    EvidenceSource::ProjectVerification => "project_verification",
    EvidenceSource::SemanticAssessment => "semantic_assessment",
    EvidenceSource::AgentSuggestion => "agent_suggestion",
  }
}

fn parse_source(value: &str) -> Result<EvidenceSource, StorageError> {
  match value {
    "project_verification" => Ok(EvidenceSource::ProjectVerification),
    "semantic_assessment" => Ok(EvidenceSource::SemanticAssessment),
    "agent_suggestion" => Ok(EvidenceSource::AgentSuggestion),
    other => Err(StorageError::IntegrityViolation(format!(
      "unknown evidence source {other}"
    ))),
  }
}

fn result_name(value: EvidenceResult) -> &'static str {
  match value {
    EvidenceResult::Passed => "passed",
    EvidenceResult::Failed => "failed",
    EvidenceResult::Inconclusive => "inconclusive",
  }
}

fn parse_result(value: &str) -> Result<EvidenceResult, StorageError> {
  match value {
    "passed" => Ok(EvidenceResult::Passed),
    "failed" => Ok(EvidenceResult::Failed),
    "inconclusive" => Ok(EvidenceResult::Inconclusive),
    other => Err(StorageError::IntegrityViolation(format!(
      "unknown evidence result {other}"
    ))),
  }
}

fn parse_provenance(
  kind: &str,
  worker_id: Option<String>,
  worker_role: Option<String>,
) -> Result<EvidenceProvenance, StorageError> {
  match (kind, worker_id, worker_role) {
    ("independent_assessment", Some(worker_id), None) => {
      Ok(EvidenceProvenance::independent_assessment(worker_id))
    }
    ("agent_proposal", None, Some(worker_role)) => {
      Ok(EvidenceProvenance::agent_proposal(worker_role))
    }
    _ => Err(StorageError::IntegrityViolation(
      "invalid evidence provenance columns".into(),
    )),
  }
}

fn parse_validity(
  validity: &str,
  invalidated_at: Option<String>,
  superseded_by_revision: Option<String>,
) -> Result<EvidenceValidity, StorageError> {
  match (validity, invalidated_at, superseded_by_revision) {
    ("valid", None, None) => Ok(EvidenceValidity::Valid),
    ("stale", Some(invalidated_at), Some(superseded_by_revision)) => Ok(EvidenceValidity::Stale {
      invalidated_at: parse_timestamp(&invalidated_at)?,
      superseded_by_revision,
    }),
    _ => Err(StorageError::IntegrityViolation(
      "invalid evidence validity columns".into(),
    )),
  }
}
