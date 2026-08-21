#![allow(dead_code)]

use tenet_domain::{
  evidence::{AcceptanceCriterion, EvidenceGraphState, VerificationObligation},
  ids::{CriterionId, ObligationId, RequirementId, SpecFragmentId},
  model::{Requirement, RequirementCatalog},
  worker::{CatalogCoverage, SpecFragment, SpecReference},
};

pub fn catalog() -> RequirementCatalog {
  let fragment = SpecFragment {
    id: SpecFragmentId::from("SPEC-0001-abcdef"),
    section: Some("Requirements".into()),
    text: "Tenet must persist state.".into(),
    text_hash: "abcdef".into(),
  };
  RequirementCatalog {
    spec_hash: "spec-hash".into(),
    requirements: vec![Requirement {
      id: RequirementId::from("REQ-001"),
      title: "Persist state".into(),
      description: "Persist controller state".into(),
      required: true,
      source_refs: vec![SpecReference {
        section: fragment.section.clone(),
        fragment_id: fragment.id.clone(),
        text_hash: fragment.text_hash.clone(),
      }],
    }],
    acceptance_criteria: vec![AcceptanceCriterion {
      id: CriterionId::from("REQ-001/AC-01"),
      requirement_id: RequirementId::from("REQ-001"),
      description: "State survives restart".into(),
      mandatory: true,
    }],
    verification_obligations: vec![VerificationObligation {
      id: ObligationId::from("REQ-001/AC-01/VO-01"),
      criterion_id: CriterionId::from("REQ-001/AC-01"),
      description: "Round trip is equal".into(),
      required: true,
    }],
    coverage: CatalogCoverage {
      normative_fragments: vec![fragment],
      uncovered_fragment_ids: Vec::new(),
    },
  }
}

pub fn empty_graph(catalog: &RequirementCatalog) -> EvidenceGraphState {
  let mut graph = EvidenceGraphState::new(&catalog.spec_hash);
  for requirement in &catalog.requirements {
    graph.register_requirement(requirement.id.clone(), requirement.required);
  }
  for criterion in &catalog.acceptance_criteria {
    graph.add_criterion(criterion.clone()).expect("criterion");
  }
  for obligation in &catalog.verification_obligations {
    graph
      .add_obligation(obligation.clone())
      .expect("obligation");
  }
  graph
}

pub fn reconciliation() -> tenet_domain::model::ReconcileResult {
  use tenet_domain::{
    evidence::ImplementationState,
    model::{CandidateCheck, ReconcileResult, RequirementAssessment, WorkScope, WorkUnit},
  };

  ReconcileResult {
    summary: "Work remains".into(),
    requirements: vec![RequirementAssessment {
      requirement_id: RequirementId::from("REQ-001"),
      implementation_state: ImplementationState::Partial,
      observations: vec!["Storage exists".into()],
      missing_implementation: vec!["Controller cutover".into()],
      missing_evidence: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
    }],
    work_units: vec![WorkUnit {
      id: "WU-001".into(),
      title: "Persist".into(),
      objective: "Persist state".into(),
      requirement_ids: vec![RequirementId::from("REQ-001")],
      criterion_ids: vec![CriterionId::from("REQ-001/AC-01")],
      verification_obligation_ids: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
      suggested_checks: vec![CandidateCheck {
        obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
        command: "cargo test".into(),
      }],
      depends_on: Vec::new(),
      scope: WorkScope {
        paths: vec!["tenet-storage/**".into()],
      },
    }],
  }
}
