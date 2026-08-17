//! Verification authorization and obligation-bound request construction.

use tenet_domain::{
  ids::VerificationRunId, model::RequirementCatalog, verification::VerificationExecutionRequest,
};

/// Constructs authorized execution requests for every required catalog obligation.
pub fn required_requests(
  catalog: &RequirementCatalog,
  run_id: VerificationRunId,
) -> Vec<VerificationExecutionRequest> {
  catalog
    .verification_obligations
    .iter()
    .filter(|obligation| obligation.required)
    .map(|obligation| VerificationExecutionRequest {
      run_id,
      obligation_id: obligation.id.clone(),
      spec: obligation.spec.clone(),
      authority: obligation.authority,
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use tenet_domain::{
    evidence::{VerificationKind, VerificationObligation},
    ids::{CriterionId, ObligationId},
    verification::{DependencyScopeAuthority, VerificationAuthority, VerificationSpec},
    worker::CatalogCoverage,
  };

  use super::*;

  fn catalog(authority: VerificationAuthority) -> RequirementCatalog {
    RequirementCatalog {
      spec_hash: "spec".into(),
      requirements: Vec::new(),
      acceptance_criteria: Vec::new(),
      verification_obligations: vec![VerificationObligation {
        id: ObligationId::from("REQ-001/AC-01/VO-01"),
        criterion_id: CriterionId::from("REQ-001/AC-01"),
        description: "Check".into(),
        kind: VerificationKind::Command,
        required: true,
        spec: VerificationSpec {
          program: "true".into(),
          args: Vec::new(),
          working_directory: ".".into(),
          environment: Default::default(),
        },
        authority,
        dependency_scope: vec!["**".into()],
        dependency_scope_authority: DependencyScopeAuthority::ProjectConfigured,
      }],
      coverage: CatalogCoverage {
        normative_fragments: Vec::new(),
        uncovered_fragment_ids: Vec::new(),
      },
    }
  }

  #[test]
  fn request_is_explicitly_bound_to_obligation() {
    let requests = required_requests(
      &catalog(VerificationAuthority::ProjectConfigured),
      VerificationRunId::new(),
    );

    assert_eq!(
      requests[0].obligation_id,
      ObligationId::from("REQ-001/AC-01/VO-01")
    );
  }

  #[test]
  fn agent_proposed_authority_remains_advisory() {
    let requests = required_requests(
      &catalog(VerificationAuthority::AgentProposed),
      VerificationRunId::new(),
    );

    assert_eq!(requests[0].authority, VerificationAuthority::AgentProposed);
  }

  #[test]
  fn project_configured_authority_remains_trusted() {
    let requests = required_requests(
      &catalog(VerificationAuthority::ProjectConfigured),
      VerificationRunId::new(),
    );

    assert!(requests[0].authority.is_trusted());
  }
}
