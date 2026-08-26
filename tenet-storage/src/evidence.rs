use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use sqlx::Row;

use tenet_domain::{
  evidence::{
    Evidence, EvidenceGraphState, EvidenceProvenance, EvidenceResult, EvidenceSource,
    EvidenceValidity, ProjectEvidenceProvenance, ProjectVerificationEvidence,
    SemanticAssessmentReport,
  },
  ids::{EvidenceId, ObligationId, VerificationRunId},
  model::RequirementCatalog,
  proof::{
    derive_proof_state, ArtifactAuthority, ArtifactProvenance, ArtifactValidity, EvidenceArtifact,
    EvidenceArtifactKind, ProofDerivation, ProofState,
  },
  trusted_verifier::{TrustedExecutionRecord, TrustedExecutionResult, TrustedVerificationSpec},
  verification::{CommandResult, ProjectCheckResult, ProjectVerificationRun, VerificationSpec},
};

use crate::{controller_authority_key, Storage, StorageError};

type AuthorityMac = Hmac<Sha256>;

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
  /// Persists a controller-owned trusted execution before any artifact may reference it.
  pub async fn record_trusted_execution(
    &self,
    run_id: &str,
    record: &TrustedExecutionRecord,
    spec: &TrustedVerificationSpec,
  ) -> Result<(), StorageError> {
    spec
      .validate()
      .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
    if !record.can_issue_authority(spec) {
      return Err(StorageError::IntegrityViolation(
        "trusted execution record failed authority admission".into(),
      ));
    }
    let result = match record.result {
      TrustedExecutionResult::Supports => "supports",
      TrustedExecutionResult::Contradicts { .. } => "contradicts",
      TrustedExecutionResult::TimedOut | TrustedExecutionResult::InfrastructureFailure { .. } => {
        return Err(StorageError::IntegrityViolation(
          "non-semantic trusted execution cannot issue persisted authority".into(),
        ));
      }
    };
    let authority_context_hash = self
      .authority_context_hash(record.obligation_ids.iter())
      .await?;
    let spec_json = serde_json::to_string(spec)
      .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
    let record_json = serde_json::to_string(record)
      .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
    let authority_mac = authority_mac(&[
      authority_context_hash.as_bytes(),
      run_id.as_bytes(),
      spec_json.as_bytes(),
      record_json.as_bytes(),
    ])?;
    sqlx::query("INSERT INTO trusted_verifier_executions(id, run_id, revision, verifier_name, spec_hash, isolation_policy_hash, result, spec_json, record_json, authority_context_hash, authority_mac) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
      .bind(record.id.to_string())
      .bind(run_id)
      .bind(&record.revision)
      .bind(&record.verifier_name)
      .bind(&record.spec_hash)
      .bind(&record.isolation_policy_hash)
      .bind(result)
      .bind(spec_json)
      .bind(record_json)
      .bind(authority_context_hash)
      .bind(authority_mac)
      .execute(&self.pool)
      .await
      .map(|_| ())
      .map_err(StorageError::from_sqlx)
  }
  /// Validates and atomically records advisory assessment judgments.
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
    graph
      .record_semantic_assessment(revision, observed_at, worker_id, report)
      .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
    let mut transaction = self.pool.begin().await.map_err(StorageError::from_sqlx)?;
    for assessment in &graph.assessments {
      let json = serde_json::to_string(assessment)
        .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
      sqlx::query("INSERT INTO assessment_judgments(run_id, obligation_id, revision, judgment_json, observed_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(run_id, obligation_id, revision) DO UPDATE SET judgment_json = excluded.judgment_json, observed_at = excluded.observed_at")
        .bind(run_id).bind(assessment.obligation_id.as_str()).bind(revision).bind(json).bind(observed_at.to_rfc3339())
        .execute(&mut *transaction).await.map_err(StorageError::from_sqlx)?;
    }
    transaction
      .commit()
      .await
      .map_err(StorageError::from_sqlx)?;
    Ok(Vec::new())
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
    trusted_specs: &[TrustedVerificationSpec],
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
    let (artifacts, trusted_authority_rejected) = self.load_artifacts(trusted_specs).await?;
    for artifact in artifacts {
      graph
        .establish_artifact(artifact)
        .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
    }
    graph.assessments = self.load_assessments().await?;
    let mut derivations = self.load_proof_derivations().await?;
    for derivation in derivations.values_mut() {
      let obligation = graph
        .obligations
        .get(&derivation.obligation_id)
        .ok_or_else(|| {
          StorageError::IntegrityViolation("proof derivation targets unknown obligation".into())
        })?;
      let reconstructed = derive_proof_state(
        &obligation.id,
        &obligation.evidence_contract,
        graph.artifacts.values(),
        &derivation.revision,
      );
      if reconstructed != *derivation {
        if !trusted_authority_rejected {
          return Err(StorageError::IntegrityViolation(
            "persisted proof derivation is not reconstructible from authoritative artifacts".into(),
          ));
        }
        *derivation = reconstructed;
      }
    }
    graph.proof_derivations = derivations;
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
    transaction
      .commit()
      .await
      .map_err(StorageError::from_sqlx)?;
    self.persist_proof_state(run_id, graph).await
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
  async fn load_trusted_execution(
    &self,
    id: VerificationRunId,
  ) -> Result<(TrustedExecutionRecord, TrustedVerificationSpec), StorageError> {
    let id_text = id.to_string();
    let row = sqlx::query("SELECT run_id, revision, verifier_name, spec_hash, isolation_policy_hash, result, spec_json, record_json, authority_context_hash, authority_mac FROM trusted_verifier_executions WHERE id = ?")
      .bind(&id_text)
      .fetch_optional(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?
      .ok_or_else(|| {
        StorageError::IntegrityViolation(
          "trusted artifact references an unknown controller execution".into(),
        )
      })?;
    let run_id = row.get::<String, _>("run_id");
    let spec_json = row.get::<String, _>("spec_json");
    let record_json = row.get::<String, _>("record_json");
    let spec: TrustedVerificationSpec = serde_json::from_str(&spec_json)
      .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
    let record: TrustedExecutionRecord = serde_json::from_str(&record_json)
      .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
    let authority_context_hash = self
      .authority_context_hash(record.obligation_ids.iter())
      .await?;
    if authority_context_hash != row.get::<String, _>("authority_context_hash") {
      return Err(StorageError::IntegrityViolation(
        "trusted execution authority context is stale".into(),
      ));
    }
    verify_authority_mac(
      &[
        authority_context_hash.as_bytes(),
        run_id.as_bytes(),
        spec_json.as_bytes(),
        record_json.as_bytes(),
      ],
      &row.get::<String, _>("authority_mac"),
    )?;
    let result = match &record.result {
      TrustedExecutionResult::Supports => "supports",
      TrustedExecutionResult::Contradicts { .. } => "contradicts",
      TrustedExecutionResult::TimedOut | TrustedExecutionResult::InfrastructureFailure { .. } => {
        "infrastructure"
      }
    };
    let columns_match = record.id == id
      && record.revision == row.get::<String, _>("revision")
      && record.verifier_name == row.get::<String, _>("verifier_name")
      && record.spec_hash == row.get::<String, _>("spec_hash")
      && record.isolation_policy_hash == row.get::<String, _>("isolation_policy_hash")
      && result == row.get::<String, _>("result")
      && record.can_issue_authority(&spec);
    if !columns_match {
      return Err(StorageError::IntegrityViolation(
        "trusted execution columns disagree with controller record".into(),
      ));
    }
    Ok((record, spec))
  }

  async fn load_artifacts(
    &self,
    trusted_specs: &[TrustedVerificationSpec],
  ) -> Result<(Vec<EvidenceArtifact>, bool), StorageError> {
    let rows = sqlx::query(
      "SELECT id, run_id, revision, authority, validity, artifact_json, authority_context_hash, authority_mac FROM evidence_artifacts ORDER BY id",
    )
    .fetch_all(&self.pool)
    .await
    .map_err(StorageError::from_sqlx)?;
    let mut artifacts = Vec::with_capacity(rows.len());
    let mut trusted_authority_rejected = false;
    for row in rows {
      let id = row.get::<String, _>("id");
      let artifact_json = row.get::<String, _>("artifact_json");
      let artifact: EvidenceArtifact = serde_json::from_str(&artifact_json)
        .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
      artifact
        .validate()
        .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
      if artifact.provenance == ArtifactProvenance::ControllerTrustedVerifier {
        let Some(tag) = row.get::<Option<String>, _>("authority_mac") else {
          trusted_authority_rejected = true;
          continue;
        };
        let Some(stored_context_hash) = row.get::<Option<String>, _>("authority_context_hash")
        else {
          trusted_authority_rejected = true;
          continue;
        };
        let authority_context_hash = match self
          .authority_context_hash(artifact.obligation_ids.iter())
          .await
        {
          Ok(value) => value,
          Err(_) => {
            trusted_authority_rejected = true;
            continue;
          }
        };
        let run_id = row.get::<String, _>("run_id");
        if authority_context_hash != stored_context_hash
          || verify_authority_mac(
            &[
              authority_context_hash.as_bytes(),
              run_id.as_bytes(),
              artifact_json.as_bytes(),
            ],
            &tag,
          )
          .is_err()
          || self
            .validate_artifact_issuance(&artifact, Some(trusted_specs))
            .await
            .is_err()
        {
          trusted_authority_rejected = true;
          continue;
        }
      } else {
        self
          .validate_artifact_issuance(&artifact, Some(trusted_specs))
          .await?;
      }
      let bindings: BTreeSet<_> = sqlx::query_scalar::<_, String>("SELECT obligation_id FROM artifact_obligations WHERE artifact_id = ? ORDER BY obligation_id")
        .bind(&id).fetch_all(&self.pool).await.map_err(StorageError::from_sqlx)?
        .into_iter().map(ObligationId::from).collect();
      if artifact.id.to_string() != id
        || artifact.revision != row.get::<String, _>("revision")
        || authority_name(artifact.authority) != row.get::<String, _>("authority")
        || artifact_validity_name(&artifact.validity) != row.get::<String, _>("validity")
        || artifact.obligation_ids != bindings
      {
        return Err(StorageError::IntegrityViolation(
          "evidence artifact columns or bindings disagree with authenticated payload".into(),
        ));
      }
      artifacts.push(artifact);
    }
    Ok((artifacts, trusted_authority_rejected))
  }

  async fn validate_artifact_issuance(
    &self,
    artifact: &EvidenceArtifact,
    trusted_specs: Option<&[TrustedVerificationSpec]>,
  ) -> Result<(), StorageError> {
    match (&artifact.provenance, &artifact.kind) {
      (
        ArtifactProvenance::ControllerProjectVerification,
        EvidenceArtifactKind::ProjectVerification {
          run_id,
          suite_hash,
          passed,
        },
      ) => {
        let run = self
          .load_project_verification(*run_id)
          .await?
          .ok_or_else(|| {
            StorageError::IntegrityViolation(
              "project artifact references an unknown controller verification run".into(),
            )
          })?;
        if run.revision != artifact.revision
          || run.suite_hash != *suite_hash
          || run.passed != *passed
        {
          return Err(StorageError::IntegrityViolation(
            "project artifact disagrees with controller verification run".into(),
          ));
        }
      }
      (
        ArtifactProvenance::ControllerConfiguredCheck,
        EvidenceArtifactKind::CommandExecution {
          run_id,
          check_name: Some(name),
          spec,
          result,
          ..
        },
      ) => {
        let run = self
          .load_project_verification(*run_id)
          .await?
          .ok_or_else(|| {
            StorageError::IntegrityViolation(
              "execution artifact references an unknown controller verification run".into(),
            )
          })?;
        let check = run
          .checks
          .iter()
          .find(|check| check.name == *name)
          .ok_or_else(|| {
            StorageError::IntegrityViolation(
              "execution artifact references an unknown configured check".into(),
            )
          })?;
        let matches = run.revision == artifact.revision
          && check.spec == *spec
          && check.result.command == result.command
          && check.result.exit_code == result.exit_code
          && check.result.timed_out == result.timed_out
          && check.result.duration_ms == u128::from(result.duration_ms)
          && check.result.stdout == result.stdout
          && check.result.stderr == result.stderr;
        if !matches {
          return Err(StorageError::IntegrityViolation(
            "execution artifact disagrees with controller verification observation".into(),
          ));
        }
      }
      (
        ArtifactProvenance::ControllerTrustedVerifier,
        EvidenceArtifactKind::TrustedExecution {
          run_id,
          verifier_name,
          spec_hash,
          isolation_policy_hash,
          execution_record_hash,
          attestation,
          result,
        },
      ) => {
        let (record, persisted_spec) = self.load_trusted_execution(*run_id).await?;
        let bindings: BTreeSet<_> = record.obligation_ids.iter().cloned().collect();
        let matches_record = record.revision == artifact.revision
          && record.verifier_name == *verifier_name
          && record.spec_hash == *spec_hash
          && record.isolation_policy_hash == *isolation_policy_hash
          && record.record_hash().ok().as_deref() == Some(execution_record_hash.as_str())
          && record.attestation.as_ref() == Some(attestation)
          && record.observation == *result
          && bindings == artifact.obligation_ids
          && record.can_issue_authority(&persisted_spec);
        if !matches_record {
          return Err(StorageError::IntegrityViolation(
            "trusted verifier artifact disagrees with controller execution record".into(),
          ));
        }
        if let Some(specs) = trusted_specs {
          let configured = specs.iter().find(|spec| spec.name == *verifier_name);
          if configured
            .and_then(|spec| spec.fingerprint().ok())
            .as_deref()
            != Some(spec_hash.as_str())
          {
            return Err(StorageError::IntegrityViolation(
              "trusted verifier artifact does not match the current controller registry".into(),
            ));
          }
        }
      }
      (ArtifactProvenance::ControllerHumanAttestation { .. }, _) => {
        return Err(StorageError::IntegrityViolation(
          "human attestations require a configured issuer registry".into(),
        ));
      }
      _ => {}
    }
    Ok(())
  }

  async fn load_assessments(
    &self,
  ) -> Result<Vec<tenet_domain::proof::AssessmentRecord>, StorageError> {
    sqlx::query_scalar::<_, String>(
      "SELECT judgment_json FROM assessment_judgments ORDER BY observed_at, obligation_id",
    )
    .fetch_all(&self.pool)
    .await
    .map_err(StorageError::from_sqlx)?
    .into_iter()
    .map(|json| {
      serde_json::from_str(&json)
        .map_err(|error| StorageError::IntegrityViolation(error.to_string()))
    })
    .collect()
  }

  async fn load_proof_derivations(
    &self,
  ) -> Result<std::collections::BTreeMap<ObligationId, ProofDerivation>, StorageError> {
    let rows = sqlx::query_scalar::<_, String>(
      "SELECT derivation_json FROM proof_derivations ORDER BY derived_at, obligation_id",
    )
    .fetch_all(&self.pool)
    .await
    .map_err(StorageError::from_sqlx)?;
    let mut values = std::collections::BTreeMap::new();
    for json in rows {
      let derivation: ProofDerivation = serde_json::from_str(&json)
        .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
      values.insert(derivation.obligation_id.clone(), derivation);
    }
    Ok(values)
  }

  async fn authority_context_hash<'a, I>(&self, obligation_ids: I) -> Result<String, StorageError>
  where
    I: IntoIterator<Item = &'a ObligationId>,
  {
    let catalog = self.load_active_catalog().await?.ok_or_else(|| {
      StorageError::IntegrityViolation(
        "trusted authority requires an active requirement catalog".into(),
      )
    })?;
    let catalog_hash = catalog
      .catalog_hash()
      .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
    let ids: BTreeSet<_> = obligation_ids
      .into_iter()
      .map(ObligationId::as_str)
      .collect();
    if ids.is_empty() {
      return Err(StorageError::IntegrityViolation(
        "trusted authority requires an obligation binding".into(),
      ));
    }
    let mut digest = Sha256::new();
    digest.update(b"tenet-trusted-authority-context-v1");
    digest.update((catalog_hash.len() as u64).to_be_bytes());
    digest.update(catalog_hash.as_bytes());
    for id in ids {
      let obligation = catalog
        .verification_obligations
        .iter()
        .find(|obligation| obligation.id.as_str() == id)
        .ok_or_else(|| {
          StorageError::IntegrityViolation(format!(
            "trusted authority references unknown obligation {id}"
          ))
        })?;
      let encoded = serde_json::to_vec(obligation)
        .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
      digest.update((encoded.len() as u64).to_be_bytes());
      digest.update(encoded);
    }
    Ok(
      digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect(),
    )
  }

  async fn persist_proof_state(
    &self,
    run_id: &str,
    graph: &EvidenceGraphState,
  ) -> Result<(), StorageError> {
    let mut transaction = self.pool.begin().await.map_err(StorageError::from_sqlx)?;
    for artifact in graph.artifacts.values() {
      artifact
        .validate()
        .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
      self.validate_artifact_issuance(artifact, None).await?;
      let json = serde_json::to_string(artifact)
        .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
      let authority_context_hash =
        if artifact.provenance == ArtifactProvenance::ControllerTrustedVerifier {
          Some(
            self
              .authority_context_hash(artifact.obligation_ids.iter())
              .await?,
          )
        } else {
          None
        };
      let authority_mac = authority_context_hash
        .as_ref()
        .map(|context| authority_mac(&[context.as_bytes(), run_id.as_bytes(), json.as_bytes()]))
        .transpose()?;
      sqlx::query("INSERT INTO evidence_artifacts(id, run_id, revision, authority, validity, artifact_json, authority_context_hash, authority_mac) VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET validity = excluded.validity, artifact_json = excluded.artifact_json, authority_context_hash = excluded.authority_context_hash, authority_mac = excluded.authority_mac")
        .bind(artifact.id.to_string()).bind(run_id).bind(&artifact.revision)
        .bind(authority_name(artifact.authority)).bind(artifact_validity_name(&artifact.validity)).bind(json)
        .bind(authority_context_hash).bind(authority_mac)
        .execute(&mut *transaction).await.map_err(StorageError::from_sqlx)?;
      for obligation_id in &artifact.obligation_ids {
        sqlx::query("INSERT INTO artifact_obligations(artifact_id, obligation_id) VALUES (?, ?) ON CONFLICT DO NOTHING")
          .bind(artifact.id.to_string()).bind(obligation_id.as_str())
          .execute(&mut *transaction).await.map_err(StorageError::from_sqlx)?;
      }
    }
    for assessment in &graph.assessments {
      let json = serde_json::to_string(assessment)
        .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
      sqlx::query("INSERT INTO assessment_judgments(run_id, obligation_id, revision, judgment_json, observed_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(run_id, obligation_id, revision) DO UPDATE SET judgment_json = excluded.judgment_json, observed_at = excluded.observed_at")
        .bind(run_id).bind(assessment.obligation_id.as_str()).bind(&assessment.revision).bind(json).bind(assessment.observed_at.to_rfc3339())
        .execute(&mut *transaction).await.map_err(StorageError::from_sqlx)?;
    }
    for derivation in graph.proof_derivations.values() {
      let json = serde_json::to_string(derivation)
        .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
      sqlx::query("INSERT INTO proof_derivations(run_id, obligation_id, revision, state, derivation_json, derived_at) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(run_id, obligation_id, revision) DO UPDATE SET state = excluded.state, derivation_json = excluded.derivation_json, derived_at = excluded.derived_at")
        .bind(run_id).bind(derivation.obligation_id.as_str()).bind(&derivation.revision).bind(proof_state_name(derivation.state)).bind(json).bind(Utc::now().to_rfc3339())
        .execute(&mut *transaction).await.map_err(StorageError::from_sqlx)?;
    }
    transaction.commit().await.map_err(StorageError::from_sqlx)
  }
}
fn authority_name(authority: ArtifactAuthority) -> &'static str {
  match authority {
    ArtifactAuthority::Authoritative => "authoritative",
    ArtifactAuthority::Supporting => "supporting",
    ArtifactAuthority::Advisory => "advisory",
  }
}

