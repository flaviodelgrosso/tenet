use std::collections::BTreeSet;

use tenet_domain::{
  evidence::{
    EvidenceGraphState, EvidencePolicy, EvidenceResult, EvidenceValidity, VerificationState,
  },
  ids::{CriterionId, EvidenceId, ObligationId, RequirementId, SpecFragmentId},
  model::{ReconcileResult, RequirementCatalog},
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompletionBlocker {
  SpecificationCoverageIncomplete(Vec<SpecFragmentId>),
  RequirementUnverified(RequirementId),
  CriterionUnverified(CriterionId),
  EvidenceStale(EvidenceId),
  EvidenceContradicted(ObligationId),
  UntrustedEvidence(ObligationId),
  DeterministicGateFailed,
  AssessmentFoundImplementationGap(RequirementId),
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
      Self::EvidenceStale(id) => write!(formatter, "evidence {id} is stale"),
      Self::EvidenceContradicted(id) => {
        write!(
          formatter,
          "verification obligation {id} has contradictory evidence"
        )
      }
      Self::UntrustedEvidence(id) => write!(
        formatter,
        "verification obligation {id} has only untrusted passing evidence"
      ),
      Self::DeterministicGateFailed => formatter.write_str("deterministic verification failed"),
      Self::AssessmentFoundImplementationGap(id) => {
        write!(formatter, "assessment found an implementation gap for {id}")
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
  pub assessment: &'a ReconcileResult,
  pub deterministic_gate_passed: bool,
  pub verified_revision: &'a str,
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
    if !context.deterministic_gate_passed {
      blockers.insert(CompletionBlocker::DeterministicGateFailed);
    }

    let policy = EvidencePolicy;
    for requirement in context
      .catalog
      .requirements
      .iter()
      .filter(|requirement| requirement.required)
    {
      if context
        .evidence
        .requirement_verification_state(&requirement.id, &policy)
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
        .criterion_verification_state(&criterion.id, &policy)
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
      let related: Vec<_> = context
        .evidence
        .evidence
        .values()
        .filter(|evidence| evidence.obligation_id == obligation.id)
        .collect();
      if related.iter().any(|evidence| policy.blocks(evidence))
        && related.iter().any(|evidence| policy.authorizes(evidence))
      {
        blockers.insert(CompletionBlocker::EvidenceContradicted(
          obligation.id.clone(),
        ));
      }
      if related.iter().any(|evidence| {
        evidence.result == EvidenceResult::Passed
          && evidence.validity.is_valid()
          && !policy.authorizes(evidence)
      }) && !related.iter().any(|evidence| policy.authorizes(evidence))
      {
        blockers.insert(CompletionBlocker::UntrustedEvidence(obligation.id.clone()));
      }
      if !related.iter().any(|evidence| policy.authorizes(evidence)) {
        for evidence in related
          .iter()
          .filter(|evidence| matches!(evidence.validity, EvidenceValidity::Stale { .. }))
        {
          blockers.insert(CompletionBlocker::EvidenceStale(evidence.id));
        }
      }
    }
    for assessment in &context.assessment.requirements {
      if !assessment.missing_implementation.is_empty() {
        blockers.insert(CompletionBlocker::AssessmentFoundImplementationGap(
          assessment.requirement_id.clone(),
        ));
      }
    }
    for work_unit in &context.assessment.work_units {
      for requirement_id in &work_unit.requirement_ids {
        blockers.insert(CompletionBlocker::AssessmentFoundImplementationGap(
          requirement_id.clone(),
        ));
      }
    }
    if context.current_revision != context.verified_revision {
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
  use proptest::prelude::*;
  use tenet_domain::{
    evidence::{AcceptanceCriterion, VerificationKind, VerificationObligation},
    ids::{CriterionId, ObligationId, VerificationRunId},
    model::{Requirement, RequirementAssessment},
    verification::{
      CommandResult, DependencyScopeAuthority, VerificationAuthority, VerificationExecutionResult,
      VerificationSpec,
    },
    worker::{derive_normative_fragments, CatalogCoverage},
  };

  use super::*;

  fn specification() -> &'static str {
    "Required behavior"
  }

  fn spec() -> VerificationSpec {
    VerificationSpec {
      program: "true".into(),
      args: Vec::new(),
      working_directory: ".".into(),
      environment: Default::default(),
    }
  }

  fn catalog() -> RequirementCatalog {
    let fragments = derive_normative_fragments(specification());
    let requirements = vec![Requirement {
      id: RequirementId::from("REQ-001"),
      title: "Required behavior".into(),
      description: "Required behavior".into(),
      required: true,
      source_refs: vec![fragments[0].reference()],
    }];
    RequirementCatalog {
      spec_hash: "spec".into(),
      coverage: CatalogCoverage::derive(specification(), &requirements),
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
        description: "Run check".into(),
        kind: VerificationKind::Command,
        required: true,
        spec: spec(),
        authority: VerificationAuthority::ProjectConfigured,
        dependency_scope: vec!["**".into()],
        dependency_scope_authority: DependencyScopeAuthority::ProjectConfigured,
      }],
    }
  }

  fn record_execution(
    graph: &mut EvidenceGraphState,
    authority: VerificationAuthority,
    passed: bool,
  ) {
    graph
      .record_execution_result(
        "abc123",
        Utc.with_ymd_and_hms(2026, 8, 16, 10, 0, 0).unwrap(),
        &VerificationExecutionResult {
          run_id: VerificationRunId::new(),
          obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
          spec: spec(),
          authority,
          result: CommandResult {
            command: spec().identity(),
            exit_code: Some(if passed { 0 } else { 1 }),
            timed_out: false,
            duration_ms: 1,
            stdout: String::new(),
            stderr: String::new(),
          },
        },
      )
      .expect("evidence");
  }

  fn graph(catalog: &RequirementCatalog, passed: bool) -> EvidenceGraphState {
    let mut graph = crate::evidence::graph_from_catalog(catalog).expect("graph");
    record_execution(&mut graph, VerificationAuthority::ProjectConfigured, passed);
    graph
  }

  fn assessment() -> ReconcileResult {
    ReconcileResult {
      summary: "No gaps".into(),
      requirements: vec![RequirementAssessment {
        requirement_id: RequirementId::from("REQ-001"),
        implementation_state: tenet_domain::evidence::ImplementationState::Present,
        observations: Vec::new(),
        missing_implementation: Vec::new(),
        missing_evidence: Vec::new(),
      }],
      work_units: Vec::new(),
    }
  }

  fn decision(catalog: &RequirementCatalog, graph: &EvidenceGraphState) -> CompletionDecision {
    CompletionPolicy.evaluate(&CompletionContext {
      catalog,
      evidence: graph,
      assessment: &assessment(),
      deterministic_gate_passed: true,
      verified_revision: "abc123",
      current_revision: "abc123",
      repository_clean: true,
      has_active_leases: false,
      has_pending_integrations: false,
    })
  }

  #[test]
  fn complete_trusted_chain_authorizes_done() {
    let catalog = catalog();
    assert_eq!(
      decision(&catalog, &graph(&catalog, true)),
      CompletionDecision::Done
    );
  }

  #[test]
  fn incomplete_coverage_blocks_even_verified_catalog() {
    let mut catalog = catalog();
    let graph = graph(&catalog, true);
    catalog.coverage.uncovered_fragment_ids =
      vec![catalog.coverage.normative_fragments[0].id.clone()];

    assert!(matches!(
      decision(&catalog, &graph),
      CompletionDecision::NotReady(blockers)
        if blockers.iter().any(|blocker| matches!(blocker, CompletionBlocker::SpecificationCoverageIncomplete(_)))
    ));
  }

  #[test]
  fn changed_head_produces_typed_blocker() {
    let catalog = catalog();
    let graph = graph(&catalog, true);
    let decision = CompletionPolicy.evaluate(&CompletionContext {
      catalog: &catalog,
      evidence: &graph,
      assessment: &assessment(),
      deterministic_gate_passed: true,
      verified_revision: "abc123",
      current_revision: "def456",
      repository_clean: true,
      has_active_leases: false,
      has_pending_integrations: false,
    });

    assert!(matches!(
      decision,
      CompletionDecision::NotReady(blockers)
        if blockers.contains(&CompletionBlocker::RepositoryChangedAfterVerification)
    ));
  }

  #[test]
  fn policy_returns_deterministic_sorted_typed_blockers() {
    let catalog = catalog();
    let graph = crate::evidence::graph_from_catalog(&catalog).expect("graph");
    let context = CompletionContext {
      catalog: &catalog,
      evidence: &graph,
      assessment: &assessment(),
      deterministic_gate_passed: false,
      verified_revision: "abc123",
      current_revision: "def456",
      repository_clean: false,
      has_active_leases: true,
      has_pending_integrations: true,
    };

    let decision = CompletionPolicy.evaluate(&context);
    assert_eq!(decision, CompletionPolicy.evaluate(&context));
    let CompletionDecision::NotReady(blockers) = decision else {
      panic!("incomplete context authorized completion");
    };
    assert!(blockers.contains(&CompletionBlocker::DeterministicGateFailed));
    assert!(blockers.contains(&CompletionBlocker::RepositoryChangedAfterVerification));
    assert!(blockers.contains(&CompletionBlocker::RepositoryDirty));
    assert!(blockers.contains(&CompletionBlocker::ActiveLease));
    assert!(blockers.contains(&CompletionBlocker::PendingIntegration));
    assert!(blockers.contains(&CompletionBlocker::RequirementUnverified(
      RequirementId::from("REQ-001")
    )));
    assert!(
      blockers.contains(&CompletionBlocker::CriterionUnverified(CriterionId::from(
        "REQ-001/AC-01"
      )))
    );
  }

  #[test]
  fn required_unverified_requirement_blocks_completion() {
    let catalog = catalog();
    let graph = crate::evidence::graph_from_catalog(&catalog).expect("graph");

    assert!(matches!(
      decision(&catalog, &graph),
      CompletionDecision::NotReady(blockers)
        if blockers.contains(&CompletionBlocker::RequirementUnverified(
          RequirementId::from("REQ-001")
        ))
    ));
  }

  #[test]
  fn mandatory_unverified_criterion_blocks_completion() {
    let catalog = catalog();
    let graph = crate::evidence::graph_from_catalog(&catalog).expect("graph");

    assert!(matches!(
      decision(&catalog, &graph),
      CompletionDecision::NotReady(blockers)
        if blockers.contains(&CompletionBlocker::CriterionUnverified(
          CriterionId::from("REQ-001/AC-01")
        ))
    ));
  }

  #[test]
  fn stale_evidence_produces_typed_blocker() {
    let catalog = catalog();
    let mut graph = graph(&catalog, true);
    graph.invalidate_where(
      "def456",
      Utc.with_ymd_and_hms(2026, 8, 16, 11, 0, 0).unwrap(),
      |_| true,
    );

    assert!(matches!(
      decision(&catalog, &graph),
      CompletionDecision::NotReady(blockers)
        if blockers.iter().any(|blocker| matches!(blocker, CompletionBlocker::EvidenceStale(_)))
    ));
  }

  #[test]
  fn contradictory_evidence_produces_typed_blocker() {
    let catalog = catalog();
    let mut graph = graph(&catalog, true);
    record_execution(&mut graph, VerificationAuthority::ProjectConfigured, false);

    assert!(matches!(
      decision(&catalog, &graph),
      CompletionDecision::NotReady(blockers)
        if blockers.contains(&CompletionBlocker::EvidenceContradicted(
          ObligationId::from("REQ-001/AC-01/VO-01")
        ))
    ));
  }

  #[test]
  fn agent_proposed_passing_evidence_is_untrusted() {
    let mut catalog = catalog();
    catalog.verification_obligations[0].authority = VerificationAuthority::AgentProposed;
    let mut graph = crate::evidence::graph_from_catalog(&catalog).expect("graph");
    record_execution(&mut graph, VerificationAuthority::AgentProposed, true);

    assert!(matches!(
      decision(&catalog, &graph),
      CompletionDecision::NotReady(blockers)
        if blockers.contains(&CompletionBlocker::UntrustedEvidence(
          ObligationId::from("REQ-001/AC-01/VO-01")
        ))
    ));
  }

  #[test]
  fn assessment_implementation_gap_blocks_completion() {
    let catalog = catalog();
    let graph = graph(&catalog, true);
    let mut assessment = assessment();
    assessment.requirements[0].missing_implementation = vec!["gap".into()];
    let decision = CompletionPolicy.evaluate(&CompletionContext {
      catalog: &catalog,
      evidence: &graph,
      assessment: &assessment,
      deterministic_gate_passed: true,
      verified_revision: "abc123",
      current_revision: "abc123",
      repository_clean: true,
      has_active_leases: false,
      has_pending_integrations: false,
    });

    assert!(matches!(
      decision,
      CompletionDecision::NotReady(blockers)
        if blockers.contains(&CompletionBlocker::AssessmentFoundImplementationGap(
          RequirementId::from("REQ-001")
        ))
    ));
  }

  proptest! {
    #[test]
    fn removing_coverage_never_makes_completion_easier(remove in any::<bool>()) {
      let mut catalog = catalog();
      let graph = graph(&catalog, true);
      let before = decision(&catalog, &graph);
      if remove {
        catalog.coverage.uncovered_fragment_ids = vec![catalog.coverage.normative_fragments[0].id.clone()];
      }
      let after = decision(&catalog, &graph);
      prop_assert!(!matches!((before, after), (CompletionDecision::NotReady(_), CompletionDecision::Done)));
    }
  }
}
