use sqlx::Row;

use tenet_domain::{
  evidence::{AcceptanceCriterion, VerificationObligation},
  ids::{CriterionId, ObligationId, RequirementId, SpecFragmentId},
  model::{
    CandidateCheck, Discovery, DiscoveryRecord, Requirement, RequirementCatalog, WorkScope,
    WorkUnit,
  },
  worker::{CatalogCoverage, SpecFragment, SpecReference},
};

use crate::{
  state::{parse_discovery_status, parse_worker_role},
  Storage, StorageError,
};

/// Controller state required by one Implement or Repair worker.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct WorkUnitContext {
  pub work_unit: WorkUnit,
  pub catalog: RequirementCatalog,
  pub discoveries: Vec<DiscoveryRecord>,
}

impl Storage {
  /// Loads only relational rows connected to one current work unit.
  pub async fn load_current_work_unit_context(
    &self,
    work_unit_id: &str,
  ) -> Result<WorkUnitContext, StorageError> {
    let run_id =
      sqlx::query_scalar::<_, String>("SELECT run_id FROM current_run WHERE singleton = 1")
        .fetch_optional(&self.pool)
        .await
        .map_err(StorageError::from_sqlx)?
        .ok_or_else(|| {
          StorageError::UnexpectedCardinality("work-unit context requires an active run".into())
        })?;
    let row = sqlx::query("SELECT title, objective FROM work_units WHERE run_id = ? AND id = ?")
      .bind(&run_id)
      .bind(work_unit_id)
      .fetch_optional(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?
      .ok_or_else(|| {
        StorageError::UnexpectedCardinality(format!("unknown current work unit {work_unit_id}"))
      })?;
    let requirement_ids = load_context_ids(&self.pool, "SELECT requirement_id FROM work_unit_requirements WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", &run_id, work_unit_id).await?;
    let criterion_ids = load_context_ids(&self.pool, "SELECT criterion_id FROM work_unit_criteria WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", &run_id, work_unit_id).await?;
    let obligation_ids = load_context_ids(&self.pool, "SELECT obligation_id FROM work_unit_obligations WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", &run_id, work_unit_id).await?;
    let depends_on = load_context_ids(&self.pool, "SELECT dependency_id FROM work_unit_dependencies WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", &run_id, work_unit_id).await?;
    let paths = load_context_ids(&self.pool, "SELECT path FROM work_unit_scope_paths WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal", &run_id, work_unit_id).await?;
    let checks = sqlx::query("SELECT obligation_id, command FROM work_unit_suggested_checks WHERE run_id = ? AND work_unit_id = ? ORDER BY ordinal")
      .bind(&run_id)
      .bind(work_unit_id)
      .fetch_all(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?
      .into_iter()
      .map(|check| CandidateCheck {
        obligation_id: ObligationId::from(check.get::<String, _>("obligation_id")),
        command: check.get("command"),
      })
      .collect();
    let work_unit = WorkUnit {
      id: work_unit_id.to_owned(),
      title: row.get("title"),
      objective: row.get("objective"),
      requirement_ids: requirement_ids
        .iter()
        .cloned()
        .map(RequirementId::from)
        .collect(),
      criterion_ids: criterion_ids
        .iter()
        .cloned()
        .map(CriterionId::from)
        .collect(),
      verification_obligation_ids: obligation_ids
        .iter()
        .cloned()
        .map(ObligationId::from)
        .collect(),
      suggested_checks: checks,
      depends_on,
      scope: WorkScope { paths },
    };

    let spec_hash = sqlx::query_scalar::<_, String>(
      "SELECT value FROM storage_metadata WHERE key = 'active_spec_hash'",
    )
    .fetch_optional(&self.pool)
    .await
    .map_err(StorageError::from_sqlx)?
    .ok_or_else(|| {
      StorageError::UnexpectedCardinality("work-unit context requires a catalog".into())
    })?;
    let requirement_rows = sqlx::query("SELECT r.id, r.title, r.description, r.required FROM work_unit_requirements AS link JOIN requirements AS r ON r.id = link.requirement_id WHERE link.run_id = ? AND link.work_unit_id = ? ORDER BY link.ordinal")
      .bind(&run_id).bind(work_unit_id).fetch_all(&self.pool).await.map_err(StorageError::from_sqlx)?;
    let mut requirements = Vec::with_capacity(requirement_rows.len());
    for requirement in requirement_rows {
      let id = requirement.get::<String, _>("id");
      let refs = sqlx::query("SELECT f.id, f.section, f.text_hash FROM requirement_source_fragments AS link JOIN spec_fragments AS f ON f.id = link.fragment_id WHERE link.requirement_id = ? ORDER BY link.ordinal")
        .bind(&id).fetch_all(&self.pool).await.map_err(StorageError::from_sqlx)?
        .into_iter().map(|reference| SpecReference {
          fragment_id: SpecFragmentId::from(reference.get::<String, _>("id")),
          section: reference.get("section"),
          text_hash: reference.get("text_hash"),
        }).collect();
      requirements.push(Requirement {
        id: RequirementId::from(id),
        title: requirement.get("title"),
        description: requirement.get("description"),
        required: requirement.get("required"),
        source_refs: refs,
      });
    }
    let acceptance_criteria = sqlx::query("SELECT criterion.id, criterion.requirement_id, criterion.description, criterion.mandatory FROM work_unit_criteria AS link JOIN acceptance_criteria AS criterion ON criterion.id = link.criterion_id WHERE link.run_id = ? AND link.work_unit_id = ? ORDER BY link.ordinal")
      .bind(&run_id).bind(work_unit_id).fetch_all(&self.pool).await.map_err(StorageError::from_sqlx)?
      .into_iter().map(|criterion| AcceptanceCriterion {
        id: CriterionId::from(criterion.get::<String, _>("id")),
        requirement_id: RequirementId::from(criterion.get::<String, _>("requirement_id")),
        description: criterion.get("description"),
        mandatory: criterion.get("mandatory"),
      }).collect();
    let verification_obligations = sqlx::query("SELECT obligation.id, obligation.criterion_id, obligation.description, obligation.required FROM work_unit_obligations AS link JOIN verification_obligations AS obligation ON obligation.id = link.obligation_id WHERE link.run_id = ? AND link.work_unit_id = ? ORDER BY link.ordinal")
      .bind(&run_id).bind(work_unit_id).fetch_all(&self.pool).await.map_err(StorageError::from_sqlx)?
      .into_iter().map(|obligation| VerificationObligation {
        id: ObligationId::from(obligation.get::<String, _>("id")),
        criterion_id: CriterionId::from(obligation.get::<String, _>("criterion_id")),
        description: obligation.get("description"),
        required: obligation.get("required"),
      }).collect();
    let normative_fragments = sqlx::query("SELECT DISTINCT fragment.id, fragment.section, fragment.text, fragment.text_hash, fragment.ordinal FROM work_unit_requirements AS work_link JOIN requirement_source_fragments AS source_link ON source_link.requirement_id = work_link.requirement_id JOIN spec_fragments AS fragment ON fragment.id = source_link.fragment_id WHERE work_link.run_id = ? AND work_link.work_unit_id = ? ORDER BY fragment.ordinal")
      .bind(&run_id).bind(work_unit_id).fetch_all(&self.pool).await.map_err(StorageError::from_sqlx)?
      .into_iter().map(|fragment| SpecFragment {
        id: SpecFragmentId::from(fragment.get::<String, _>("id")),
        section: fragment.get("section"),
        text: fragment.get("text"),
        text_hash: fragment.get("text_hash"),
      }).collect();
    let discoveries = sqlx::query("SELECT fingerprint, payload_json, catalog_hash, repository_revision, work_unit_id, role, cycle, status FROM discoveries WHERE catalog_hash = ? AND work_unit_id = ? AND status = 'active' ORDER BY fingerprint")
      .bind(&spec_hash).bind(work_unit_id).fetch_all(&self.pool).await.map_err(StorageError::from_sqlx)?
      .into_iter().map(|discovery| Ok(DiscoveryRecord {
        fingerprint: discovery.get("fingerprint"),
        discovery: serde_json::from_str::<Discovery>(&discovery.get::<String, _>("payload_json")).map_err(|error| StorageError::IntegrityViolation(error.to_string()))?,
        catalog_hash: discovery.get("catalog_hash"),
        repository_revision: discovery.get("repository_revision"),
        work_unit_id: discovery.get("work_unit_id"),
        role: parse_worker_role(&discovery.get::<String, _>("role"))?,
        cycle: discovery.get::<i64, _>("cycle") as u32,
        status: parse_discovery_status(&discovery.get::<String, _>("status"))?,
      })).collect::<Result<Vec<_>, StorageError>>()?;

    Ok(WorkUnitContext {
      work_unit,
      catalog: RequirementCatalog {
        spec_hash,
        requirements,
        acceptance_criteria,
        verification_obligations,
        coverage: CatalogCoverage {
          normative_fragments,
          uncovered_fragment_ids: Vec::new(),
        },
      },
      discoveries,
    })
  }
}

async fn load_context_ids(
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
