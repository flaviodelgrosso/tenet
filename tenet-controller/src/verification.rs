//! Verification authorization and obligation-bound request construction.

use anyhow::{bail, Result};
use tenet_domain::{
  ids::VerificationRunId, model::RequirementCatalog, verification::VerificationExecutionRequest,
};

/// Constructs obligation-bound requests only when every required check has trusted authority.
pub fn required_requests(
  catalog: &RequirementCatalog,
  run_id: VerificationRunId,
) -> Result<Vec<VerificationExecutionRequest>> {
  let untrusted = catalog
    .verification_obligations
    .iter()
    .filter(|obligation| obligation.required && !obligation.authority.is_trusted())
    .map(|obligation| obligation.id.to_string())
    .collect::<Vec<_>>();
  if !untrusted.is_empty() {
    bail!(
      "required verification obligations lack trusted execution authority: {}",
      untrusted.join(", ")
    );
  }

  Ok(
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
      .collect(),
  )
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
    )
    .expect("trusted request");

    assert_eq!(
      requests[0].obligation_id,
      ObligationId::from("REQ-001/AC-01/VO-01")
    );
  }

  #[test]
  fn agent_proposed_obligation_is_not_executed_as_final_verification() {
    let error = required_requests(
      &catalog(VerificationAuthority::AgentProposed),
      VerificationRunId::new(),
    )
    .expect_err("agent-proposed command must remain advisory");

    assert!(error.to_string().contains("REQ-001/AC-01/VO-01"));
  }

  #[test]
  fn project_configured_authority_remains_trusted() {
    let requests = required_requests(
      &catalog(VerificationAuthority::ProjectConfigured),
      VerificationRunId::new(),
    )
    .expect("trusted request");

    assert!(requests[0].authority.is_trusted());
  }
}
