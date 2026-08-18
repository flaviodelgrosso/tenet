//! Semantic-assessment validation belongs to controller-owned evidence admission.

use anyhow::{Context, Result};

use tenet_domain::{
  evidence::{EvidenceGraphState, SemanticAssessmentReport},
  model::RequirementCatalog,
};

use crate::evidence;

/// Validates schema-typed assessment coverage and relationships without admitting evidence.
pub fn validate_semantic_assessment(
  catalog: &RequirementCatalog,
  report: &SemanticAssessmentReport,
) -> Result<()> {
  let mut graph = evidence::graph_from_catalog(catalog)?;
  graph
    .record_semantic_assessment("validation", chrono::Utc::now(), "validation", report)
    .context("validate semantic assessment coverage")?;
  Ok(())
}

/// Builds a fresh graph for callers that need to validate before loading persisted evidence.
pub fn validation_graph(catalog: &RequirementCatalog) -> Result<EvidenceGraphState> {
  evidence::graph_from_catalog(catalog)
}