fn artifact_validity_name(validity: &ArtifactValidity) -> &'static str {
  match validity {
    ArtifactValidity::Valid => "valid",
    ArtifactValidity::Stale { .. } => "stale",
  }
}

fn proof_state_name(state: ProofState) -> &'static str {
  match state {
    ProofState::Proven => "proven",
    ProofState::Contradicted => "contradicted",
    ProofState::Insufficient => "insufficient",
    ProofState::Stale => "stale",
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

fn authority_mac(parts: &[&[u8]]) -> Result<String, StorageError> {
  let mut mac = AuthorityMac::new_from_slice(controller_authority_key()?)
    .expect("HMAC accepts every key length");
  for part in parts {
    mac.update(&(part.len() as u64).to_be_bytes());
    mac.update(part);
  }
  Ok(
    mac
      .finalize()
      .into_bytes()
      .iter()
      .map(|byte| format!("{byte:02x}"))
      .collect(),
  )
}

fn verify_authority_mac(parts: &[&[u8]], encoded: &str) -> Result<(), StorageError> {
  if encoded.len() != 64 {
    return Err(StorageError::IntegrityViolation(
      "controller authority authentication tag is malformed".into(),
    ));
  }
  let mut tag = [0_u8; 32];
  for (index, output) in tag.iter_mut().enumerate() {
    *output = u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16).map_err(|_| {
      StorageError::IntegrityViolation(
        "controller authority authentication tag is malformed".into(),
      )
    })?;
  }
  let mut mac = AuthorityMac::new_from_slice(controller_authority_key()?)
    .expect("HMAC accepts every key length");
  for part in parts {
    mac.update(&(part.len() as u64).to_be_bytes());
    mac.update(part);
  }
  mac.verify_slice(&tag).map_err(|_| {
    StorageError::IntegrityViolation("controller authority authentication tag is invalid".into())
  })
}
