use chrono::{DateTime, Utc};
use sqlx::{Row, Sqlite, Transaction};

use tenet_domain::{
  evidence::{AcceptanceCriterion, VerificationObligation},
  ids::{CriterionId, ObligationId, RequirementId, SpecFragmentId},
  model::{CatalogApproval, Requirement, RequirementCatalog},
  worker::{CatalogCoverage, SpecFragment, SpecReference},
};

use crate::{Storage, StorageError};

impl Storage {
  /// Atomically replaces the active generated catalog and all normalized relationships.
  pub async fn persist_catalog(
    &self,
    source_path: &str,
    observed_at: DateTime<Utc>,
    catalog: &RequirementCatalog,
  ) -> Result<(), StorageError> {
    let catalog_hash = catalog
      .catalog_hash()
      .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
    let mut transaction = self.pool.begin().await.map_err(StorageError::from_sqlx)?;
    clear_active_catalog(&mut transaction).await?;
    sqlx::query(
      "INSERT INTO specification_snapshots(hash, source_path, observed_at) VALUES (?, ?, ?)",
    )
    .bind(&catalog.spec_hash)
    .bind(source_path)
    .bind(observed_at.to_rfc3339())
    .execute(&mut *transaction)
    .await
    .map_err(StorageError::from_sqlx)?;

    for (ordinal, fragment) in catalog.coverage.normative_fragments.iter().enumerate() {
      sqlx::query("INSERT INTO spec_fragments(id, spec_hash, ordinal, section, text, text_hash) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(fragment.id.as_str())
        .bind(&catalog.spec_hash)
        .bind(ordinal as i64)
        .bind(&fragment.section)
        .bind(&fragment.text)
        .bind(&fragment.text_hash)
        .execute(&mut *transaction)
        .await
        .map_err(StorageError::from_sqlx)?;
    }
    for (ordinal, requirement) in catalog.requirements.iter().enumerate() {
      sqlx::query("INSERT INTO requirements(id, spec_hash, ordinal, title, description, required) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(requirement.id.as_str())
        .bind(&catalog.spec_hash)
        .bind(ordinal as i64)
        .bind(&requirement.title)
        .bind(&requirement.description)
        .bind(requirement.required)
        .execute(&mut *transaction)
        .await
        .map_err(StorageError::from_sqlx)?;
      for (reference_ordinal, reference) in requirement.source_refs.iter().enumerate() {
        sqlx::query("INSERT INTO requirement_source_fragments(requirement_id, fragment_id, ordinal) VALUES (?, ?, ?)")
          .bind(requirement.id.as_str())
          .bind(reference.fragment_id.as_str())
          .bind(reference_ordinal as i64)
          .execute(&mut *transaction)
          .await
          .map_err(StorageError::from_sqlx)?;
      }
    }
    for (ordinal, criterion) in catalog.acceptance_criteria.iter().enumerate() {
      sqlx::query("INSERT INTO acceptance_criteria(id, requirement_id, ordinal, description, mandatory) VALUES (?, ?, ?, ?, ?)")
        .bind(criterion.id.as_str())
        .bind(criterion.requirement_id.as_str())
        .bind(ordinal as i64)
        .bind(&criterion.description)
        .bind(criterion.mandatory)
        .execute(&mut *transaction)
        .await
        .map_err(StorageError::from_sqlx)?;
    }
    for (ordinal, obligation) in catalog.verification_obligations.iter().enumerate() {
      sqlx::query("INSERT INTO verification_obligations(id, criterion_id, ordinal, description, required, evidence_contract_json) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(obligation.id.as_str())
        .bind(obligation.criterion_id.as_str())
        .bind(ordinal as i64)
        .bind(&obligation.description)
        .bind(obligation.required)
        .bind(serde_json::to_string(&obligation.evidence_contract).map_err(|error| StorageError::IntegrityViolation(error.to_string()))?)
        .execute(&mut *transaction)
        .await
        .map_err(StorageError::from_sqlx)?;
    }
    for (ordinal, fragment_id) in catalog.coverage.uncovered_fragment_ids.iter().enumerate() {
      sqlx::query(
        "INSERT INTO uncovered_spec_fragments(spec_hash, fragment_id, ordinal) VALUES (?, ?, ?)",
      )
      .bind(&catalog.spec_hash)
      .bind(fragment_id.as_str())
      .bind(ordinal as i64)
      .execute(&mut *transaction)
      .await
      .map_err(StorageError::from_sqlx)?;
    }
    sqlx::query("INSERT INTO storage_metadata(key, value) VALUES ('active_spec_hash', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
      .bind(&catalog.spec_hash)
      .execute(&mut *transaction)
      .await
      .map_err(StorageError::from_sqlx)?;
    sqlx::query("INSERT INTO storage_metadata(key, value) VALUES ('active_catalog_hash', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
      .bind(catalog_hash)
      .execute(&mut *transaction)
      .await
      .map_err(StorageError::from_sqlx)?;
    transaction.commit().await.map_err(StorageError::from_sqlx)
  }

  /// Loads one catalog by the exact authoritative specification hash.
  pub async fn load_catalog(
    &self,
    specification_hash: &str,
  ) -> Result<Option<RequirementCatalog>, StorageError> {
    let exists =
      sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM specification_snapshots WHERE hash = ?")
        .bind(specification_hash)
        .fetch_one(&self.pool)
        .await
        .map_err(StorageError::from_sqlx)?;
    if exists == 0 {
      return Ok(None);
    }

    let fragment_rows = sqlx::query("SELECT id, section, text, text_hash FROM spec_fragments WHERE spec_hash = ? ORDER BY ordinal")
      .bind(specification_hash)
      .fetch_all(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    let normative_fragments = fragment_rows
      .into_iter()
      .map(|row| SpecFragment {
        id: SpecFragmentId::from(row.get::<String, _>("id")),
        section: row.get("section"),
        text: row.get("text"),
        text_hash: row.get("text_hash"),
      })
      .collect();

    let requirement_rows = sqlx::query("SELECT id, title, description, required FROM requirements WHERE spec_hash = ? ORDER BY ordinal")
      .bind(specification_hash)
      .fetch_all(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    let mut requirements = Vec::with_capacity(requirement_rows.len());
    for row in requirement_rows {
      let id = row.get::<String, _>("id");
      let reference_rows = sqlx::query("SELECT f.id, f.section, f.text_hash FROM requirement_source_fragments AS r JOIN spec_fragments AS f ON f.id = r.fragment_id WHERE r.requirement_id = ? ORDER BY r.ordinal")
        .bind(&id)
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from_sqlx)?;
      let source_refs = reference_rows
        .into_iter()
        .map(|reference| SpecReference {
          fragment_id: SpecFragmentId::from(reference.get::<String, _>("id")),
          section: reference.get("section"),
          text_hash: reference.get("text_hash"),
        })
        .collect();
      requirements.push(Requirement {
        id: RequirementId::from(id),
        title: row.get("title"),
        description: row.get("description"),
        required: row.get("required"),
        source_refs,
      });
    }

    let criterion_rows = sqlx::query(
      "SELECT id, requirement_id, description, mandatory FROM acceptance_criteria ORDER BY ordinal",
    )
    .fetch_all(&self.pool)
    .await
    .map_err(StorageError::from_sqlx)?;
    let acceptance_criteria = criterion_rows
      .into_iter()
      .map(|row| AcceptanceCriterion {
        id: CriterionId::from(row.get::<String, _>("id")),
        requirement_id: RequirementId::from(row.get::<String, _>("requirement_id")),
        description: row.get("description"),
        mandatory: row.get("mandatory"),
      })
      .collect();

    let obligation_rows = sqlx::query("SELECT id, criterion_id, description, required, evidence_contract_json FROM verification_obligations ORDER BY ordinal")
      .fetch_all(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    let verification_obligations = obligation_rows
      .into_iter()
      .map(|row| {
        Ok(VerificationObligation {
          id: ObligationId::from(row.get::<String, _>("id")),
          criterion_id: CriterionId::from(row.get::<String, _>("criterion_id")),
          description: row.get("description"),
          required: row.get("required"),
          evidence_contract: serde_json::from_str(&row.get::<String, _>("evidence_contract_json"))
            .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?,
        })
      })
      .collect::<Result<Vec<_>, StorageError>>()?;
    let uncovered_fragment_ids = sqlx::query_scalar::<_, String>(
      "SELECT fragment_id FROM uncovered_spec_fragments WHERE spec_hash = ? ORDER BY ordinal",
    )
    .bind(specification_hash)
    .fetch_all(&self.pool)
    .await
    .map_err(StorageError::from_sqlx)?
    .into_iter()
    .map(SpecFragmentId::from)
    .collect();

    Ok(Some(RequirementCatalog {
      spec_hash: specification_hash.to_owned(),
      requirements,
      acceptance_criteria,
      verification_obligations,
      coverage: CatalogCoverage {
        normative_fragments,
        uncovered_fragment_ids,
      },
    }))
  }

  /// Loads the explicitly selected active catalog.
  pub async fn load_active_catalog(&self) -> Result<Option<RequirementCatalog>, StorageError> {
    let hash = sqlx::query_scalar::<_, String>(
      "SELECT value FROM storage_metadata WHERE key = 'active_spec_hash'",
    )
    .fetch_optional(&self.pool)
    .await
    .map_err(StorageError::from_sqlx)?;
    match hash {
      Some(hash) => self.load_catalog(&hash).await,
      None => Ok(None),
    }
  }

  /// Persists human approval only when it matches the exact active catalog identity.
  pub async fn persist_catalog_approval(
    &self,
    approval: &CatalogApproval,
  ) -> Result<(), StorageError> {
    let mut transaction = self.pool.begin().await.map_err(StorageError::from_sqlx)?;
    let result = sqlx::query(
      "INSERT INTO catalog_approvals(singleton, spec_hash, catalog_hash, approved_at) SELECT 1, ?, ?, ? WHERE EXISTS (SELECT 1 FROM storage_metadata AS spec, storage_metadata AS catalog WHERE spec.key = 'active_spec_hash' AND spec.value = ? AND catalog.key = 'active_catalog_hash' AND catalog.value = ?) ON CONFLICT(singleton) DO UPDATE SET spec_hash = excluded.spec_hash, catalog_hash = excluded.catalog_hash, approved_at = excluded.approved_at",
    )
    .bind(&approval.spec_hash)
    .bind(&approval.catalog_hash)
    .bind(approval.approved_at.to_rfc3339())
    .bind(&approval.spec_hash)
    .bind(&approval.catalog_hash)
    .execute(&mut *transaction)
    .await
    .map_err(StorageError::from_sqlx)?;
    if result.rows_affected() != 1 {
      return Err(StorageError::IntegrityViolation(
        "catalog approval does not match the exact active catalog".into(),
      ));
    }
    transaction.commit().await.map_err(StorageError::from_sqlx)
  }

  /// Loads the approval associated with the active catalog, if one exists.
  pub async fn load_catalog_approval(&self) -> Result<Option<CatalogApproval>, StorageError> {
    let row = sqlx::query(
      "SELECT spec_hash, catalog_hash, approved_at FROM catalog_approvals WHERE singleton = 1",
    )
    .fetch_optional(&self.pool)
    .await
    .map_err(StorageError::from_sqlx)?;
    row
      .map(|row| {
        let approved_at = row.get::<String, _>("approved_at");
        Ok(CatalogApproval {
          spec_hash: row.get("spec_hash"),
          catalog_hash: row.get("catalog_hash"),
          approved_at: DateTime::parse_from_rfc3339(&approved_at)
            .map(|value| value.with_timezone(&Utc))
            .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?,
        })
      })
      .transpose()
  }

  /// Returns whether human approval matches the exact active catalog.
  pub async fn catalog_is_approved(
    &self,
    catalog: &RequirementCatalog,
  ) -> Result<bool, StorageError> {
    let catalog_hash = catalog
      .catalog_hash()
      .map_err(|error| StorageError::IntegrityViolation(error.to_string()))?;
    Ok(self.load_catalog_approval().await?.is_some_and(|approval| {
      approval.spec_hash == catalog.spec_hash && approval.catalog_hash == catalog_hash
    }))
  }
}

async fn clear_active_catalog(
  transaction: &mut Transaction<'_, Sqlite>,
) -> Result<(), StorageError> {
  for statement in [
    "DELETE FROM catalog_approvals",
    "DELETE FROM integration_transactions",
    "DELETE FROM completed_work_units",
    "DELETE FROM candidates",
    "DELETE FROM discoveries",
    "DELETE FROM repair_progress",
    "DELETE FROM leases",
    "DELETE FROM reconcile_rounds",
    "DELETE FROM proof_derivations",
    "DELETE FROM assessment_judgments",
    "DELETE FROM evidence_artifacts",
    "DELETE FROM semantic_evidence",
    "DELETE FROM uncovered_spec_fragments",
    "DELETE FROM requirements",
    "DELETE FROM specification_snapshots",
    "DELETE FROM storage_metadata WHERE key IN ('active_spec_hash', 'active_catalog_hash')",
  ] {
    sqlx::query(statement)
      .execute(&mut **transaction)
      .await
      .map_err(StorageError::from_sqlx)?;
  }
  Ok(())
}
