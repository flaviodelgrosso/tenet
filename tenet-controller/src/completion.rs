use std::collections::BTreeSet;

use tenet_domain::{
  evidence::EvidenceGraphState,
  ids::{CriterionId, ObligationId, RequirementId, SpecFragmentId},
  model::RequirementCatalog,
  proof::{derive_proof_state, EvidenceContract, ProofState},
  verification::ProjectVerificationRun,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompletionBlocker {
  SpecificationCoverageIncomplete(Vec<SpecFragmentId>),
  RequirementUnverified(RequirementId),
  CriterionUnverified(CriterionId),
  ProofContradicted(ObligationId),
  ProofInsufficient(ObligationId),
  ProofStale(ObligationId),
  HumanAttestationRequired(ObligationId),
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
      Self::ProofContradicted(id) => write!(formatter, "authoritative proof contradicts {id}"),
      Self::ProofInsufficient(id) => {
        write!(formatter, "authoritative proof is insufficient for {id}")
      }
      Self::ProofStale(id) => write!(formatter, "authoritative proof is stale for {id}"),
      Self::HumanAttestationRequired(id) => {
        write!(
          formatter,
          "obligation {id} requires explicit authenticated human attestation"
        )
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

    let obligation_proven = |obligation_id: &ObligationId| {
      context
        .evidence
        .proof_derivations
        .get(obligation_id)
        .is_some_and(|derivation| {
          derivation.revision == context.current_revision && derivation.state == ProofState::Proven
        })
    };
    for requirement in context
      .catalog
      .requirements
      .iter()
      .filter(|requirement| requirement.required)
    {
      let proven = context
        .catalog
        .acceptance_criteria
        .iter()
        .filter(|criterion| criterion.requirement_id == requirement.id && criterion.mandatory)
        .flat_map(|criterion| {
          context
            .catalog
            .verification_obligations
            .iter()
            .filter(move |obligation| {
              obligation.criterion_id == criterion.id && obligation.required
            })
        })
        .all(|obligation| obligation_proven(&obligation.id));
      if !proven {
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
      let proven = context
        .catalog
        .verification_obligations
        .iter()
        .filter(|obligation| obligation.criterion_id == criterion.id && obligation.required)
        .all(|obligation| obligation_proven(&obligation.id));
      if !proven {
        blockers.insert(CompletionBlocker::CriterionUnverified(criterion.id.clone()));
      }
    }
    for obligation in context
      .catalog
      .verification_obligations
      .iter()
      .filter(|obligation| obligation.required)
    {
      match context.evidence.proof_derivations.get(&obligation.id) {
        Some(derivation) if derivation.revision == context.current_revision => {
          match derivation.state {
            ProofState::Proven => {}
            ProofState::Contradicted => {
              blockers.insert(CompletionBlocker::ProofContradicted(obligation.id.clone()));
            }
            ProofState::Insufficient => {
              blockers.insert(CompletionBlocker::ProofInsufficient(obligation.id.clone()));
              if human_requirement(
                &obligation.evidence_contract,
                &obligation.id,
                context.evidence,
                context.current_revision,
              ) == HumanRequirement::Required
              {
                blockers.insert(CompletionBlocker::HumanAttestationRequired(
                  obligation.id.clone(),
                ));
              }
            }
            ProofState::Stale => {
              blockers.insert(CompletionBlocker::ProofStale(obligation.id.clone()));
            }
          }
        }
        _ => {
          blockers.insert(CompletionBlocker::ProofInsufficient(obligation.id.clone()));
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum HumanRequirement {
  Satisfied,
  CanProceedWithoutHuman,
  Required,
  Impossible,
}

fn human_requirement(
  contract: &EvidenceContract,
  obligation_id: &ObligationId,
  evidence: &EvidenceGraphState,
  revision: &str,
) -> HumanRequirement {
  let state = derive_proof_state(
    obligation_id,
    contract,
    evidence.artifacts.values(),
    revision,
  )
  .state;
  if state == ProofState::Proven {
    return HumanRequirement::Satisfied;
  }
  if state == ProofState::Contradicted {
    return HumanRequirement::Impossible;
  }
  match contract {
    EvidenceContract::HumanAttestation { .. } => HumanRequirement::Required,
    EvidenceContract::Artifact { .. } => HumanRequirement::CanProceedWithoutHuman,
    EvidenceContract::All { requirements } => {
      let (all_satisfied, impossible, required) = requirements.iter().fold(
        (true, false, false),
        |(all_satisfied, impossible, required), requirement| match human_requirement(
          requirement,
          obligation_id,
          evidence,
          revision,
        ) {
          HumanRequirement::Satisfied => (all_satisfied, impossible, required),
          HumanRequirement::CanProceedWithoutHuman => (false, impossible, required),
          HumanRequirement::Required => (false, impossible, true),
          HumanRequirement::Impossible => (false, true, required),
        },
      );
      if impossible {
        HumanRequirement::Impossible
      } else if required {
        HumanRequirement::Required
      } else if all_satisfied {
        HumanRequirement::Satisfied
      } else {
        HumanRequirement::CanProceedWithoutHuman
      }
    }
    EvidenceContract::Any { requirements } => {
      let (satisfied, can_proceed, required) = requirements.iter().fold(
        (false, false, false),
        |(satisfied, can_proceed, required), requirement| match human_requirement(
          requirement,
          obligation_id,
          evidence,
          revision,
        ) {
          HumanRequirement::Satisfied => (true, can_proceed, required),
          HumanRequirement::CanProceedWithoutHuman => (satisfied, true, required),
          HumanRequirement::Required => (satisfied, can_proceed, true),
          HumanRequirement::Impossible => (satisfied, can_proceed, required),
        },
      );
      if satisfied {
        HumanRequirement::Satisfied
      } else if can_proceed {
        HumanRequirement::CanProceedWithoutHuman
      } else if required {
        HumanRequirement::Required
      } else {
        HumanRequirement::Impossible
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use chrono::{TimeZone, Utc};
  use tenet_domain::{
    evidence::{AcceptanceCriterion, VerificationObligation},
    ids::{CriterionId, ObligationId, RequirementId, VerificationRunId},
    model::Requirement,
    proof::{AssessmentJudgment, EvidenceContract, EvidencePredicate},
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
        evidence_contract: EvidenceContract::Artifact {
          predicate: EvidencePredicate::NamedProjectCheck {
            name: "quality".into(),
          },
        },
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

  fn decision_with_contract(
    project_passed: bool,
    _outcome: Option<AssessmentJudgment>,
    contract: EvidenceContract,
  ) -> CompletionDecision {
    let mut catalog = catalog();
    catalog.verification_obligations[0].evidence_contract = contract;
    let project = project(project_passed);
    let mut graph = crate::evidence::graph_from_catalog(&catalog).expect("graph");
    graph.record_project_verification(&project);
    graph.record_project_artifacts(&project).expect("artifacts");
    graph.derive_proofs("abc");
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

  fn decision(project_passed: bool, outcome: Option<AssessmentJudgment>) -> CompletionDecision {
    decision_with_contract(
      project_passed,
      outcome,
      EvidenceContract::Artifact {
        predicate: EvidencePredicate::NamedProjectCheck {
          name: "quality".into(),
        },
      },
    )
  }

  #[test]
  fn mechanical_project_proof_needs_no_assessor() {
    assert_eq!(decision(true, None), CompletionDecision::Done);
  }

  #[test]
  fn project_pass_and_semantic_satisfaction_yield_done() {
    assert_eq!(
      decision(
        true,
        Some(AssessmentJudgment::Supported {
          artifact_ids: Vec::new(),
          rationale: "Satisfied".into(),
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
        Some(AssessmentJudgment::Supported {
          artifact_ids: Vec::new(),
          rationale: "Satisfied".into(),
        })
      ),
      CompletionDecision::NotReady(blockers)
        if blockers.contains(&CompletionBlocker::ProjectVerificationFailed)
    ));
  }

  #[test]
  fn unconfirmed_model_suspicion_does_not_create_contradiction() {
    assert_eq!(
      decision(
        true,
        Some(AssessmentJudgment::Contradicted {
          artifact_ids: Vec::new(),
          rationale: "Suspected behavior".into(),
          proposals: Vec::new(),
        })
      ),
      CompletionDecision::Done
    );
  }

  #[test]
  fn semantic_support_cannot_satisfy_trusted_verifier_contract() {
    let decision = decision_with_contract(
      true,
      Some(AssessmentJudgment::Supported {
        artifact_ids: Vec::new(),
        rationale: "Model says supported".into(),
      }),
      EvidenceContract::Artifact {
        predicate: EvidencePredicate::TrustedVerifierCheck {
          name: "expiry-boundary".into(),
        },
      },
    );
    assert!(matches!(
      decision,
      CompletionDecision::NotReady(blockers)
        if blockers.contains(&CompletionBlocker::ProofInsufficient(
          ObligationId::from("REQ-001/AC-01/VO-01")
        ))
    ));
  }

  #[test]
  fn missing_human_attestation_blocks_completion() {
    assert!(matches!(
      decision_with_contract(
        true,
        None,
        EvidenceContract::HumanAttestation {
          statement: "Product approval".into()
        }
      ),
      CompletionDecision::NotReady(_)
    ));
  }
  #[test]
  fn confirmed_authoritative_counterexample_blocks_completion() {
    let mut catalog = catalog();
    catalog.verification_obligations[0].evidence_contract = EvidenceContract::Artifact {
      predicate: EvidencePredicate::NamedProjectCheck {
        name: "quality".into(),
      },
    };
    let mut project = project(true);
    project.checks[0].result.exit_code = Some(1);
    let mut graph = crate::evidence::graph_from_catalog(&catalog).expect("graph");
    graph.record_project_verification(&project);
    graph.record_project_artifacts(&project).expect("artifacts");
    graph.derive_proofs("abc");
    let decision = CompletionPolicy.evaluate(&CompletionContext {
      catalog: &catalog,
      evidence: &graph,
      project_verification: &project,

      current_suite_hash: "suite",
      current_revision: "abc",
      repository_clean: true,
      has_active_leases: false,
      has_pending_integrations: false,
    });
    assert!(matches!(
      decision,
      CompletionDecision::NotReady(blockers)
        if blockers.contains(&CompletionBlocker::ProofContradicted(
          ObligationId::from("REQ-001/AC-01/VO-01")
        ))
    ));
  }
  #[test]
  fn unresolved_machine_alternative_does_not_require_human_attestation() {
    let decision = decision_with_contract(
      true,
      None,
      EvidenceContract::Any {
        requirements: vec![
          EvidenceContract::HumanAttestation {
            statement: "Manual review".into(),
          },
          EvidenceContract::Artifact {
            predicate: EvidencePredicate::TrustedVerifierCheck {
              name: "machine".into(),
            },
          },
        ],
      },
    );
    let CompletionDecision::NotReady(blockers) = decision else {
      panic!("missing machine evidence must block");
    };
    assert!(!blockers
      .iter()
      .any(|blocker| matches!(blocker, CompletionBlocker::HumanAttestationRequired(_))));
  }
}
