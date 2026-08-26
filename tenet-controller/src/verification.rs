//! Semantic-assessment validation belongs to controller-owned evidence admission.

use anyhow::{Context, Result};

use tenet_domain::evidence::{EvidenceGraphState, SemanticAssessmentReport};

/// Validates schema-typed assessment coverage and relationships without admitting evidence.
pub fn validate_semantic_assessment(
  graph: &EvidenceGraphState,
  report: &SemanticAssessmentReport,
) -> Result<()> {
  let mut validation = graph.clone();
  validation
    .record_semantic_assessment("validation", chrono::Utc::now(), "validation", report)
    .context("validate semantic assessment coverage and artifact references")?;
  Ok(())
}
