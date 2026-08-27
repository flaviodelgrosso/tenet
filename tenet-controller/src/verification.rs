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

#[cfg(test)]
mod tests {
  use tenet_domain::{
    evidence::{
      AcceptanceCriterion, EvidenceGraphState, ObligationAssessmentResult,
      SemanticAssessmentReport, VerificationObligation,
    },
    ids::{ArtifactId, CriterionId, ObligationId, RequirementId},
    proof::{AssessmentJudgment, EvidenceContract, EvidencePredicate},
  };

  use super::*;

  fn internal_adjudication_graph() -> EvidenceGraphState {
    let mut graph = EvidenceGraphState::new("internal-adjudication");
    graph.register_requirement(RequirementId::from("REQ-001"), true);
    graph
      .add_criterion(AcceptanceCriterion {
        id: CriterionId::from("REQ-001/AC-01"),
        requirement_id: RequirementId::from("REQ-001"),
        description: "Observable behavior".into(),
        mandatory: true,
      })
      .expect("criterion");
    graph
      .add_obligation(VerificationObligation {
        id: ObligationId::from("REQ-001/AC-01/VO-01"),
        criterion_id: CriterionId::from("REQ-001/AC-01"),
        description: "Configured check is the admitted evidence producer".into(),
        required: true,
        evidence_contract: EvidenceContract::Artifact {
          predicate: EvidencePredicate::NamedProjectCheck {
            name: "quality".into(),
          },
        },
      })
      .expect("obligation");
    graph
  }

  #[test]
  fn internal_adjudication_rejects_assessor_fabricated_artifact_ids() {
    let report = SemanticAssessmentReport {
      summary: "model claims support".into(),
      assessments: vec![ObligationAssessmentResult {
        obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
        assessment: AssessmentJudgment::Supported {
          artifact_ids: vec![ArtifactId::new()],
          rationale: "fabricated reference".into(),
        },
      }],
    };

    let error = validate_semantic_assessment(&internal_adjudication_graph(), &report)
      .expect_err("assessor cannot mint artifact ids");

    assert!(format!("{error:#}").contains("unknown evidence artifact"));
  }
}
