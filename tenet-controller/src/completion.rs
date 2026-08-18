use std::collections::BTreeSet;

use tenet_domain::{
  evidence::{
    EvidenceGraphState, EvidencePolicy, EvidenceResult, EvidenceSource, VerificationState,
  },
  ids::{CriterionId, ObligationId, RequirementId, SpecFragmentId},
  model::RequirementCatalog,
  verification::ProjectVerificationRun,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompletionBlocker {
  SpecificationCoverageIncomplete(Vec<SpecFragmentId>),
  RequirementUnverified(RequirementId),
  CriterionUnverified(CriterionId),
  SemanticGap(ObligationId),
  SemanticUncertain(ObligationId),
  ProjectVerificationFailed,
  ProjectVerificationStale,
  RepositoryChangedAfterVerification,
  RepositoryDirty,
  ActiveLease,
  PendingIntegration,
}

impl std::fmt::Display for CompletionBlocker {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::SpecificationCoverageIncomplete(ids) => write!(
        formatter,
        "specification coverage is incomplete ({})",
        ids
          .iter()
          .map(ToString::to_string)
          .collect::<Vec<_>>()
          .join(", ")
      ),
      Self::RequirementUnverified(id) => write!(formatter, "requirement {id} is not verified"),
      Self::CriterionUnverified(id) => {
        write!(formatter, "acceptance criterion {id} is not verified")
      }
      Self::SemanticGap(id) => write!(formatter, "semantic assessment found a gap for {id}"),
      Self::SemanticUncertain(id) => {
        write!(formatter, "semantic assessment is uncertain for {id}")
      }
      Self::ProjectVerificationFailed => formatter.write_str("project verification failed"),
      Self::ProjectVerificationStale => {
        formatter.write_str("project verification does not match the current revision and suite")
      }
      Self::RepositoryChangedAfterVerification => {
        formatter.write_str("repository changed after final verification")
      }
      Self::RepositoryDirty => formatter.write_str("repository is dirty"),
      Self::ActiveLease => formatter.write_str("an active work lease remains"),
      Self::PendingIntegration => formatter.write_str("an integration transaction remains"),
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionDecision {
  Done,
  NotReady(Vec<CompletionBlocker>),
}

pub struct CompletionContext<'a> {
  pub catalog: &'a RequirementCatalog,
  pub evidence: &'a EvidenceGraphState,
  pub project_verification: &'a ProjectVerificationRun,
  pub current_suite_hash: &'a str,
  pub current_revision: &'a str,
  pub repository_clean: bool,
  pub has_active_leases: bool,
  pub has_pending_integrations: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CompletionPolicy;

impl CompletionPolicy {
  pub fn evaluate(&self, context: &CompletionContext<'_>) -> CompletionDecision {
    let mut blockers = BTreeSet::new();
    if !context.catalog.coverage.is_complete()
      || context
        .catalog
        .coverage
        .validate_references(&context.catalog.requirements)
        .is_err()
    {
      blockers.insert(CompletionBlocker::SpecificationCoverageIncomplete(
        context.catalog.coverage.uncovered_fragment_ids.clone(),
      ));
    }

    let project = context.project_verification;
    if !project.passed {
      blockers.insert(CompletionBlocker::ProjectVerificationFailed);
    }
    if project.revision != context.current_revision
      || project.suite_hash != context.current_suite_hash
    {
      blockers.insert(CompletionBlocker::ProjectVerificationStale);
    }

    let policy = EvidencePolicy::new(context.current_revision, context.current_suite_hash);
    for requirement in context
      .catalog
      .requirements
      .iter()
      .filter(|requirement| requirement.required)
    {
      if context
        .evidence
        .requirement_verification_state(&requirement.id, policy)
        != Ok(VerificationState::Verified)
      {
        blockers.insert(CompletionBlocker::RequirementUnverified(
          requirement.id.clone(),
        ));
      }
    }
    for criterion in context
      .catalog
      .acceptance_criteria
      .iter()
      .filter(|criterion| criterion.mandatory)
    {
      if context
        .evidence
        .criterion_verification_state(&criterion.id, policy)
        != Ok(VerificationState::Verified)
      {
        blockers.insert(CompletionBlocker::CriterionUnverified(criterion.id.clone()));
      }
    }
    for obligation in context
      .catalog
      .verification_obligations
      .iter()
      .filter(|obligation| obligation.required)
    {
      for evidence in context.evidence.evidence.values().filter(|evidence| {
        evidence.obligation_id == obligation.id
          && evidence.revision == context.current_revision
          && evidence.validity.is_valid()
          && evidence.source == EvidenceSource::SemanticAssessment
      }) {
        match evidence.result {
          EvidenceResult::Failed => {
            blockers.insert(CompletionBlocker::SemanticGap(obligation.id.clone()));
          }
          EvidenceResult::Inconclusive => {
            blockers.insert(CompletionBlocker::SemanticUncertain(obligation.id.clone()));
          }
          EvidenceResult::Passed => {}
        }
      }
    }

    if context.current_revision != project.revision {
      blockers.insert(CompletionBlocker::RepositoryChangedAfterVerification);
    }
    if !context.repository_clean {
      blockers.insert(CompletionBlocker::RepositoryDirty);
    }
    if context.has_active_leases {
      blockers.insert(CompletionBlocker::ActiveLease);
    }
    if context.has_pending_integrations {
      blockers.insert(CompletionBlocker::PendingIntegration);
    }

    if blockers.is_empty() {
      CompletionDecision::Done
    } else {
      CompletionDecision::NotReady(blockers.into_iter().collect())
    }
  }
}

#[cfg(test)]
mod tests {
  use chrono::{TimeZone, Utc};
  use tenet_domain::{
    evidence::{
      AcceptanceCriterion, ObligationAssessment, ObligationAssessmentResult,
      SemanticAssessmentReport, VerificationObligation,
    },
    ids::{CriterionId, ObligationId, RequirementId, VerificationRunId},
    model::Requirement,
    verification::{CommandResult, ProjectCheckResult, VerificationSpec},
    worker::{derive_normative_fragments, CatalogCoverage},
  };

  use super::*;

  fn catalog() -> RequirementCatalog {
    let specification = "Required behavior";
    let fragments = derive_normative_fragments(specification);
    let requirements = vec![Requirement {
      id: RequirementId::from("REQ-001"),
      title: "Required behavior".into(),
      description: "Required behavior".into(),
      required: true,
      source_refs: vec![fragments[0].reference()],
    }];
    RequirementCatalog {
      spec_hash: "spec".into(),
      coverage: CatalogCoverage::derive(specification, &requirements),
      requirements,
      acceptance_criteria: vec![AcceptanceCriterion {
        id: CriterionId::from("REQ-001/AC-01"),
        requirement_id: RequirementId::from("REQ-001"),
        description: "Observable".into(),
        mandatory: true,
      }],
      verification_obligations: vec![VerificationObligation {
        id: ObligationId::from("REQ-001/AC-01/VO-01"),
        criterion_id: CriterionId::from("REQ-001/AC-01"),
        description: "Behavior is present".into(),
        required: true,
      }],
    }
  }

  fn project(passed: bool) -> ProjectVerificationRun {
    let now = Utc.with_ymd_and_hms(2026, 8, 18, 10, 0, 0).unwrap();
    ProjectVerificationRun {
      run_id: VerificationRunId::new(),
      revision: "abc".into(),
      suite_hash: "suite".into(),
      checks: vec![ProjectCheckResult {
        name: "quality".into(),
        spec: VerificationSpec {
          program: "true".into(),
          args: Vec::new(),
          working_directory: ".".into(),
          environment: Default::default(),
        },
        timeout_secs: 10,
        result: CommandResult {
          command: "true".into(),
          exit_code: Some(if passed { 0 } else { 1 }),
          timed_out: false,
          duration_ms: 1,
          stdout: String::new(),
          stderr: String::new(),
        },
      }],
      passed,
      started_at: now,
      finished_at: now,
    }
  }

  fn semantic(outcome: ObligationAssessment) -> SemanticAssessmentReport {
    SemanticAssessmentReport {
      summary: "assessment".into(),
      assessments: vec![ObligationAssessmentResult {
        obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
        assessment: outcome,
      }],
    }
  }

  fn decision(project_passed: bool, outcome: Option<ObligationAssessment>) -> CompletionDecision {
    let catalog = catalog();
    let project = project(project_passed);
    let mut graph = crate::evidence::graph_from_catalog(&catalog).expect("graph");
    graph.record_project_verification(&project);
    if let Some(outcome) = outcome {
      graph
        .record_semantic_assessment("abc", project.finished_at, "assess", &semantic(outcome))
        .expect("assessment");
    }
    CompletionPolicy.evaluate(&CompletionContext {
      catalog: &catalog,
      evidence: &graph,
      project_verification: &project,
      current_suite_hash: "suite",
      current_revision: "abc",
      repository_clean: true,
      has_active_leases: false,
      has_pending_integrations: false,
    })
  }

  #[test]
  fn final_completion_requires_both_layers() {
    assert!(matches!(
      decision(true, None),
      CompletionDecision::NotReady(_)
    ));
  }

  #[test]
  fn project_pass_and_semantic_satisfaction_yield_done() {
    assert_eq!(
      decision(
        true,
        Some(ObligationAssessment::Satisfied {
          rationale: "Satisfied".into(),
          evidence_refs: Vec::new(),
        })
      ),
      CompletionDecision::Done
    );
  }

  #[test]
  fn semantic_satisfaction_does_not_override_project_failure() {
    assert!(matches!(
      decision(
        false,
        Some(ObligationAssessment::Satisfied {
          rationale: "Satisfied".into(),
          evidence_refs: Vec::new(),
        })
      ),
      CompletionDecision::NotReady(blockers)
        if blockers.contains(&CompletionBlocker::ProjectVerificationFailed)
    ));
  }

  #[test]
  fn semantic_gap_blocks_completion_when_project_passes() {
    assert!(matches!(
      decision(
        true,
        Some(ObligationAssessment::Gap {
          description: "Missing behavior".into(),
        })
      ),
      CompletionDecision::NotReady(blockers)
        if blockers.contains(&CompletionBlocker::SemanticGap(
          ObligationId::from("REQ-001/AC-01/VO-01")
        ))
    ));
  }

  #[test]
  fn semantic_uncertainty_blocks_completion() {
    assert!(matches!(
      decision(
        true,
        Some(ObligationAssessment::Uncertain {
          reason: "Specification ambiguous".into(),
          specification_ambiguous: true,
        })
      ),
      CompletionDecision::NotReady(blockers)
        if blockers.contains(&CompletionBlocker::SemanticUncertain(
          ObligationId::from("REQ-001/AC-01/VO-01")
        ))
    ));
  }
}
